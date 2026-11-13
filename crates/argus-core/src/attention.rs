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
    /// Records of linked worktrees whose folder was deleted: their
    /// branches still count as checked out there.
    StaleWorktrees(usize),
    /// A linked worktree whose branch is merged, upstream gone, nothing
    /// uncommitted: done with.
    FinishedWorktree,
}

impl Issue {
    /// Every kind, counts left at 0.
    pub const ALL: [Issue; 15] = [
        Issue::Conflicts(0),
        Issue::InProgress,
        Issue::Dirty(0),
        Issue::Unpushed(0),
        Issue::UpstreamGone,
        Issue::NoUpstream,
        Issue::UnpushedBranches(0),
        Issue::Detached,
        Issue::DetachedCommits(0),
        Issue::NonDefaultBranch,
        Issue::Stashes(0),
        Issue::Behind(0),
        Issue::NoRemote,
        Issue::StaleWorktrees(0),
        Issue::FinishedWorktree,
    ];

    /// The kind's name, without its count (snoozes are kept by it).
    pub fn name(self) -> &'static str {
        match self {
            Issue::Conflicts(_) => "conflicts",
            Issue::InProgress => "in_progress",
            Issue::Dirty(_) => "dirty",
            Issue::Unpushed(_) => "unpushed",
            Issue::UpstreamGone => "upstream_gone",
            Issue::NoUpstream => "no_upstream",
            Issue::UnpushedBranches(_) => "unpushed_branches",
            Issue::Detached => "detached",
            Issue::DetachedCommits(_) => "detached_commits",
            Issue::NonDefaultBranch => "non_default_branch",
            Issue::Stashes(_) => "stashes",
            Issue::Behind(_) => "behind",
            Issue::NoRemote => "no_remote",
            Issue::StaleWorktrees(_) => "stale_worktrees",
            Issue::FinishedWorktree => "finished_worktree",
        }
    }

    /// The kind named (`-` or `_` between words), its count 0.
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.replace('-', "_");
        Self::ALL.into_iter().find(|i| i.name() == name)
    }

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
            Issue::NonDefaultBranch | Issue::FinishedWorktree => 20,
            Issue::StaleWorktrees(_) => 15,
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
        // said instead of not being on the default branch
        Head::Branch(_) if s.is_finished_worktree() => v.push(Issue::FinishedWorktree),
        Head::Branch(_) if s.default_branch.is_some() && !s.on_default() => {
            v.push(Issue::NonDefaultBranch)
        }
        _ => {}
    }
    if !s.stashes.is_empty() {
        v.push(Issue::Stashes(s.stashes.len()));
    }
    if !s.stale_worktrees.is_empty() {
        v.push(Issue::StaleWorktrees(s.stale_worktrees.len()));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Names leave the count out, read back, and are all different.
    #[test]
    fn issues_have_names_without_their_count() {
        assert_eq!(Issue::Stashes(3).name(), "stashes");
        assert_eq!(Issue::NonDefaultBranch.name(), "non_default_branch");
        assert_eq!(
            Issue::from_name("non-default-branch"),
            Some(Issue::NonDefaultBranch)
        );
        assert_eq!(Issue::from_name("stashes"), Some(Issue::Stashes(0)));
        assert_eq!(Issue::from_name("nope"), None);
        for i in Issue::ALL {
            assert_eq!(Issue::from_name(i.name()), Some(i), "{i:?}");
        }
        let names: std::collections::BTreeSet<&str> = Issue::ALL.iter().map(|i| i.name()).collect();
        assert_eq!(names.len(), Issue::ALL.len());
    }

    #[test]
    fn only_being_behind_is_for_a_pull() {
        assert!(Issue::Behind(2).is_pull_only());
        for i in [Issue::Dirty(1), Issue::Detached, Issue::NoUpstream] {
            assert!(!i.is_pull_only(), "{i:?}");
        }
    }
}
