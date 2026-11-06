//! Succinct, UI-agnostic summary of an entry as a list of badges: each an
//! icon (see [`crate::icons`]) with the repo's data (a count, branch or tag
//! name) and a colour [`Role`]. Front-ends map roles to colours/styles; the
//! text is already compact.

use serde::Serialize;

use crate::attention::Issue;
use crate::icons::Icons;
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

/// Flag badges (favourite, new, hidden, sync), shown before the name.
pub fn flags(e: &Entry, i: &Icons) -> Vec<Badge> {
    let mut v = vec![];
    let b = |role, text: &str| Badge {
        role,
        text: text.to_string(),
    };
    if e.flags.favourite {
        v.push(b(Role::Favourite, &i.favourite));
    }
    if e.flags.new {
        v.push(b(Role::New, &i.new));
    }
    if e.flags.excluded {
        v.push(b(Role::Hidden, &i.excluded));
    } else if e.flags.snoozed {
        v.push(b(Role::Hidden, &i.snoozed));
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
        // On a tag, the tag says more than the commit id.
        Head::Detached(id) => Badge {
            role: Role::Detached,
            text: match s.tags.as_slice() {
                [] => format!("{}{id}", i.detached),
                [t] => format!("{}{t}", i.detached),
                [t, rest @ ..] => format!("{}{t} +{}", i.detached, rest.len()),
            },
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
            text: format!("{} {first}", i.error.trim_end()),
        });
        return v;
    }
    let Some(s) = e.status() else { return v };
    let mut push = |role, text: String| v.push(Badge { role, text });
    if let Some(op) = s.in_progress {
        push(
            Role::Conflict,
            format!("{} {}", i.in_progress.trim_end(), op.label()),
        );
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
