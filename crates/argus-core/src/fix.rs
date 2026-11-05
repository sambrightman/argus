//! Fix planning. A plan is derived purely from a [`RepoStatus`], so it is
//! cheap, deterministic and testable. Applying is one fix per user action.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ops::{self, Op, OpOutcome};
use crate::status::{self, Head, RepoStatus, StatusOptions};

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
    /// Leave a fully pushed / merged branch for the default branch.
    SwitchDefault,
    /// Push commits to an existing upstream (fast-forward only).
    Push,
    /// Push a branch that has no upstream yet.
    Publish,
    /// Fast-forward local branches that aren't checked out.
    FastForward,
    /// Pull the current branch (your pull settings apply).
    Pull,
    /// Stash all changes, including untracked files.
    Stash,
    /// Commit everything (never automatic: needs a message).
    Commit,
}

impl FixKind {
    pub const ALL: [FixKind; 7] = [
        FixKind::SwitchDefault,
        FixKind::Push,
        FixKind::Publish,
        FixKind::FastForward,
        FixKind::Pull,
        FixKind::Stash,
        FixKind::Commit,
    ];

    /// Requires a message from the user.
    pub fn needs_input(self) -> bool {
        matches!(self, FixKind::Commit)
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

/// Apply the first allowed fix for the repo's current state, if any.
///
/// Deliberately one fix per call: each auto-fix is a separate user action
/// (e.g. push, then a later action switches to the default branch).
pub fn apply_fix(path: &Path, allowed: &[FixKind]) -> Result<Option<Applied>, status::StatusError> {
    let s = status::status(path, StatusOptions { submodules: false })?;
    let Some(fix) = plan(&s)
        .into_iter()
        .find(|f| allowed.contains(&f.kind) && !f.kind.needs_input())
    else {
        return Ok(None);
    };
    let outcome = ops::run(path, &fix.op);
    Ok(Some(Applied { fix, outcome }))
}
