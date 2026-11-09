//! Icon sets: completeness, fonts, overrides, lookup.

use argus_core::Config;
use argus_core::icons::{self, ASCII, Icons, NERD, UNICODE};

fn icons(set: &Icons) -> Vec<(&'static str, &str)> {
    vec![
        ("favourite", &*set.favourite),
        ("new", &*set.new),
        ("ignored", &*set.ignored),
        ("snoozed", &*set.snoozed),
        ("branch", &*set.branch),
        ("detached", &*set.detached),
        ("detached_commits", &*set.detached_commits),
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
        ("pull", &*set.pull),
        ("worktree", &*set.worktree),
        ("submodule", &*set.submodule),
        ("spinner", &*set.spinner),
        ("marked", &*set.marked),
        ("ok", &*set.ok),
        ("failed", &*set.failed),
        ("celebrate", &*set.celebrate),
        ("issue_counted", &*set.issue_counted),
        ("issue_pinned", &*set.issue_pinned),
        ("issue_ignored", &*set.issue_ignored),
    ]
}

/// Symbols in all of Menlo, SF Mono, Inconsolata, DejaVu Sans Mono, Fira
/// Code, Hack and Source Code Pro (checked with fontTools).
const IN_ALL_FONTS: &str = "↑↓●×±≠…·—├└│»§•∞◊○▌†";

/// Known gaps, each to be judged with `argus demo --glyphs`.
const GAPS: &[(char, &str)] = &[
    ('◆', "missing in SF Mono (macOS Terminal's default)"),
    ('✓', "missing in Hack"),
    ('✗', "missing in Fira Code, Hack, Source Code Pro"),
];

/// Missing from most fonts, but the fallbacks fit the cell: braille
/// (Apple Braille, DejaVu Sans) and emoji (always the emoji font, two
/// cells, which width calculations count).
fn falls_back_cleanly(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c) || c == '🎉'
}

#[test]
fn unicode_icons_are_in_common_fonts_or_known_gaps() {
    for (name, icon) in icons(&UNICODE) {
        for c in icon.chars() {
            assert!(
                c.is_ascii()
                    || IN_ALL_FONTS.contains(c)
                    || GAPS.iter().any(|g| g.0 == c)
                    || falls_back_cleanly(c),
                "{name}: {c} (U+{:04X}) isn't in common fonts",
                c as u32
            );
        }
    }
    // inverse: the policy rejects a glyph most fonts lack
    assert!(!IN_ALL_FONTS.contains('★') && !falls_back_cleanly('★'));
    // and every listed gap is still in use
    let all: String = icons(&UNICODE).iter().map(|(_, i)| *i).collect();
    for (c, why) in GAPS {
        assert!(all.contains(*c), "{c} ({why}) is no longer used");
    }
}

#[test]
fn punctuation_is_in_all_fonts() {
    for c in icons::PUNCTUATION.chars() {
        assert!(IN_ALL_FONTS.contains(c), "{c}");
    }
}

#[test]
fn spinner_frames_cycle_one_character_at_a_time() {
    let ascii = ASCII;
    let frames: Vec<&str> = (0..5).map(|t| ascii.spin(t)).collect();
    assert_eq!(frames, ["|", "/", "-", "\\", "|"]);
    assert_eq!(UNICODE.spin(1), "⠙");
    assert_eq!(UNICODE.spin(10), UNICODE.spin(0));
    // inverse: no frames (an empty override) is a blank, not a panic
    let (i, _) = ASCII.with_overrides(&[("spinner".to_string(), String::new())].into());
    assert_eq!(i.spin(3), " ");
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
                // these need no Nerd glyph: the Unicode one fits
                [
                    "worktree",
                    "spinner",
                    "marked",
                    "celebrate",
                    "issue_counted",
                    "issue_pinned",
                    "issue_ignored"
                ]
                .contains(&name)
                    || nerd != uni
                    || uni.is_ascii(),
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
    for name in icons::ICON_NAMES {
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
        let looked_up: Vec<_> = icons::ICON_NAMES
            .iter()
            .map(|&n| (n, set.get(n).unwrap()))
            .collect();
        assert_eq!(looked_up, listed);
    }
    // inverse: unknown names
    assert_eq!(UNICODE.get("favourit"), None);
    assert_eq!(UNICODE.get(""), None);
}

#[test]
fn the_config_example_names_every_icon() {
    let example = argus_core::configfile::example();
    let doc: String = example
        .lines()
        .skip_while(|l| !l.contains("Per-icon overrides"))
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    for name in icons::ICON_NAMES {
        assert!(
            doc.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == *name),
            "{name} missing from: {doc}"
        );
    }
}

#[test]
fn the_pull_icon_is_overridden_by_its_new_name_only() {
    let (i, warnings) = ASCII.with_overrides(&[("pull".to_string(), "P".to_string())].into());
    assert_eq!((i.pull.as_ref(), warnings.len()), ("P", 0));
    let (i, warnings) = ASCII.with_overrides(&[("sync".to_string(), "S".to_string())].into());
    assert_eq!(i.pull, ASCII.pull);
    assert!(warnings[0].contains("unknown icon"), "{warnings:?}");
}
