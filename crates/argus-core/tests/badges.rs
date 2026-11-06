//! Badge text and icon sets.

use argus_core::badges::{self, ASCII, Icons, NERD, UNICODE};
use argus_core::testkit::{Sandbox, commit, git};
use argus_core::{Config, State, Workspace};

fn icons(set: &Icons) -> Vec<(&'static str, &str)> {
    vec![
        ("favourite", &*set.favourite),
        ("new", &*set.new),
        ("excluded", &*set.excluded),
        ("snoozed", &*set.snoozed),
        ("branch", &*set.branch),
        ("detached", &*set.detached),
        ("ahead", &*set.ahead),
        ("behind", &*set.behind),
        ("staged", &*set.staged),
        ("unstaged", &*set.unstaged),
        ("untracked", &*set.untracked),
        ("conflict", &*set.conflict),
        ("stash", &*set.stash),
        ("in_progress", &*set.in_progress),
        ("gone", &*set.gone),
        ("no_upstream", &*set.no_upstream),
        ("branches", &*set.branches),
        ("error", &*set.error),
        ("sync", &*set.sync),
        ("worktree", &*set.worktree),
        ("submodule", &*set.submodule),
    ]
}

/// Characters every common monospace font has (checked against Menlo,
/// Inconsolata, DejaVu Sans Mono, Fira Code, Hack and Source Code Pro).
/// Anything else is drawn from a fallback font and may overlap its
/// neighbour.
const SAFE: &str = "↑↓●◆×±≠…├└│»§•∞";

#[test]
fn unicode_icons_are_in_common_fonts() {
    for (name, icon) in icons(&UNICODE) {
        for c in icon.chars() {
            assert!(
                c.is_ascii() || SAFE.contains(c),
                "{name}: {c} (U+{:04X}) isn't in common fonts",
                c as u32
            );
        }
    }
}

#[test]
fn nerd_icons_cover_everything_and_leave_room() {
    // Non-Mono Nerd Fonts draw icons up to two cells wide: each needs a
    // trailing space, and none may fall back to the Unicode glyph.
    for ((name, nerd), (_, uni)) in icons(&NERD).into_iter().zip(icons(&UNICODE)) {
        if nerd.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)) {
            assert!(
                nerd.ends_with(' '),
                "{name}: {nerd:?} needs a trailing space"
            );
        } else {
            assert!(
                name == "worktree" || nerd != uni || uni.is_ascii(),
                "{name}: not a Nerd glyph ({nerd:?})"
            );
        }
    }
}

#[test]
fn ascii_icons_are_ascii() {
    for (name, icon) in icons(&ASCII) {
        assert!(icon.is_ascii(), "{name}: {icon}");
    }
}

fn entry_badge(r: &std::path::Path) -> String {
    let cfg = format!("roots = ['{}']", r.parent().unwrap().display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    badges::branch(&ws.entries[0], &UNICODE).unwrap().text
}

#[test]
fn detached_head_shows_its_tag_or_commit() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    commit(&r, "second");
    git(&r, &["tag", "v1.9"]);
    git(&r, &["tag", "v1.10"]);
    git(&r, &["tag", "-a", "-m", "r", "release"]);
    assert_eq!(entry_badge(&r), "main", "a branch shows the branch");
    git(&r, &["switch", "-q", "--detach", "v1.9"]);
    assert_eq!(entry_badge(&r), "@release +2");
    git(&r, &["tag", "-d", "release", "v1.10"]);
    assert_eq!(entry_badge(&r), "@v1.9");
    git(&r, &["switch", "-q", "--detach", "HEAD~1"]);
    let id = git(&r, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(entry_badge(&r), format!("@{}", id.trim()));
}

#[test]
fn icon_overrides_replace_single_icons_on_any_set() {
    let c = Config::parse(
        "[ui]\nicons = 'nerd'\n[ui.icon_overrides]\nfavourite = '★'\nuntracked = '…'\nbogus = 'x'",
    )
    .unwrap();
    let (i, warnings) = c.ui.icons();
    assert_eq!(i.favourite, "★");
    assert_eq!(i.untracked, "…");
    assert_eq!(i.ahead, NERD.ahead, "the rest come from the chosen set");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("bogus") && warnings[0].contains("favourite"),
        "{warnings:?}"
    );
    // inverse: no overrides, the set as is
    let (i, warnings) = Config::default().ui.icons();
    assert_eq!(i, UNICODE);
    assert!(warnings.is_empty());
    // every icon can be overridden, by the names the legend uses
    for name in badges::ICON_NAMES {
        let c = Config::parse(&format!("[ui.icon_overrides]\n{name} = 'X'")).unwrap();
        let (i, w) = c.ui.icons();
        assert!(w.is_empty(), "{name}: {w:?}");
        assert!(
            icons(&i).iter().any(|(n, v)| n == name && *v == "X"),
            "{name}"
        );
    }
}

#[test]
fn icons_can_be_looked_up_by_name() {
    for set in [&UNICODE, &NERD, &ASCII] {
        let listed: Vec<_> = icons(set);
        let looked_up: Vec<_> = badges::ICON_NAMES
            .iter()
            .map(|&n| (n, set.get(n).unwrap()))
            .collect();
        assert_eq!(looked_up, listed);
    }
    // inverse: unknown names
    assert_eq!(UNICODE.get("favourit"), None);
    assert_eq!(UNICODE.get(""), None);
}
