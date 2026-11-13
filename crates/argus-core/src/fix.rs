//! Fix planning. A plan is derived purely from a [`RepoStatus`], so it is
//! cheap, deterministic and testable. Applying is one fix per user action.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::StashConfig;
use crate::ops::{self, Op, OpOutcome};
use crate::status::{
    self, BranchInfo, Changes, Head, Integration, PathKind, PullBy, RepoStatus, StatusOptions,
    Tracking,
};

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
    /// End the merge, rebase, cherry-pick, revert, am or bisect in
    /// progress, losing its progress (never automatic: confirmed first).
    Abort,
    /// Stash staged, unstaged and untracked changes (not ignored files).
    Stash,
    /// Commit everything (never automatic: needs a message).
    Commit,
    /// Add the untracked files to a gitignore (never automatic: which
    /// file, and anchored or not, are asked).
    Gitignore,
    /// Name the commits a detached HEAD has on no branch: create a branch
    /// there (never automatic: needs a name).
    Branch,
    /// Push commits to the current branch's remote branch (fast-forward
    /// only): its upstream when that's the same-named branch, else the
    /// same-named branch on its push remote, never a differently named
    /// upstream such as the `main` a feature branch pulls from.
    Push,
    /// Put the current branch on the remote: push and set the upstream
    /// when it has none or its upstream is gone; push to its own name when
    /// it pulls from another branch.
    Publish,
    /// Add a remote to a repo with none (never automatic: needs a URL).
    AddRemote,
    /// Push other local branches to their remote branches, as `push`
    /// would (existing ones only, fast-forward only).
    PushBranches,
    /// Leave a fully pushed / merged branch for the default branch.
    SwitchDefault,
    /// Switch a detached HEAD to the branch at the same commit.
    Reattach,
    /// Pull the current branch (your pull settings apply).
    Pull,
    /// Fast-forward local branches that aren't checked out.
    FastForward,
    /// Delete branches merged into the default branch whose upstream is
    /// gone or that have none (never automatic: confirmed first).
    DeleteBranches,
    /// Pop the newest stash into a work tree with no tracked changes, if it
    /// applies without conflicts.
    StashPop,
    /// Check submodules at another commit than recorded out at the
    /// recorded one, when their commit is on a branch (or other ref) and
    /// they have no changes of their own: nothing is lost.
    SubmoduleUpdate,
    /// Delete the records of linked worktrees whose folder is gone (not
    /// locked ones): nothing on disk is lost.
    PruneWorktrees,
    /// Remove a finished linked worktree (`git worktree remove`): its
    /// folder goes, ignored files in it too; the branch stays (never
    /// automatic: confirmed first).
    RemoveWorktree,
}

impl FixKind {
    /// Default priority: clean up uncommitted work, share commits, leave
    /// the branch, then catch up (on the branch you end up on).
    pub const ALL: [FixKind; 18] = [
        FixKind::Abort,
        FixKind::Stash,
        FixKind::Commit,
        FixKind::SubmoduleUpdate,
        FixKind::Gitignore,
        FixKind::Branch,
        FixKind::Push,
        FixKind::Publish,
        FixKind::AddRemote,
        FixKind::PushBranches,
        FixKind::SwitchDefault,
        FixKind::Reattach,
        FixKind::Pull,
        FixKind::FastForward,
        FixKind::DeleteBranches,
        FixKind::PruneWorktrees,
        FixKind::RemoveWorktree,
        FixKind::StashPop,
    ];

    /// Requires a message, name or URL from the user.
    pub fn needs_input(self) -> bool {
        matches!(
            self,
            FixKind::Commit | FixKind::Gitignore | FixKind::Branch | FixKind::AddRemote
        )
    }

    /// Permanently destructive: asked about first, never automatic.
    pub fn needs_confirmation(self) -> bool {
        matches!(
            self,
            FixKind::Abort | FixKind::DeleteBranches | FixKind::RemoveWorktree
        )
    }

    /// Something a fix of this kind can't do alone: never automatic.
    pub fn manual(self) -> bool {
        self.needs_input() || self.needs_confirmation()
    }

    /// The name used in config and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            FixKind::SwitchDefault => "switch-default",
            FixKind::Push => "push",
            FixKind::Publish => "publish",
            FixKind::PushBranches => "push-branches",
            FixKind::Reattach => "reattach",
            FixKind::FastForward => "fast-forward",
            FixKind::Pull => "pull",
            FixKind::Stash => "stash",
            FixKind::Commit => "commit",
            FixKind::Branch => "branch",
            FixKind::AddRemote => "add-remote",
            FixKind::Abort => "abort",
            FixKind::DeleteBranches => "delete-branches",
            FixKind::StashPop => "stash-pop",
            FixKind::Gitignore => "gitignore",
            FixKind::SubmoduleUpdate => "submodule-update",
            FixKind::PruneWorktrees => "prune-worktrees",
            FixKind::RemoveWorktree => "remove-worktree",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    pub op: Op,
    pub reason: String,
    /// Why this can't be applied automatically (shown, never chosen).
    pub blocked: Option<String>,
}

/// Every fix applicable to the repo in its current state.
///
/// Nothing is proposed while a merge/rebase is in progress or conflicts exist:
/// that needs a human.
/// `stash` gives the stash fix's options (the `[stash]` defaults).
/// Fixes that would undo a state `[ignore]` says is normal: switching to
/// the default branch for an ignored non-default-branch branch,
/// reattaching an ignored detached HEAD, publishing for an ignored missing
/// or gone upstream, pulling for ignored "behind", popping when any stash
/// is ignored. Auto-fix leaves them out; choosing one explicitly still
/// works.
pub fn turned_off_by(ignored: &[crate::attention::Issue]) -> Vec<FixKind> {
    use crate::attention::Issue;
    let mut v = vec![];
    for i in ignored {
        v.push(match i {
            Issue::NonDefaultBranch => FixKind::SwitchDefault,
            Issue::Detached => FixKind::Reattach,
            Issue::NoUpstream | Issue::UpstreamGone => FixKind::Publish,
            Issue::Behind(_) => FixKind::Pull,
            Issue::Stashes(_) => FixKind::StashPop,
            _ => continue,
        });
    }
    v
}

pub fn plan(s: &RepoStatus, stash: &StashConfig) -> Vec<Fix> {
    let mut v = vec![];
    if !s.stale_worktrees.is_empty() {
        v.push(Fix {
            blocked: None,
            kind: FixKind::PruneWorktrees,
            op: Op::PruneWorktrees,
            reason: format!(
                "folder gone: {}",
                s.stale_worktrees
                    .iter()
                    .map(|p| p.file_name().map_or_else(
                        || p.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    if s.bare {
        return v;
    }
    if let Some(p) = s.in_progress {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Abort,
            op: Op::Abort { what: p },
            reason: format!("{} in progress", p.label()),
        });
    }
    if s.in_progress.is_some() || s.changes.conflicted > 0 {
        return v;
    }
    let tracked_dirty = s.changes.staged + s.changes.unstaged > 0;
    let any_dirty = s.changes.uncommitted() > 0;
    let head_branch = match &s.head {
        Head::Branch(b) => Some(b.as_str()),
        _ => None,
    };
    let head_info = head_branch.and_then(|b| s.branches.iter().find(|bi| bi.name == b));

    if let (Some(b), Some(default)) = (head_branch, &s.default_branch) {
        // Switching never loses commits, but it hides them; only do it when
        // the work is merged (if only locally, switching shows it) or exists
        // on a remote.
        let safe = head_info.is_some_and(|h| h.merged || !h.is_unpushed());
        if s.is_finished_worktree() {
            v.push(Fix {
                blocked: if s.locked {
                    Some("it's locked (git worktree lock)".into())
                } else if !s.submodules.is_empty() {
                    Some("it has submodules, which git worktree remove refuses".into())
                } else {
                    None
                },
                kind: FixKind::RemoveWorktree,
                op: Op::RemoveWorktree,
                reason: format!("{b} is merged into {default} and its upstream is gone"),
            });
        } else if b != default && !tracked_dirty && safe {
            v.push(Fix {
                blocked: None,
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
        let own = head_info.and_then(|h| h.own.as_ref());
        if own.is_none() && !t.gone && t.ahead > 0 && t.behind == 0 && same_name {
            v.push(Fix {
                blocked: None,
                kind: FixKind::Push,
                op: Op::Push,
                reason: format!("{} commit(s) ahead of {}", t.ahead, t.name),
            });
        }
        if let (Some(b), Some(h)) = (head_branch, head_info) {
            if own.is_none()
                && t.gone
                && h.is_unpushed()
                && let Some(remote) = remote_of(t, b)
            {
                v.push(Fix {
                    blocked: None,
                    kind: FixKind::Publish,
                    op: Op::PushSetUpstream {
                        remote: remote.to_string(),
                        branch: b.to_string(),
                    },
                    reason: format!("{b}'s upstream {} is gone", t.name),
                });
            }
            if let Some(o) = own
                && let Some(remote) = remote_of(o, b)
            {
                let op = Op::PushTo {
                    remote: remote.to_string(),
                    branch: b.to_string(),
                };
                if o.gone && h.is_unpushed() {
                    v.push(Fix {
                        blocked: None,
                        kind: FixKind::Publish,
                        op,
                        reason: format!("{b} isn't on {remote} yet (it pulls from {})", t.name),
                    });
                } else if !o.gone && o.ahead > 0 {
                    v.push(Fix {
                        blocked: diverged_from(o),
                        kind: FixKind::Push,
                        op,
                        reason: format!(
                            "{} commit(s) ahead of {} (it pulls from {})",
                            o.ahead, o.name, t.name
                        ),
                    });
                }
            }
        }
        if !t.gone && t.behind > 0 {
            let diverged = t.ahead > 0;
            let mut reason = if diverged {
                format!(
                    "diverged from {}: {} ahead, {} behind",
                    t.name, t.ahead, t.behind
                )
            } else {
                format!("{} commit(s) behind {}", t.behind, t.name)
            };
            // Don't rely on autostash: its re-apply can itself conflict.
            let blocked = if tracked_dirty {
                Some("uncommitted changes".to_string())
            } else if diverged {
                match &s.integration {
                    Some(Integration::Clean(PullBy::Merge)) => {
                        reason.push_str("; merges cleanly, adding a merge commit");
                        None
                    }
                    Some(Integration::Clean(PullBy::Rebase)) => {
                        reason.push_str("; rebases cleanly");
                        None
                    }
                    Some(Integration::Conflicts(paths)) => {
                        Some(format!("would conflict in {}", list(paths, 3)))
                    }
                    Some(Integration::Refused(why)) => Some(why.clone()),
                    Some(Integration::Unknown(e)) => Some(format!("can't predict conflicts: {e}")),
                    None => Some("can't predict conflicts".to_string()),
                }
            } else {
                None
            };
            v.push(Fix {
                kind: FixKind::Pull,
                op: Op::Pull,
                reason,
                blocked,
            });
        }
    } else if let (Some(b), Some(h), Some(remote)) = (head_branch, head_info, &s.remote)
        && h.is_unpushed()
    {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Publish,
            op: Op::PushSetUpstream {
                remote: remote.clone(),
                branch: b.to_string(),
            },
            reason: format!("{b} has no upstream"),
        });
    }

    if let Head::Detached(_) = s.head
        && s.detached_orphans > 0
    {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Branch,
            op: Op::CreateBranch {
                name: String::new(),
            },
            reason: format!("{} commit(s) on no branch", s.detached_orphans),
        });
    }
    if s.remote.is_none() {
        v.push(Fix {
            blocked: None,
            kind: FixKind::AddRemote,
            op: Op::AddRemote {
                name: "origin".into(),
                url: String::new(),
            },
            reason: "no remote".into(),
        });
    }

    if let Head::Detached(_) = s.head
        && !s.submodule
    {
        // the default branch, or the only one; several are left to the user
        let (here, blocked): (Vec<&String>, _) = match &s.default_branch {
            Some(d) if s.at_head.contains(d) => (vec![d], None),
            _ if s.at_head.len() == 1 => (s.at_head.iter().collect(), None),
            _ => (
                s.at_head.iter().collect(),
                Some(format!(
                    "{} branches are at HEAD: {}",
                    s.at_head.len(),
                    list(&s.at_head, 3)
                )),
            ),
        };
        for b in here {
            v.push(Fix {
                blocked: blocked.clone(),
                kind: FixKind::Reattach,
                op: Op::Switch { branch: b.clone() },
                reason: format!("HEAD is at {b}"),
            });
        }
    }

    for b in s.branches.iter().filter(|b| !b.is_head) {
        // where `push` would push it, if that exists (`remote_of` leaves out
        // a differently named upstream)
        let target = b.own.as_ref().or(b.upstream.as_ref());
        if let Some(t) = target.filter(|t| !t.gone && t.ahead > 0)
            && let Some(remote) = remote_of(t, &b.name)
        {
            v.push(Fix {
                blocked: diverged_from(t),
                kind: FixKind::PushBranches,
                op: Op::PushTo {
                    remote: remote.to_string(),
                    branch: b.name.clone(),
                },
                reason: format!("{} is {} commit(s) ahead of {}", b.name, t.ahead, t.name),
            });
        }
    }

    for b in s.branches.iter().filter(|b| !b.is_head) {
        if let Some(t) = &b.upstream
            && !t.gone
            && t.ahead == 0
            && t.behind > 0
        {
            v.push(Fix {
                blocked: None,
                kind: FixKind::FastForward,
                op: Op::FastForward {
                    branch: b.name.clone(),
                    upstream: t.name.clone(),
                },
                reason: format!("{} is {} behind {}", b.name, t.behind, t.name),
            });
        }
    }

    if let Some(default) = &s.default_branch {
        for b in s
            .branches
            .iter()
            .filter(|b| !b.is_head && b.merged && b.name != *default)
        {
            let gone = match &b.upstream {
                None => false,
                Some(t) if t.gone => true,
                Some(_) => continue,
            };
            v.push(Fix {
                blocked: None,
                kind: FixKind::DeleteBranches,
                op: Op::DeleteBranch {
                    branch: b.name.clone(),
                },
                reason: format!(
                    "{} is merged into {default}{}",
                    b.name,
                    if gone {
                        " and its upstream is gone"
                    } else {
                        ""
                    }
                ),
            });
        }
    }
    if let Some(message) = s.stashes.first()
        && !tracked_dirty
    {
        v.push(Fix {
            blocked: None,
            kind: FixKind::StashPop,
            op: Op::StashPop,
            reason: format!("stash@{{0}}: {message}"),
        });
    }

    // what a stash would take: untracked files only when asked
    let stashable = s.changes.stashable(stash.untracked);
    if stashable > 0 {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Stash,
            op: Op::stash(stash),
            reason: format!("{stashable} uncommitted path(s)"),
        });
    }
    if any_dirty {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Commit,
            op: Op::CommitAll {
                message: String::new(),
            },
            reason: format!("{} uncommitted path(s)", s.changes.uncommitted()),
        });
    }
    // or, instead of committing a moved submodule, put it back where
    // nothing is lost: its commit is held by a ref, and nothing of its own
    // is in the way; or its folder is gone
    if let Some(f) = put_back_submodules(&s.moved_submodules, &s.missing_submodules) {
        v.push(f);
    }
    let untracked: Vec<String> = s
        .changes
        .files
        .iter()
        .filter(|f| f.index == '?')
        .map(|f| f.path.clone())
        .collect();
    if !untracked.is_empty() {
        v.push(Fix {
            blocked: None,
            kind: FixKind::Gitignore,
            reason: format!("{} untracked path(s)", untracked.len()),
            op: Op::Ignore {
                file: crate::gitignore::IgnoreFile::TopLevel,
                paths: untracked,
                anchored: true,
            },
        });
    }
    v
}

/// The remote of `t`, a tracking ref named after `branch` (`origin/feat`).
fn remote_of<'a>(t: &'a Tracking, branch: &str) -> Option<&'a str> {
    t.name
        .strip_suffix(branch)
        .and_then(|r| r.strip_suffix('/'))
        .filter(|r| !r.is_empty())
}

/// Why pushing to `t` can't fast-forward, if it can't.
fn diverged_from(t: &Tracking) -> Option<String> {
    (t.behind > 0).then(|| {
        format!(
            "{} has {} commit(s) this branch doesn't (rebased?): pushing needs force",
            t.name, t.behind
        )
    })
}

/// `a, b, c and 2 more`
pub fn list(items: &[String], max: usize) -> String {
    let mut s = items[..items.len().min(max)].join(", ");
    if items.len() > max {
        s.push_str(&format!(" and {} more", items.len() - max));
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub fix: Fix,
    pub outcome: OpOutcome,
}

/// Every applicable fix in priority order: kinds in `priority` in the order
/// given, then the rest in [`FixKind::ALL`] order.
pub fn ranked(s: &RepoStatus, priority: &[FixKind], stash: &StashConfig) -> Vec<Fix> {
    let mut v = plan(s, stash);
    v.sort_by_key(|f| {
        priority
            .iter()
            .position(|&k| k == f.kind)
            .unwrap_or(priority.len() + FixKind::ALL.iter().position(|&k| k == f.kind).unwrap())
    });
    v
}

/// The fix an auto-fix action applies: the highest-priority one in `allowed`
/// that needs no input or confirmation and isn't blocked.
pub fn choose(s: &RepoStatus, allowed: &[FixKind], stash: &StashConfig) -> Option<Fix> {
    ranked(s, allowed, stash)
        .into_iter()
        .find(|f| allowed.contains(&f.kind) && !f.kind.manual() && f.blocked.is_none())
}

/// The fixes successive auto-fix actions would apply, starting with
/// [`choose`]: predicted with [`simulate`], assuming each one succeeds.
pub fn chain(s: &RepoStatus, allowed: &[FixKind], stash: &StashConfig) -> Vec<Fix> {
    chain_by(s, allowed, stash, simulate)
}

/// [`chain`], predicting with `sim`: guarded against one that makes no
/// progress (a repeat) or never settles (a cap).
fn chain_by(
    s: &RepoStatus,
    allowed: &[FixKind],
    stash: &StashConfig,
    sim: fn(&RepoStatus, &Fix) -> RepoStatus,
) -> Vec<Fix> {
    let mut out: Vec<Fix> = vec![];
    let mut cur = s.clone();
    while let Some(f) = choose(&cur, allowed, stash) {
        if out.iter().any(|g| g.kind == f.kind && g.op == f.op)
            || out.len() > 2 * FixKind::ALL.len()
        {
            break;
        }
        cur = sim(&cur, &f);
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
    /// priority order; possibly not allowed. Never ones needing input or
    /// confirmation.
    pub alternatives: Vec<Fix>,
}

pub fn preview(s: &RepoStatus, allowed: &[FixKind], stash: &StashConfig) -> Preview {
    let chain = chain(s, allowed, stash);
    // compare by label: a stash's message carries a timestamp
    let in_chain = |f: &Fix| {
        chain
            .iter()
            .any(|c| c.kind == f.kind && c.op.label() == f.op.label())
    };
    let alternatives = ranked(s, allowed, stash)
        .into_iter()
        .filter(|f| !f.kind.manual() && !in_chain(f))
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
        Op::Stash {
            message,
            untracked,
            keep_index,
            staged,
            paths,
        } => {
            s.stashes.insert(0, message.clone());
            s.stash_ids.insert(0, String::new()); // not known until made
            // only the plain stash the fix makes is simulated in detail
            if *staged || !paths.is_empty() {
                return s;
            }
            // submodules stay, at their commits and with what's inside
            // them, and so do nested repos
            let c = &mut s.changes;
            c.unstaged = c.unstaged_submodules;
            if *untracked {
                c.untracked = c.untracked_repos;
            }
            if !*keep_index {
                c.staged = c.staged_submodules;
            }
            c.files.retain_mut(|f| {
                if f.kind != PathKind::File {
                    return true;
                }
                if f.index == '?' {
                    return !*untracked;
                }
                if *keep_index && f.index != ' ' {
                    f.worktree = ' ';
                    return true;
                }
                false
            });
        }
        Op::CommitAll { .. } => {
            // what's inside submodules stays theirs (only they have tracked
            // paths with `m` or `?` in the work tree), and nested repos stay
            let c = &mut s.changes;
            c.files.retain(|f| {
                f.kind == PathKind::Repo || (f.index != '?' && matches!(f.worktree, 'm' | '?'))
            });
            let mut content = 0;
            for f in c.files.iter_mut().filter(|f| f.kind != PathKind::Repo) {
                f.index = ' ';
                content += 1;
            }
            s.changes = Changes {
                submodule_content: content,
                untracked: c.untracked_repos,
                untracked_repos: c.untracked_repos,
                files: std::mem::take(&mut c.files),
                ..Default::default()
            };
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
            // a diverged branch that merges gains the merge commit
            let merge = matches!(
                s.integration.take(),
                Some(Integration::Clean(PullBy::Merge))
            ) as usize;
            if let Some(t) = &mut s.upstream {
                t.behind = 0;
                t.ahead += merge;
            }
            if let Some(b) = head_info(&mut s, head.as_deref()) {
                b.unpushed = b.unpushed.map(|n| n + merge);
                if let Some(t) = &mut b.upstream {
                    t.behind = 0;
                    t.ahead += merge;
                }
            }
        }
        Op::PushTo { remote, branch } => {
            let name = format!("{remote}/{branch}");
            if let Some(b) = s.branches.iter_mut().find(|b| &b.name == branch) {
                b.unpushed = Some(0);
                for t in [b.own.as_mut(), b.upstream.as_mut()].into_iter().flatten() {
                    if t.name == name {
                        t.ahead = 0;
                        t.gone = false;
                    }
                }
                if b.is_head {
                    s.upstream = b.upstream.clone();
                }
            }
        }
        Op::CreateBranch { name } => {
            for b in &mut s.branches {
                b.is_head = false;
            }
            s.branches.insert(
                0,
                BranchInfo {
                    name: name.clone(),
                    upstream: None,
                    is_head: true,
                    merged: false,
                    unpushed: s.remote.as_ref().map(|_| s.detached_orphans),
                    own: None,
                },
            );
            s.head = Head::Branch(name.clone());
            s.upstream = None;
            s.detached_orphans = 0;
        }
        Op::AddRemote { name, url } => {
            s.remote = Some(name.clone());
            s.remote_url = Some(url.clone());
        }
        Op::Abort { .. } => {
            s.in_progress = None;
        }
        Op::DeleteBranch { branch } => s.branches.retain(|b| &b.name != branch),
        Op::PruneWorktrees => s.stale_worktrees.clear(),
        Op::PruneWorktree { path } => s.stale_worktrees.retain(|p| p != path),
        Op::StashPop => {
            s.stashes.remove(0);
            s.stash_ids.remove(0);
            // its changes, whatever they are
            s.changes.unstaged = s.changes.unstaged.max(1);
        }
        Op::StashDrop { index, .. } => {
            s.stashes.remove(*index);
            s.stash_ids.remove(*index);
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
                    own: None,
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
        Op::SubmoduleUpdate { paths } => {
            // their work-tree side goes; a staged side stays
            let c = &mut s.changes;
            let mut n = 0;
            c.files.retain_mut(|f| {
                if !paths.contains(&f.path) {
                    return true;
                }
                n += 1;
                f.worktree = ' ';
                f.index != ' '
            });
            c.unstaged -= n;
            c.unstaged_submodules -= n;
            s.moved_submodules.retain(|m| !paths.contains(&m.path));
            s.missing_submodules.retain(|p| !paths.contains(p));
        }
        // never fixes
        Op::Fetch
        | Op::Clone { .. }
        | Op::Commit { .. }
        | Op::Fixup { .. }
        | Op::SwitchTrack { .. }
        | Op::SpinOff { .. }
        | Op::Apply { .. }
        | Op::Reset { .. }
        | Op::Amend { .. }
        | Op::Add { .. }
        | Op::AddTracked { .. }
        | Op::Restore { .. }
        | Op::Remove { .. }
        | Op::Clean { .. }
        | Op::RemoveWorktree
        | Op::Ignore { .. } => {}
    }
    s
}

/// [`FixKind::SubmoduleUpdate`] for the submodules that can go back to the
/// recorded commit, and those whose folder is gone (nothing to lose);
/// blocked, with why, when none can.
fn put_back_submodules(moved: &[status::MovedSubmodule], missing: &[String]) -> Option<Fix> {
    let back: Vec<String> = moved
        .iter()
        .filter(|m| m.unreferenced == 0 && !m.changed)
        .map(|m| m.path.clone())
        .collect();
    let blocked = match moved.first() {
        Some(first) if back.is_empty() && missing.is_empty() => Some(match first {
            m if m.changed => format!("{} has changes of its own", m.path),
            m => format!(
                "{}: {} on no branch would be lost",
                m.path,
                match m.unreferenced {
                    1 => "1 commit".to_string(),
                    n => format!("{n} commits"),
                }
            ),
        }),
        None if missing.is_empty() => return None,
        _ => None,
    };
    let back = match (&blocked, moved.first()) {
        (Some(_), Some(first)) => vec![first.path.clone()],
        _ => back,
    };
    let mut reasons = vec![];
    match back.len() {
        0 => {}
        1 => reasons.push(format!("{} at another commit than recorded", back[0])),
        _ => reasons.push(format!(
            "{} at other commits than recorded",
            back.join(", ")
        )),
    }
    match missing {
        [] => {}
        [one] => reasons.push(format!("{one}'s folder is missing")),
        all => reasons.push(format!("folders missing: {}", all.join(", "))),
    }
    let mut paths = back;
    paths.extend(missing.iter().cloned());
    paths.sort();
    Some(Fix {
        blocked,
        kind: FixKind::SubmoduleUpdate,
        op: Op::SubmoduleUpdate { paths },
        reason: reasons.join("; "),
    })
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
pub fn apply_fix(
    path: &Path,
    allowed: &[FixKind],
    stash: &StashConfig,
) -> Result<Option<Applied>, status::StatusError> {
    let s = status::status(path, StatusOptions::default())?;
    let Some(fix) = choose(&s, allowed, stash) else {
        return Ok(None);
    };
    let outcome = ops::run(path, &fix.op);
    Ok(Some(Applied { fix, outcome }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::Issue;
    use crate::status::{StatusOptions, Tracking, status};
    use crate::testkit::{Sandbox, git};

    /// A clone whose `main` is checked out and `stale` is one behind.
    fn with_stale_branch() -> RepoStatus {
        let sb = Sandbox::new();
        sb.remote("acme", "w");
        let r = sb.clone_as_github("acme", "w", "w");
        git(&r, &["branch", "-q", "stale", "HEAD"]);
        let mut s = status(&r, StatusOptions::default()).unwrap();
        let b = s.branches.iter_mut().find(|b| b.name == "stale").unwrap();
        b.upstream = Some(Tracking {
            name: "origin/stale".into(),
            ahead: 0,
            behind: 1,
            gone: false,
        });
        s
    }

    fn ff_branches(v: &[Fix]) -> Vec<String> {
        v.iter()
            .map(|f| match &f.op {
                Op::FastForward { branch, .. } => branch.clone(),
                op => panic!("{op:?}"),
            })
            .collect()
    }

    #[test]
    fn a_chain_stops_when_the_simulation_makes_no_progress() {
        let s = with_stale_branch();
        let all = FixKind::ALL;
        let stash = StashConfig::default();
        assert_eq!(ff_branches(&chain(&s, &all, &stash)), ["stale"]);
        // the same fix again: stop instead of repeating it
        let unchanged = chain_by(&s, &all, &stash, |s, _| s.clone());
        assert_eq!(ff_branches(&unchanged), ["stale"]);
    }

    #[test]
    fn a_chain_that_never_settles_is_capped() {
        // each fast-forward makes another branch stale: a new fix every time
        fn another(s: &RepoStatus, f: &Fix) -> RepoStatus {
            let mut s = simulate(s, f);
            let mut b = s
                .branches
                .iter()
                .find(|b| b.name == "stale")
                .unwrap()
                .clone();
            b.name = format!("stale{}", s.branches.len());
            b.upstream.as_mut().unwrap().behind = 1;
            s.branches.push(b);
            s
        }
        let s = with_stale_branch();
        let v = chain_by(&s, &FixKind::ALL, &StashConfig::default(), another);
        assert_eq!(v.len(), 2 * FixKind::ALL.len() + 1);
        let names = ff_branches(&v);
        assert!(names.windows(2).all(|w| w[0] != w[1]), "{names:?}");
    }

    #[test]
    fn a_simulated_commit_adds_one_to_what_is_ahead_and_unpushed() {
        let mut s = with_stale_branch();
        let two = Tracking {
            name: "origin/main".into(),
            ahead: 2,
            behind: 0,
            gone: false,
        };
        s.upstream = Some(two.clone());
        let main = s.branches.iter_mut().find(|b| b.is_head).unwrap();
        main.upstream = Some(two);
        main.unpushed = Some(2);
        let commit = Fix {
            blocked: None,
            kind: FixKind::Commit,
            op: Op::CommitAll {
                message: "m".into(),
            },
            reason: String::new(),
        };
        let after = simulate(&s, &commit);
        assert_eq!(after.upstream.unwrap().ahead, 3);
        let main = after.branches.iter().find(|b| b.is_head).unwrap();
        assert_eq!(main.unpushed, Some(3));
        assert_eq!(main.upstream.as_ref().unwrap().ahead, 3);
        let stale = after.branches.iter().find(|b| !b.is_head).unwrap();
        assert_eq!(stale.upstream.as_ref().unwrap().ahead, 0, "only HEAD's");
    }

    #[test]
    fn a_simulated_switch_makes_the_branch_only_if_missing() {
        let s = with_stale_branch();
        let switch = |branch: &str| Fix {
            blocked: None,
            kind: FixKind::SwitchDefault,
            op: Op::Switch {
                branch: branch.into(),
            },
            reason: String::new(),
        };
        let names = |s: &RepoStatus| -> Vec<(String, bool)> {
            s.branches
                .iter()
                .map(|b| (b.name.clone(), b.is_head))
                .collect()
        };
        let after = simulate(&s, &switch("stale"));
        assert_eq!(
            names(&after),
            [("main".into(), false), ("stale".into(), true)]
        );
        assert_eq!(after.upstream.unwrap().behind, 1, "stale's upstream");
        let after = simulate(&s, &switch("new"));
        assert_eq!(
            names(&after),
            [
                ("main".into(), false),
                ("stale".into(), false),
                ("new".into(), true)
            ]
        );
        assert_eq!(after.upstream.unwrap().name, "origin/new");
    }

    #[test]
    fn a_blocked_fix_of_a_kind_in_the_chain_is_still_an_alternative() {
        let mut s = with_stale_branch();
        let stale = s
            .branches
            .iter()
            .find(|b| b.name == "stale")
            .unwrap()
            .clone();
        for (name, behind) in [("ahead", 0), ("diverged", 1)] {
            let mut b = stale.clone();
            b.name = name.into();
            b.upstream = Some(Tracking {
                name: format!("origin/{name}"),
                ahead: 1,
                behind,
                gone: false,
            });
            s.branches.push(b);
        }
        let p = preview(&s, &FixKind::ALL, &StashConfig::default());
        let push = |v: &[Fix]| -> Vec<(String, bool)> {
            v.iter()
                .filter(|f| f.kind == FixKind::PushBranches)
                .map(|f| (f.op.label(), f.blocked.is_some()))
                .collect()
        };
        let [(ahead, false)] = &push(&p.chain)[..] else {
            panic!("{:?}", p.chain)
        };
        let [(diverged, true)] = &push(&p.alternatives)[..] else {
            panic!("{:?}", p.alternatives)
        };
        assert!(ahead.contains("ahead") && diverged.contains("diverged"));
    }

    #[test]
    fn lists_say_how_many_more() {
        let items = |n: usize| (1..=n).map(|i| i.to_string()).collect::<Vec<_>>();
        assert_eq!(list(&items(3), 3), "1, 2, 3");
        assert_eq!(list(&items(4), 3), "1, 2, 3 and 1 more");
        assert_eq!(list(&items(1), 3), "1");
    }

    #[test]
    fn ignored_states_turn_off_the_fix_that_would_undo_them() {
        assert_eq!(
            turned_off_by(&[Issue::NonDefaultBranch]),
            [FixKind::SwitchDefault]
        );
        assert_eq!(turned_off_by(&[Issue::Detached]), [FixKind::Reattach]);
        assert_eq!(turned_off_by(&[Issue::NoUpstream]), [FixKind::Publish]);
        assert_eq!(turned_off_by(&[Issue::UpstreamGone]), [FixKind::Publish]);
        assert_eq!(turned_off_by(&[Issue::Stashes(1)]), [FixKind::StashPop]);
        assert_eq!(turned_off_by(&[Issue::Behind(3)]), [FixKind::Pull]);
        // inverse: issues with no fix to undo them
        for i in [Issue::NoRemote, Issue::UnpushedBranches(2)] {
            assert!(turned_off_by(&[i]).is_empty(), "{i:?}");
        }
    }
}
