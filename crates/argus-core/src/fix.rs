//! Fix planning. A plan is derived purely from a [`RepoStatus`], so it is
//! cheap, deterministic and testable. Applying is one fix per user action.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ops::{self, Op, OpOutcome};
use crate::status::{self, BranchInfo, Head, RepoStatus, StatusOptions, Tracking};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum FixKind {
    /// Stash staged, unstaged and untracked changes (not ignored files).
    Stash,
    /// Commit everything (never automatic: needs a message).
    Commit,
    /// Push commits to an existing upstream (fast-forward only).
    Push,
    /// Push a branch that has no upstream yet.
    Publish,
    /// Pull the current branch (your pull settings apply).
    Pull,
    /// Fast-forward local branches that aren't checked out.
    FastForward,
    /// Leave a fully pushed / merged branch for the default branch.
    SwitchDefault,
}

impl FixKind {
    /// Default priority: clean up uncommitted work, then share commits,
    /// then catch up, then leave the branch.
    pub const ALL: [FixKind; 7] = [
        FixKind::Stash,
        FixKind::Commit,
        FixKind::Push,
        FixKind::Publish,
        FixKind::Pull,
        FixKind::FastForward,
        FixKind::SwitchDefault,
    ];

    /// Requires a message from the user.
    pub fn needs_input(self) -> bool {
        matches!(self, FixKind::Commit)
    }

    /// The name used in config and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            FixKind::SwitchDefault => "switch-default",
            FixKind::Push => "push",
            FixKind::Publish => "publish",
            FixKind::FastForward => "fast-forward",
            FixKind::Pull => "pull",
            FixKind::Stash => "stash",
            FixKind::Commit => "commit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    pub op: Op,
    pub reason: String,
}

/// Every fix applicable to the repo in its current state.
///
/// Nothing is proposed while a merge/rebase is in progress or conflicts exist:
/// that needs a human.
pub fn plan(s: &RepoStatus) -> Vec<Fix> {
    let mut v = vec![];
    if s.bare || s.in_progress.is_some() || s.changes.conflicted > 0 {
        return v;
    }
    let tracked_dirty = s.changes.staged + s.changes.unstaged > 0;
    let any_dirty = tracked_dirty || s.changes.untracked > 0;
    let head_branch = match &s.head {
        Head::Branch(b) => Some(b.as_str()),
        _ => None,
    };
    let head_info = head_branch.and_then(|b| s.branches.iter().find(|bi| bi.name == b));

    if let (Some(b), Some(default)) = (head_branch, &s.default_branch) {
        // Switching never loses commits, but it hides them; only do it when
        // the work is merged or exists on a remote.
        let safe = head_info.is_some_and(|h| !h.is_unpushed());
        if b != default && !tracked_dirty && safe {
            v.push(Fix {
                kind: FixKind::SwitchDefault,
                op: Op::Switch {
                    branch: default.clone(),
                },
                reason: format!("{b} has nothing unpushed"),
            });
        }
    }

    if let Some(t) = &s.upstream {
        let same_name = head_info.is_some_and(|h| h.upstream_is_same_name());
        if !t.gone && t.ahead > 0 && t.behind == 0 && same_name {
            v.push(Fix {
                kind: FixKind::Push,
                op: Op::Push,
                reason: format!("{} commit(s) ahead of {}", t.ahead, t.name),
            });
        }
        if !t.gone && t.behind > 0 {
            v.push(Fix {
                kind: FixKind::Pull,
                op: Op::Pull,
                reason: format!("{} commit(s) behind {}", t.behind, t.name),
            });
        }
    } else if let (Some(b), Some(h), Some(remote)) = (head_branch, head_info, &s.remote)
        && h.is_unpushed()
    {
        v.push(Fix {
            kind: FixKind::Publish,
            op: Op::PushSetUpstream {
                remote: remote.clone(),
                branch: b.to_string(),
            },
            reason: format!("{b} has no upstream"),
        });
    }

    for b in s.branches.iter().filter(|b| !b.is_head) {
        if let Some(t) = &b.upstream
            && !t.gone
            && t.ahead == 0
            && t.behind > 0
        {
            v.push(Fix {
                kind: FixKind::FastForward,
                op: Op::FastForward {
                    branch: b.name.clone(),
                    upstream: t.name.clone(),
                },
                reason: format!("{} is {} behind {}", b.name, t.behind, t.name),
            });
        }
    }

    if any_dirty {
        v.push(Fix {
            kind: FixKind::Stash,
            op: Op::Stash {
                message: format!(
                    "argus {}",
                    jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M")
                ),
            },
            reason: format!("{} uncommitted path(s)", s.changes.total()),
        });
        v.push(Fix {
            kind: FixKind::Commit,
            op: Op::CommitAll {
                message: String::new(),
            },
            reason: format!("{} uncommitted path(s)", s.changes.total()),
        });
    }
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub fix: Fix,
    pub outcome: OpOutcome,
}

/// Every applicable fix in priority order: kinds in `priority` in the order
/// given, then the rest in [`FixKind::ALL`] order.
pub fn ranked(s: &RepoStatus, priority: &[FixKind]) -> Vec<Fix> {
    let mut v = plan(s);
    v.sort_by_key(|f| {
        priority
            .iter()
            .position(|&k| k == f.kind)
            .unwrap_or(priority.len() + FixKind::ALL.iter().position(|&k| k == f.kind).unwrap())
    });
    v
}

/// The fix an auto-fix action applies: the highest-priority one in `allowed`
/// that needs no input.
pub fn choose(s: &RepoStatus, allowed: &[FixKind]) -> Option<Fix> {
    ranked(s, allowed)
        .into_iter()
        .find(|f| allowed.contains(&f.kind) && !f.kind.needs_input())
}

/// The fixes successive auto-fix actions would apply, starting with
/// [`choose`]: predicted with [`simulate`], assuming each one succeeds.
pub fn chain(s: &RepoStatus, allowed: &[FixKind]) -> Vec<Fix> {
    let mut out: Vec<Fix> = vec![];
    let mut cur = s.clone();
    while let Some(f) = choose(&cur, allowed) {
        // a repeat would mean the simulation made no progress
        if out.iter().any(|g| g.kind == f.kind && g.op == f.op)
            || out.len() > 2 * FixKind::ALL.len()
        {
            break;
        }
        cur = simulate(&cur, &f);
        out.push(f);
    }
    out
}

/// What auto-fix would do, for showing before doing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// The fix the next action applies, then the ones later actions would
    /// (see [`chain`]).
    pub chain: Vec<Fix>,
    /// Other fixes available now that the chain doesn't include, in
    /// priority order; possibly not allowed. Never ones needing input.
    pub alternatives: Vec<Fix>,
}

pub fn preview(s: &RepoStatus, allowed: &[FixKind]) -> Preview {
    let chain = chain(s, allowed);
    // compare by label: a stash's message carries a timestamp
    let in_chain = |f: &Fix| {
        chain
            .iter()
            .any(|c| c.kind == f.kind && c.op.label() == f.op.label())
    };
    let alternatives = ranked(s, allowed)
        .into_iter()
        .filter(|f| !f.kind.needs_input() && !in_chain(f))
        .collect();
    Preview {
        chain,
        alternatives,
    }
}

/// The status expected after `f` succeeds, for previews. Pull is assumed to
/// integrate cleanly; nothing is read from disk.
pub fn simulate(s: &RepoStatus, f: &Fix) -> RepoStatus {
    let mut s = s.clone();
    let head = s.head.branch().map(str::to_string);
    match &f.op {
        Op::Stash { .. } => {
            s.changes = Default::default();
            s.stashes += 1;
        }
        Op::CommitAll { .. } => {
            s.changes = Default::default();
            if let Some(t) = &mut s.upstream {
                t.ahead += 1;
            }
            if let Some(b) = head_info(&mut s, head.as_deref()) {
                b.merged = false;
                b.unpushed = Some(b.unpushed.unwrap_or(0) + 1);
                if let Some(t) = &mut b.upstream {
                    t.ahead += 1;
                }
            }
        }
        Op::Push => {
            if let Some(t) = &mut s.upstream {
                t.ahead = 0;
            }
            if let Some(b) = head_info(&mut s, head.as_deref()) {
                b.unpushed = Some(0);
                if let Some(t) = &mut b.upstream {
                    t.ahead = 0;
                }
            }
        }
        Op::PushSetUpstream { remote, branch } => {
            let t = Tracking {
                name: format!("{remote}/{branch}"),
                ahead: 0,
                behind: 0,
                gone: false,
            };
            s.upstream = Some(t.clone());
            if let Some(b) = head_info(&mut s, head.as_deref()) {
                b.unpushed = Some(0);
                b.upstream = Some(t);
            }
        }
        Op::Pull => {
            if let Some(t) = &mut s.upstream {
                t.behind = 0;
            }
            if let Some(b) = head_info(&mut s, head.as_deref())
                && let Some(t) = &mut b.upstream
            {
                t.behind = 0;
            }
        }
        Op::FastForward { branch, .. } => {
            if let Some(b) = s.branches.iter_mut().find(|b| &b.name == branch)
                && let Some(t) = &mut b.upstream
            {
                t.behind = 0;
            }
        }
        Op::Switch { branch } => {
            if !s.branches.iter().any(|b| &b.name == branch) {
                // `git switch` creates it from the remote branch
                s.branches.push(BranchInfo {
                    name: branch.clone(),
                    upstream: s.remote.as_ref().map(|r| Tracking {
                        name: format!("{r}/{branch}"),
                        ahead: 0,
                        behind: 0,
                        gone: false,
                    }),
                    is_head: false,
                    merged: true,
                    unpushed: Some(0),
                });
            }
            for b in &mut s.branches {
                b.is_head = &b.name == branch;
            }
            s.upstream = s
                .branches
                .iter()
                .find(|b| b.is_head)
                .and_then(|b| b.upstream.clone());
            s.head = Head::Branch(branch.clone());
        }
        Op::Fetch | Op::Clone { .. } => {}
    }
    s
}

fn head_info<'a>(s: &'a mut RepoStatus, head: Option<&str>) -> Option<&'a mut BranchInfo> {
    s.branches
        .iter_mut()
        .find(|b| Some(b.name.as_str()) == head)
}

/// Apply the [`choose`]n fix for the repo's current state, if any.
///
/// Deliberately one fix per call: each auto-fix is a separate user action
/// (e.g. push, then a later action switches to the default branch).
pub fn apply_fix(path: &Path, allowed: &[FixKind]) -> Result<Option<Applied>, status::StatusError> {
    let s = status::status(path, StatusOptions { submodules: false })?;
    let Some(fix) = choose(&s, allowed) else {
        return Ok(None);
    };
    let outcome = ops::run(path, &fix.op);
    Ok(Some(Applied { fix, outcome }))
}
