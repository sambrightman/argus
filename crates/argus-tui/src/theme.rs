//! Themes. The default `terminal` theme uses only the 16 ANSI colours, so the
//! terminal's own colour scheme applies. Any role can be overridden from
//! config with a colour name, `#rrggbb` or a 256-colour index.

use std::collections::BTreeMap;
use std::str::FromStr;

use argus_core::badges::Role;
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ui {
    Selection,
    Marked,
    Header,
    Border,
    Dim,
    Key,
    Title,
    Name,
    Detail,
    StatusBar,
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    roles: BTreeMap<String, Style>,
}

const ROLE_NAMES: &[(Role, &str)] = &[
    (Role::Favourite, "favourite"),
    (Role::New, "new"),
    (Role::Hidden, "hidden"),
    (Role::Branch, "branch"),
    (Role::DefaultBranch, "default-branch"),
    (Role::Detached, "detached"),
    (Role::Ahead, "ahead"),
    (Role::Behind, "behind"),
    (Role::Staged, "staged"),
    (Role::Unstaged, "unstaged"),
    (Role::Untracked, "untracked"),
    (Role::Conflict, "conflict"),
    (Role::Stash, "stash"),
    (Role::Warning, "warning"),
    (Role::Error, "error"),
    (Role::Dim, "dim"),
];

const UI_NAMES: &[(Ui, &str)] = &[
    (Ui::Selection, "selection"),
    (Ui::Marked, "marked"),
    (Ui::Header, "header"),
    (Ui::Border, "border"),
    (Ui::Dim, "ui-dim"),
    (Ui::Key, "key"),
    (Ui::Title, "title"),
    (Ui::Name, "name"),
    (Ui::Detail, "detail"),
    (Ui::StatusBar, "status-bar"),
    (Ui::Running, "running"),
    (Ui::Ok, "ok"),
    (Ui::Failed, "failed"),
];

pub const BUILTIN: &[&str] = &["terminal", "dark", "light", "mono"];

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

impl Theme {
    pub fn role(&self, r: Role) -> Style {
        let name = ROLE_NAMES
            .iter()
            .find(|(x, _)| *x == r)
            .map(|(_, n)| *n)
            .unwrap_or("dim");
        self.roles.get(name).copied().unwrap_or_default()
    }

    pub fn ui(&self, u: Ui) -> Style {
        let name = UI_NAMES
            .iter()
            .find(|(x, _)| *x == u)
            .map(|(_, n)| *n)
            .unwrap_or("ui-dim");
        self.roles.get(name).copied().unwrap_or_default()
    }

    /// Known style keys, for documentation and validation.
    pub fn keys() -> impl Iterator<Item = &'static str> {
        ROLE_NAMES
            .iter()
            .map(|(_, n)| *n)
            .chain(UI_NAMES.iter().map(|(_, n)| *n))
    }

    /// Build a theme by name with overrides. Unknown names fall back to
    /// `terminal`; invalid overrides are returned as warnings.
    pub fn build(name: &str, overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        use Color::*;
        let b = Modifier::BOLD;
        let d = Modifier::DIM;
        let mut warnings = vec![];
        let base: Vec<(&str, Style)> = match name {
            "dark" => vec![
                ("favourite", fg(Rgb(0xe0, 0xaf, 0x68))),
                ("new", fg(Rgb(0x7d, 0xcf, 0xff)).add_modifier(b)),
                ("hidden", fg(Rgb(0x56, 0x5f, 0x89))),
                ("branch", fg(Rgb(0xbb, 0x9a, 0xf7))),
                ("default-branch", fg(Rgb(0x73, 0x7a, 0xa2))),
                ("detached", fg(Rgb(0xff, 0x9e, 0x64)).add_modifier(b)),
                ("ahead", fg(Rgb(0x9e, 0xce, 0x6a))),
                ("behind", fg(Rgb(0x7d, 0xcf, 0xff))),
                ("staged", fg(Rgb(0x9e, 0xce, 0x6a))),
                ("unstaged", fg(Rgb(0xe0, 0xaf, 0x68))),
                ("untracked", fg(Rgb(0x73, 0x7a, 0xa2))),
                ("conflict", fg(Rgb(0xf7, 0x76, 0x8e)).add_modifier(b)),
                ("stash", fg(Rgb(0x7a, 0xa2, 0xf7))),
                ("warning", fg(Rgb(0xff, 0x9e, 0x64))),
                ("error", fg(Rgb(0xf7, 0x76, 0x8e)).add_modifier(b)),
                ("dim", fg(Rgb(0x56, 0x5f, 0x89))),
                ("selection", Style::new().bg(Rgb(0x28, 0x34, 0x57))),
                ("marked", fg(Rgb(0xe0, 0xaf, 0x68)).add_modifier(b)),
                ("header", fg(Rgb(0x7a, 0xa2, 0xf7)).add_modifier(b)),
                ("border", fg(Rgb(0x3b, 0x42, 0x61))),
                ("ui-dim", fg(Rgb(0x56, 0x5f, 0x89))),
                ("key", fg(Rgb(0xbb, 0x9a, 0xf7)).add_modifier(b)),
                ("title", fg(Rgb(0xc0, 0xca, 0xf5)).add_modifier(b)),
                ("name", fg(Rgb(0xc0, 0xca, 0xf5))),
                ("detail", fg(Rgb(0xa9, 0xb1, 0xd6))),
                (
                    "status-bar",
                    Style::new()
                        .bg(Rgb(0x1f, 0x23, 0x35))
                        .fg(Rgb(0xa9, 0xb1, 0xd6)),
                ),
                ("running", fg(Rgb(0x7d, 0xcf, 0xff))),
                ("ok", fg(Rgb(0x9e, 0xce, 0x6a))),
                ("failed", fg(Rgb(0xf7, 0x76, 0x8e)).add_modifier(b)),
            ],
            "light" => vec![
                ("favourite", fg(Rgb(0x8f, 0x5e, 0x15))),
                ("new", fg(Rgb(0x00, 0x6f, 0x9f)).add_modifier(b)),
                ("hidden", fg(Rgb(0x9a, 0x9a, 0x9a))),
                ("branch", fg(Rgb(0x7c, 0x3a, 0xb8))),
                ("default-branch", fg(Rgb(0x80, 0x80, 0x90))),
                ("detached", fg(Rgb(0xb1, 0x50, 0x00)).add_modifier(b)),
                ("ahead", fg(Rgb(0x2d, 0x7d, 0x2d))),
                ("behind", fg(Rgb(0x00, 0x6f, 0x9f))),
                ("staged", fg(Rgb(0x2d, 0x7d, 0x2d))),
                ("unstaged", fg(Rgb(0x8f, 0x5e, 0x15))),
                ("untracked", fg(Rgb(0x80, 0x80, 0x90))),
                ("conflict", fg(Rgb(0xc0, 0x1c, 0x28)).add_modifier(b)),
                ("stash", fg(Rgb(0x1e, 0x50, 0xa0))),
                ("warning", fg(Rgb(0xb1, 0x50, 0x00))),
                ("error", fg(Rgb(0xc0, 0x1c, 0x28)).add_modifier(b)),
                ("dim", fg(Rgb(0x9a, 0x9a, 0x9a))),
                ("selection", Style::new().bg(Rgb(0xdd, 0xe4, 0xf0))),
                ("marked", fg(Rgb(0x8f, 0x5e, 0x15)).add_modifier(b)),
                ("header", fg(Rgb(0x1e, 0x50, 0xa0)).add_modifier(b)),
                ("border", fg(Rgb(0xc0, 0xc0, 0xc8))),
                ("ui-dim", fg(Rgb(0x9a, 0x9a, 0x9a))),
                ("key", fg(Rgb(0x7c, 0x3a, 0xb8)).add_modifier(b)),
                ("title", fg(Rgb(0x20, 0x20, 0x30)).add_modifier(b)),
                ("name", fg(Rgb(0x20, 0x20, 0x30))),
                ("detail", fg(Rgb(0x40, 0x40, 0x50))),
                (
                    "status-bar",
                    Style::new()
                        .bg(Rgb(0xe8, 0xe8, 0xee))
                        .fg(Rgb(0x40, 0x40, 0x50)),
                ),
                ("running", fg(Rgb(0x00, 0x6f, 0x9f))),
                ("ok", fg(Rgb(0x2d, 0x7d, 0x2d))),
                ("failed", fg(Rgb(0xc0, 0x1c, 0x28)).add_modifier(b)),
            ],
            "mono" => vec![
                ("favourite", Style::new().add_modifier(b)),
                ("new", Style::new().add_modifier(b | Modifier::UNDERLINED)),
                ("hidden", Style::new().add_modifier(d)),
                ("branch", Style::new().add_modifier(Modifier::ITALIC)),
                ("default-branch", Style::new().add_modifier(d)),
                ("detached", Style::new().add_modifier(b | Modifier::ITALIC)),
                (
                    "conflict",
                    Style::new().add_modifier(b | Modifier::REVERSED),
                ),
                ("error", Style::new().add_modifier(b | Modifier::REVERSED)),
                ("warning", Style::new().add_modifier(b)),
                ("untracked", Style::new().add_modifier(d)),
                ("dim", Style::new().add_modifier(d)),
                ("selection", Style::new().add_modifier(Modifier::REVERSED)),
                ("marked", Style::new().add_modifier(b)),
                ("header", Style::new().add_modifier(b)),
                ("ui-dim", Style::new().add_modifier(d)),
                ("key", Style::new().add_modifier(b)),
                ("title", Style::new().add_modifier(b)),
                ("status-bar", Style::new().add_modifier(Modifier::REVERSED)),
                ("failed", Style::new().add_modifier(b)),
            ],
            other => {
                if other != "terminal" {
                    warnings.push(format!("unknown theme {other:?}, using \"terminal\""));
                }
                vec![
                    ("favourite", fg(Yellow)),
                    ("new", fg(Cyan).add_modifier(b)),
                    ("hidden", fg(DarkGray)),
                    ("branch", fg(Magenta)),
                    ("default-branch", fg(DarkGray)),
                    ("detached", fg(Magenta).add_modifier(b)),
                    ("ahead", fg(Green)),
                    ("behind", fg(Cyan)),
                    ("staged", fg(Green)),
                    ("unstaged", fg(Yellow)),
                    ("untracked", fg(DarkGray)),
                    ("conflict", fg(Red).add_modifier(b)),
                    ("stash", fg(Blue)),
                    ("warning", fg(Red)),
                    ("error", fg(Red).add_modifier(b)),
                    ("dim", fg(DarkGray)),
                    ("selection", Style::new().add_modifier(Modifier::REVERSED)),
                    ("marked", fg(Yellow).add_modifier(b)),
                    ("header", fg(Blue).add_modifier(b)),
                    ("border", fg(DarkGray)),
                    ("ui-dim", fg(DarkGray)),
                    ("key", fg(Magenta).add_modifier(b)),
                    ("title", Style::new().add_modifier(b)),
                    ("name", Style::new()),
                    ("detail", Style::new()),
                    ("status-bar", Style::new().add_modifier(Modifier::REVERSED)),
                    ("running", fg(Cyan)),
                    ("ok", fg(Green)),
                    ("failed", fg(Red).add_modifier(b)),
                ]
            }
        };
        let mut roles: BTreeMap<String, Style> =
            base.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let known: Vec<&str> = Self::keys().collect();
        for (k, v) in overrides {
            if !known.contains(&k.as_str()) {
                warnings.push(format!("unknown colour key {k:?}"));
                continue;
            }
            match parse_style(v) {
                Some(s) => {
                    roles.insert(k.clone(), s);
                }
                None => warnings.push(format!("bad colour {v:?} for {k}")),
            }
        }
        (
            Self {
                name: name.to_string(),
                roles,
            },
            warnings,
        )
    }
}

/// `"red"`, `"#ff8800"`, `"208"`, optionally with modifiers and a background:
/// `"bold yellow"`, `"black on #ffcc00"`.
pub fn parse_style(s: &str) -> Option<Style> {
    let mut style = Style::new();
    let mut words = s.split_whitespace().peekable();
    let mut any = false;
    while let Some(w) = words.next() {
        any = true;
        match w {
            "bold" => style = style.add_modifier(Modifier::BOLD),
            "dim" => style = style.add_modifier(Modifier::DIM),
            "italic" => style = style.add_modifier(Modifier::ITALIC),
            "underline" | "underlined" => style = style.add_modifier(Modifier::UNDERLINED),
            "reverse" | "reversed" => style = style.add_modifier(Modifier::REVERSED),
            "on" => style = style.bg(Color::from_str(words.next()?).ok()?),
            c => style = style.fg(Color::from_str(c).ok()?),
        }
    }
    any.then_some(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_defines_every_role_it_needs() {
        for name in BUILTIN {
            let (t, w) = Theme::build(name, &BTreeMap::new());
            assert!(w.is_empty(), "{name}: {w:?}");
            // selection must be visible in every theme
            assert_ne!(t.ui(Ui::Selection), Style::default(), "{name}");
        }
    }

    #[test]
    fn overrides_apply_and_bad_ones_warn() {
        let o = BTreeMap::from([
            ("favourite".to_string(), "#ff0000".to_string()),
            ("branch".to_string(), "bold 208 on black".to_string()),
            ("nope".to_string(), "red".to_string()),
            ("ahead".to_string(), "notacolour".to_string()),
        ]);
        let (t, w) = Theme::build("terminal", &o);
        assert_eq!(t.role(Role::Favourite).fg, Some(Color::Rgb(255, 0, 0)));
        let br = t.role(Role::Branch);
        assert_eq!(
            (br.fg, br.bg),
            (Some(Color::Indexed(208)), Some(Color::Black))
        );
        assert!(br.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            t.role(Role::Ahead).fg,
            Some(Color::Green),
            "bad override leaves default"
        );
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn unknown_theme_falls_back() {
        let (t, w) = Theme::build("solarised-ultra", &BTreeMap::new());
        assert_eq!(t.role(Role::Ahead).fg, Some(Color::Green));
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn parse_style_rejects_empty_and_dangling_on() {
        assert!(parse_style("").is_none());
        assert!(parse_style("red on").is_none());
    }
}
