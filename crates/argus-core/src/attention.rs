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
    /// Current branch is ahead of its upstream.
    Unpushed(usize),
    /// Current branch's upstream was deleted on the remote.
    UpstreamGone,
    /// Current branch has no upstream and isn't merged into the default branch.
    NoUpstream,
    /// Other local branches with commits that exist nowhere else.
    UnpushedBranches(usize),
    Detached,
    OffDefault,
    Stashes(usize),
    Behind(usize),
    NoRemote,
}

impl Issue {
    /// Higher is more urgent.
    pub fn severity(self) -> u32 {
        match self {
            Issue::Conflicts(_) | Issue::InProgress => 100,
            Issue::Unpushed(_) => 60,
            Issue::Dirty(_) => 50,
            Issue::UpstreamGone => 40,
            Issue::NoUpstream => 35,
            Issue::UnpushedBranches(_) => 30,
            Issue::Detached => 25,
            Issue::OffDefault => 20,
            Issue::Stashes(_) => 10,
            Issue::NoRemote => 8,
            Issue::Behind(_) => 5,
        }
    }

    /// Issues that merely mean "a pull would help" rather than "look at me".
    pub fn is_sync_only(self) -> bool {
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
    match (&s.upstream, head_info) {
        (Some(t), _) if t.gone => {
            if !head_info.is_some_and(|h| h.merged) {
                v.push(Issue::UpstreamGone)
            }
        }
        (Some(t), _) if t.ahead > 0 => v.push(Issue::Unpushed(t.ahead)),
        (None, Some(h)) if !h.merged && s.remote.is_some() => v.push(Issue::NoUpstream),
        _ => {}
    }
    let others = s.branches.iter().filter(|b| !b.is_head && b.is_unpushed()).count();
    if others > 0 {
        v.push(Issue::UnpushedBranches(others));
    }
    match &s.head {
        Head::Detached(_) => v.push(Issue::Detached),
        Head::Branch(_) if s.default_branch.is_some() && !s.on_default() => v.push(Issue::OffDefault),
        _ => {}
    }
    if s.stashes > 0 {
        v.push(Issue::Stashes(s.stashes));
    }
    if s.remote.is_none() && !s.bare {
        v.push(Issue::NoRemote);
    }
    if let Some(t) = &s.upstream {
        if t.behind > 0 && !t.gone {
            v.push(Issue::Behind(t.behind));
        }
    }
    v.sort_by_key(|i| std::cmp::Reverse(i.severity()));
    v
}

/// Severity of the worst issue, 0 if none.
pub fn score(issues: &[Issue]) -> u32 {
    issues.iter().map(|i| i.severity()).max().unwrap_or(0)
}

pub fn needs_attention(issues: &[Issue]) -> bool {
    issues.iter().any(|i| !i.is_sync_only())
}
