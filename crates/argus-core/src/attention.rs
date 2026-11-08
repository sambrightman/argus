//! What about a repository needs the user's attention, and how badly.

use serde::{Deserialize, Serialize};

use crate::status::{Head, RepoStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "issue", content = "n", rename_all = "kebab-case")]
pub enum Issue {
    Conflicts(usize),
    InProgress,
    /// Uncommitted changes (staged, unstaged or untracked paths).
    Dirty(usize),
    /// Current branch has commits on no remote.
    Unpushed(usize),
    /// Current branch's upstream was deleted on the remote, with work on no remote.
    UpstreamGone,
    /// Current branch has no upstream and work on no remote.
    NoUpstream,
    /// Other local branches with commits that exist nowhere else.
    UnpushedBranches(usize),
    Detached,
    /// Detached HEAD with commits no ref holds.
    DetachedCommits(usize),
    NonDefaultBranch,
    Stashes(usize),
    Behind(usize),
    NoRemote,
}

impl Issue {
    /// Higher is more urgent.
    pub fn severity(self) -> u32 {
        match self {
            Issue::Conflicts(_) | Issue::InProgress => 100,
            Issue::DetachedCommits(_) => 70,
            Issue::Unpushed(_) => 60,
            Issue::Dirty(_) => 50,
            Issue::UpstreamGone => 40,
            Issue::NoUpstream => 35,
            Issue::UnpushedBranches(_) => 30,
            Issue::Detached => 25,
            Issue::NonDefaultBranch => 20,
            Issue::Stashes(_) => 10,
            Issue::NoRemote => 8,
            Issue::Behind(_) => 5,
        }
    }

    /// Issues that merely mean "a pull would help" rather than "look at me".
    pub fn is_pull_only(self) -> bool {
        matches!(self, Issue::Behind(_))
    }
}

pub fn issues(s: &RepoStatus) -> Vec<Issue> {
    let mut v = vec![];
    if s.changes.conflicted > 0 {
        v.push(Issue::Conflicts(s.changes.conflicted));
    }
    if s.in_progress.is_some() {
        v.push(Issue::InProgress);
    }
    let dirty = s.changes.staged + s.changes.unstaged + s.changes.untracked;
    if dirty > 0 {
        v.push(Issue::Dirty(dirty));
    }
    let head_branch = match &s.head {
        Head::Branch(b) => Some(b.as_str()),
        _ => None,
    };
    let head_info = head_branch.and_then(|b| s.branches.iter().find(|bi| bi.name == b));
    if let Some(h) = head_info.filter(|h| h.is_unpushed()) {
        match &s.upstream {
            Some(t) if t.gone => v.push(Issue::UpstreamGone),
            Some(_) => v.push(Issue::Unpushed(h.unpushed.unwrap_or(0))),
            None if s.remote.is_some() => v.push(Issue::NoUpstream),
            None => {}
        }
    }
    let others = s
        .branches
        .iter()
        .filter(|b| !b.is_head && b.is_unpushed())
        .count();
    // Branches are shared between worktrees; the main checkout reports them.
    // Without a remote, nothing is pushed: `NoRemote` says it once.
    if others > 0 && !s.linked_worktree && s.remote.is_some() {
        v.push(Issue::UnpushedBranches(others));
    }
    match &s.head {
        Head::Detached(_) if s.submodule => {}
        Head::Detached(_) => {
            v.push(Issue::Detached);
            if s.detached_orphans > 0 {
                v.push(Issue::DetachedCommits(s.detached_orphans));
            }
        }
        Head::Branch(_) if s.default_branch.is_some() && !s.on_default() => {
            v.push(Issue::NonDefaultBranch)
        }
        _ => {}
    }
    if !s.stashes.is_empty() {
        v.push(Issue::Stashes(s.stashes.len()));
    }
    if s.remote.is_none() && !s.bare {
        v.push(Issue::NoRemote);
    }
    if let Some(t) = &s.upstream
        && t.behind > 0
        && !t.gone
    {
        v.push(Issue::Behind(t.behind));
    }
    v.sort_by_key(|i| std::cmp::Reverse(i.severity()));
    v
}

/// Severity of the worst issue, 0 if none.
pub fn score(issues: &[Issue]) -> u32 {
    issues.iter().map(|i| i.severity()).max().unwrap_or(0)
}

pub fn needs_attention(issues: &[Issue]) -> bool {
    issues.iter().any(|i| !i.is_pull_only())
}
