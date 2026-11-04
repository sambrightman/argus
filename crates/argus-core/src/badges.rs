//! Succinct, UI-agnostic summary of an entry as a list of badges. Front-ends
//! map [`Role`] to colours/styles; the text is already compact.

use serde::Serialize;

use crate::attention::Issue;
use crate::status::Head;
use crate::workspace::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Favourite,
    New,
    Hidden,
    Branch,
    DefaultBranch,
    Detached,
    Ahead,
    Behind,
    Staged,
    Unstaged,
    Untracked,
    Conflict,
    Stash,
    Warning,
    Error,
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Badge {
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Icons {
    pub favourite: &'static str,
    pub new: &'static str,
    pub excluded: &'static str,
    pub snoozed: &'static str,
    pub branch: &'static str,
    pub detached: &'static str,
    pub ahead: &'static str,
    pub behind: &'static str,
    pub staged: &'static str,
    pub unstaged: &'static str,
    pub untracked: &'static str,
    pub conflict: &'static str,
    pub stash: &'static str,
    pub in_progress: &'static str,
    pub gone: &'static str,
    pub no_upstream: &'static str,
    pub branches: &'static str,
    pub error: &'static str,
    pub sync: &'static str,
    pub worktree: &'static str,
    pub submodule: &'static str,
}

pub const UNICODE: Icons = Icons {
    favourite: "★",
    new: "✦",
    excluded: "⊘",
    snoozed: "☾",
    branch: "",
    detached: "➦",
    ahead: "↑",
    behind: "↓",
    staged: "●",
    unstaged: "✚",
    untracked: "…",
    conflict: "✖",
    stash: "⚑",
    in_progress: "⚠",
    gone: "✗",
    no_upstream: "⌀",
    branches: "⑂",
    error: "‼",
    sync: "⟳",
    worktree: "├",
    submodule: "◫",
};

/// Nerd Font / Powerline glyphs where they add something.
pub const NERD: Icons = Icons {
    branch: "\u{e0a0} ",
    stash: "\u{f01c} ",
    error: "\u{f071} ",
    ..UNICODE
};

pub fn by_name(name: &str) -> Icons {
    match name {
        "ascii" => ASCII,
        "nerd" => NERD,
        _ => UNICODE,
    }
}

pub const ASCII: Icons = Icons {
    favourite: "*",
    new: "+",
    excluded: "x",
    snoozed: "z",
    branch: "",
    detached: "@",
    ahead: "^",
    behind: "v",
    staged: "S",
    unstaged: "M",
    untracked: "?",
    conflict: "!",
    stash: "$",
    in_progress: "!!",
    gone: "gone",
    no_upstream: "-u",
    branches: "b",
    error: "ERR",
    sync: "~",
    worktree: "|",
    submodule: "[]",
};

/// Flag badges (favourite, new, hidden, sync), shown before the name.
pub fn flags(e: &Entry, i: &Icons) -> Vec<Badge> {
    let mut v = vec![];
    let b = |role, text: &str| Badge {
        role,
        text: text.to_string(),
    };
    if e.flags.favourite {
        v.push(b(Role::Favourite, i.favourite));
    }
    if e.flags.new {
        v.push(b(Role::New, i.new));
    }
    if e.flags.excluded {
        v.push(b(Role::Hidden, i.excluded));
    } else if e.flags.snoozed {
        v.push(b(Role::Hidden, i.snoozed));
    }
    v
}

/// Branch badge.
pub fn branch(e: &Entry, i: &Icons) -> Option<Badge> {
    let s = e.status()?;
    Some(match &s.head {
        Head::Branch(b) | Head::Unborn(b) => Badge {
            role: if s.on_default() {
                Role::DefaultBranch
            } else {
                Role::Branch
            },
            text: format!("{}{b}", i.branch),
        },
        Head::Detached(id) => Badge {
            role: Role::Detached,
            text: format!("{}{id}", i.detached),
        },
    })
}

/// State badges after the branch: ahead/behind, changes, stashes, problems.
pub fn state(e: &Entry, i: &Icons) -> Vec<Badge> {
    let mut v = vec![];
    if let Some(err) = e.error() {
        let first = err.lines().next().unwrap_or(err);
        v.push(Badge {
            role: Role::Error,
            text: format!("{} {first}", i.error),
        });
        return v;
    }
    let Some(s) = e.status() else { return v };
    let mut push = |role, text: String| v.push(Badge { role, text });
    if let Some(op) = s.in_progress {
        push(Role::Conflict, format!("{} {}", i.in_progress, op.label()));
    }
    if s.changes.conflicted > 0 {
        push(
            Role::Conflict,
            format!("{}{}", i.conflict, s.changes.conflicted),
        );
    }
    if let Some(t) = &s.upstream {
        if t.gone {
            push(Role::Warning, i.gone.to_string());
        }
        if t.ahead > 0 {
            push(Role::Ahead, format!("{}{}", i.ahead, t.ahead));
        }
        if t.behind > 0 {
            push(Role::Behind, format!("{}{}", i.behind, t.behind));
        }
    }
    if e.issues.contains(&Issue::NoUpstream) {
        push(Role::Warning, i.no_upstream.to_string());
    }
    if s.changes.staged > 0 {
        push(Role::Staged, format!("{}{}", i.staged, s.changes.staged));
    }
    if s.changes.unstaged > 0 {
        push(
            Role::Unstaged,
            format!("{}{}", i.unstaged, s.changes.unstaged),
        );
    }
    if s.changes.untracked > 0 {
        push(
            Role::Untracked,
            format!("{}{}", i.untracked, s.changes.untracked),
        );
    }
    if s.stashes > 0 {
        push(Role::Stash, format!("{}{}", i.stash, s.stashes));
    }
    for issue in &e.issues {
        match issue {
            Issue::UnpushedBranches(n) => push(Role::Warning, format!("{}{n}", i.branches)),
            Issue::DetachedCommits(n) => push(Role::Conflict, format!("{}{n}", i.detached)),
            _ => {}
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_sets_are_complete() {
        // every icon in both sets is non-empty except the optional branch glyph
        for set in [UNICODE, ASCII] {
            for s in [
                set.favourite,
                set.new,
                set.ahead,
                set.behind,
                set.staged,
                set.unstaged,
                set.untracked,
                set.stash,
            ] {
                assert!(!s.is_empty());
            }
        }
        assert!(
            ASCII.favourite.is_ascii()
                && ASCII.untracked.is_ascii()
                && ASCII.in_progress.is_ascii()
        );
    }
}
