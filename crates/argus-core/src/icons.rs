//! Every symbol the interface prints, as named, themeable icons: repo
//! badges (see [`crate::badges`]) and interface marks (spinner, marked
//! rows, job results). Sets are constants; `[ui.icon_overrides]` replaces
//! single icons. Plain punctuation stays as text ([`PUNCTUATION`]).

use std::borrow::Cow;
use std::collections::BTreeMap;

/// One glyph per thing argus marks: repo badges, then interface marks.
/// Built-in sets are constants; config can override single icons
/// (`[ui.icon_overrides]`). `argus demo --glyphs` shows them all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icons {
    pub favourite: Cow<'static, str>,
    pub new: Cow<'static, str>,
    pub excluded: Cow<'static, str>,
    pub snoozed: Cow<'static, str>,
    pub branch: Cow<'static, str>,
    pub detached: Cow<'static, str>,
    /// Commits only on a detached HEAD: lost on switching away.
    pub detached_commits: Cow<'static, str>,
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
    /// Spinner frames, one character each, for scans and running jobs.
    pub spinner: Cow<'static, str>,
    /// A row marked for a batch action (one cell).
    pub marked: Cow<'static, str>,
    /// A job or fix that succeeded.
    pub ok: Cow<'static, str>,
    /// A job or fix that failed.
    pub failed: Cow<'static, str>,
    /// Nothing needs attention.
    pub celebrate: Cow<'static, str>,
}

/// `ICON_NAMES` and lookup by name, from one list of `Icons` fields.
macro_rules! icon_names {
    ($($field:ident),* $(,)?) => {
        /// Icon names, as used in `[ui.icon_overrides]`.
        pub const ICON_NAMES: &[&str] = &[$(stringify!($field)),*];

        impl Icons {
            /// The glyph for an icon name (see [`ICON_NAMES`]).
            pub fn get(&self, name: &str) -> Option<&str> {
                match name {
                    $(stringify!($field) => Some(&self.$field),)*
                    _ => None,
                }
            }

            fn get_mut(&mut self, name: &str) -> Option<&mut Cow<'static, str>> {
                match name {
                    $(stringify!($field) => Some(&mut self.$field),)*
                    _ => None,
                }
            }
        }
    };
}

icon_names!(
    favourite,
    new,
    excluded,
    snoozed,
    branch,
    detached,
    detached_commits,
    ahead,
    behind,
    staged,
    unstaged,
    untracked,
    conflict,
    stash,
    in_progress,
    gone,
    no_upstream,
    branches,
    error,
    sync,
    worktree,
    submodule,
    spinner,
    marked,
    ok,
    failed,
    celebrate,
);

impl Icons {
    /// The spinner frame for `tick` (a space if there are no frames).
    pub fn spin(&self, tick: usize) -> &str {
        let n = self.spinner.chars().count();
        match self.spinner.char_indices().nth(tick % n.max(1)) {
            Some((at, c)) => &self.spinner[at..at + c.len_utf8()],
            None => " ",
        }
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
    detached_commits: s("†"),
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
    spinner: s("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    marked: s("▌"),
    ok: s("✓"),
    failed: s("✗"),
    celebrate: s("🎉"),
};

/// Nerd Font glyphs (codicons, Powerline). Each is followed by a space:
/// non-Mono Nerd Fonts draw them up to two cells wide.
pub const NERD: Icons = Icons {
    favourite: s("\u{eb59} "),        // cod-star_full
    new: s("\u{ec21} "),              // cod-sparkle_filled
    excluded: s("\u{eae7} "),         // cod-eye_closed
    snoozed: s("\u{ec08} "),          // cod-bell_slash
    branch: s("\u{e0a0} "),           // Powerline branch
    detached: s("\u{eafc} "),         // cod-git_commit
    detached_commits: s("\u{ea82} "), // cod-history
    ahead: s("\u{eaa0} "),            // cod-arrow_small_up
    behind: s("\u{ea9d} "),           // cod-arrow_small_down
    staged: s("\u{eadc} "),           // cod-diff_added
    unstaged: s("\u{eade} "),         // cod-diff_modified
    untracked: s("\u{eb32} "),        // cod-question
    conflict: s("\u{ea76} "),         // cod-close
    stash: s("\u{ea98} "),            // cod-archive
    in_progress: s("\u{ea6c} "),      // cod-warning
    gone: s("\u{eac1} "),             // cod-close_all
    no_upstream: s("\u{ead0} "),      // cod-debug_disconnect
    branches: s("\u{ea63} "),         // cod-repo_forked
    error: s("\u{ea87} "),            // cod-error
    sync: s("\u{ea77} "),             // cod-sync
    worktree: s("├"),
    submodule: s("\u{eaec} "), // cod-file_submodule
    spinner: s("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    marked: s("▌"),
    ok: s("\u{eab2} "),     // cod-check
    failed: s("\u{ea76} "), // cod-close
    celebrate: s("🎉"),
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
    detached_commits: s("c"),
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
    spinner: s("|/-\\"),
    marked: s(">"),
    ok: s("ok"),
    failed: s("FAILED"),
    celebrate: s(""),
};

/// Punctuation the interface uses as plain text, not as icons: in every
/// common monospace font.
pub const PUNCTUATION: &str = "…·—↑↓";

/// Glyphs worth trying in `[ui.icon_overrides]`, from best to least
/// supported by common monospace fonts.
pub const ALTERNATIVES: &str = "◊○●•∙…‹›«»§¶±≠×÷∞◆✓■□▲▼▶◀◇◉⌂⌘⏎♦✗‼⋯↯⇅⇡⇣✔✘⚡★☆✦✧✱⚑⚠⎇";

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
