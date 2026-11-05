//! Succinct, UI-agnostic summary of an entry as a list of badges. Front-ends
//! map [`Role`] to colours/styles; the text is already compact.

use std::borrow::Cow;
use std::collections::BTreeMap;

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

/// One glyph per thing argus marks. Built-in sets are constants; config
/// can override single icons (`[ui.icon_overrides]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icons {
    pub favourite: Cow<'static, str>,
    pub new: Cow<'static, str>,
    pub excluded: Cow<'static, str>,
    pub snoozed: Cow<'static, str>,
    pub branch: Cow<'static, str>,
    pub detached: Cow<'static, str>,
    pub ahead: Cow<'static, str>,
    pub behind: Cow<'static, str>,
    pub staged: Cow<'static, str>,
    pub unstaged: Cow<'static, str>,
    pub untracked: Cow<'static, str>,
    pub conflict: Cow<'static, str>,
    pub stash: Cow<'static, str>,
    pub in_progress: Cow<'static, str>,
    pub gone: Cow<'static, str>,
    pub no_upstream: Cow<'static, str>,
    pub branches: Cow<'static, str>,
    pub error: Cow<'static, str>,
    pub sync: Cow<'static, str>,
    pub worktree: Cow<'static, str>,
    pub submodule: Cow<'static, str>,
}

/// Icon names, as used in `[ui.icon_overrides]`.
pub const ICON_NAMES: &[&str] = &[
    "favourite",
    "new",
    "excluded",
    "snoozed",
    "branch",
    "detached",
    "ahead",
    "behind",
    "staged",
    "unstaged",
    "untracked",
    "conflict",
    "stash",
    "in_progress",
    "gone",
    "no_upstream",
    "branches",
    "error",
    "sync",
    "worktree",
    "submodule",
];

impl Icons {
    fn get_mut(&mut self, name: &str) -> Option<&mut Cow<'static, str>> {
        Some(match name {
            "favourite" => &mut self.favourite,
            "new" => &mut self.new,
            "excluded" => &mut self.excluded,
            "snoozed" => &mut self.snoozed,
            "branch" => &mut self.branch,
            "detached" => &mut self.detached,
            "ahead" => &mut self.ahead,
            "behind" => &mut self.behind,
            "staged" => &mut self.staged,
            "unstaged" => &mut self.unstaged,
            "untracked" => &mut self.untracked,
            "conflict" => &mut self.conflict,
            "stash" => &mut self.stash,
            "in_progress" => &mut self.in_progress,
            "gone" => &mut self.gone,
            "no_upstream" => &mut self.no_upstream,
            "branches" => &mut self.branches,
            "error" => &mut self.error,
            "sync" => &mut self.sync,
            "worktree" => &mut self.worktree,
            "submodule" => &mut self.submodule,
            _ => return None,
        })
    }

    /// Replace single icons; returns warnings for unknown names.
    pub fn with_overrides(mut self, overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut warnings = vec![];
        for (name, glyph) in overrides {
            match self.get_mut(name) {
                Some(slot) => *slot = Cow::Owned(glyph.clone()),
                None => warnings.push(format!(
                    "unknown icon in [ui.icon_overrides]: {name} (known: {})",
                    ICON_NAMES.join(", ")
                )),
            }
        }
        (self, warnings)
    }
}

const fn s(glyph: &'static str) -> Cow<'static, str> {
    Cow::Borrowed(glyph)
}

/// Glyphs that common monospace fonts (Menlo, SF Mono, Inconsolata, DejaVu
/// Sans Mono, Fira Code, Hack, Source Code Pro) all have, so none is drawn
/// from a fallback font that can overlap the next character.
pub const UNICODE: Icons = Icons {
    favourite: s("◆"),
    new: s("+"),
    excluded: s("×"),
    snoozed: s("z"),
    branch: s(""),
    detached: s("@"),
    ahead: s("↑"),
    behind: s("↓"),
    staged: s("●"),
    unstaged: s("±"),
    untracked: s("?"),
    conflict: s("≠"),
    stash: s("$"),
    in_progress: s("!"),
    gone: s("×"),
    no_upstream: s("↑?"),
    branches: s("»"),
    error: s("!!"),
    sync: s("~"),
    worktree: s("├"),
    submodule: s("§"),
};

/// Nerd Font glyphs (codicons, Powerline). Each is followed by a space:
/// non-Mono Nerd Fonts draw them up to two cells wide.
pub const NERD: Icons = Icons {
    favourite: s("\u{eb59} "),   // cod-star_full
    new: s("\u{ec21} "),         // cod-sparkle_filled
    excluded: s("\u{eae7} "),    // cod-eye_closed
    snoozed: s("\u{ec08} "),     // cod-bell_slash
    branch: s("\u{e0a0} "),      // Powerline branch
    detached: s("\u{eafc} "),    // cod-git_commit
    ahead: s("\u{eaa0} "),       // cod-arrow_small_up
    behind: s("\u{ea9d} "),      // cod-arrow_small_down
    staged: s("\u{eadc} "),      // cod-diff_added
    unstaged: s("\u{eade} "),    // cod-diff_modified
    untracked: s("\u{eb32} "),   // cod-question
    conflict: s("\u{ea76} "),    // cod-close
    stash: s("\u{ea98} "),       // cod-archive
    in_progress: s("\u{ea6c} "), // cod-warning
    gone: s("\u{eac1} "),        // cod-close_all
    no_upstream: s("\u{ead0} "), // cod-debug_disconnect
    branches: s("\u{ea63} "),    // cod-repo_forked
    error: s("\u{ea87} "),       // cod-error
    sync: s("\u{ea77} "),        // cod-sync
    worktree: s("├"),
    submodule: s("\u{eaec} "), // cod-file_submodule
};

/// Names accepted by [`by_name`] (`[ui] icons`).
pub const SETS: &[&str] = &["unicode", "nerd", "ascii"];

pub fn by_name(name: &str) -> Icons {
    match name {
        "ascii" => ASCII,
        "nerd" => NERD,
        _ => UNICODE,
    }
}

pub const ASCII: Icons = Icons {
    favourite: s("*"),
    new: s("+"),
    excluded: s("x"),
    snoozed: s("z"),
    branch: s(""),
    detached: s("@"),
    ahead: s("^"),
    behind: s("v"),
    staged: s("S"),
    unstaged: s("M"),
    untracked: s("?"),
    conflict: s("!"),
    stash: s("$"),
    in_progress: s("!!"),
    gone: s("gone"),
    no_upstream: s("-u"),
    branches: s("b"),
    error: s("ERR"),
    sync: s("~"),
    worktree: s("|"),
    submodule: s("[]"),
};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_sets_are_complete() {
        // every icon in both sets is non-empty except the optional branch glyph
        for set in [UNICODE, ASCII] {
            for s in [
                &set.favourite,
                &set.new,
                &set.ahead,
                &set.behind,
                &set.staged,
                &set.unstaged,
                &set.untracked,
                &set.stash,
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
