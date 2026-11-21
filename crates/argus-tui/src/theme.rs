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
    /// The rows a region (`C-space`) spans: a background, kept off what
    /// has one of its own.
    Region,
    Marked,
    /// A repo marked for a batch action: its name, and the mark before it.
    MarkedRepo,
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
    /// Each tag's, in config order (see [`Theme::with_tags`]).
    tags: Vec<Style>,
}

const ROLE_NAMES: &[(Role, &str)] = &[
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
    (Role::DiffAdd, "diff-add"),
    (Role::DiffRemove, "diff-remove"),
    (Role::DiffHunk, "diff-hunk"),
];

const UI_NAMES: &[(Ui, &str)] = &[
    (Ui::Selection, "selection"),
    (Ui::Region, "region"),
    (Ui::Marked, "marked"),
    (Ui::MarkedRepo, "marked_repo"),
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
        if let Role::Tag(i) = r {
            return self
                .tags
                .get(i)
                .copied()
                .unwrap_or_else(|| self.roles.get("tag").copied().unwrap_or_default());
        }
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

    /// Known style keys, for documentation and validation (and
    /// `tag.<name>` for each tag).
    pub fn keys() -> impl Iterator<Item = &'static str> {
        ROLE_NAMES
            .iter()
            .map(|(_, n)| *n)
            .chain(["tag", "pull"])
            .chain(UI_NAMES.iter().map(|(_, n)| *n))
    }

    /// The theme `c` names, with its colour overrides and its tags', and
    /// the warnings of both.
    pub fn for_config(c: &argus_core::Config) -> (Self, Vec<String>) {
        let (t, mut w) = Self::build(&c.ui.theme, &c.ui.colors);
        let (t, more) = t.with_tags(c);
        w.extend(more);
        (t, w)
    }

    /// Each tag's style, in config order: `[ui] colors` naming it
    /// (`tag.<name>`, applied by [`Theme::build`]), else this theme's own for it, else
    /// its `color`, else the theme's `tag`. Returns warnings: a `tag.`
    /// colour for no tag, a bad colour.
    pub fn with_tags(mut self, config: &argus_core::Config) -> (Self, Vec<String>) {
        let tags = &config.tags;
        let mut warnings: Vec<String> = config
            .ui
            .colors
            .keys()
            .filter_map(|k| k.strip_prefix("tag."))
            .filter(|n| !tags.contains_key(*n))
            .map(|n| format!("colour for tag.{n}, which isn't a tag"))
            .collect();
        let role = |name: &str| self.roles.get(name).copied();
        self.tags = tags
            .iter()
            .map(|(name, def)| {
                if let Some(s) = self.roles.get(&format!("tag.{name}")) {
                    return *s;
                }
                // pull has a colour of its own in every theme (`pull`),
                // which `[ui] colors` setting it puts before the tag's
                let pull = (name == argus_core::config::PULL).then_some("pull");
                let fallback = pull.and_then(role).or(role("tag")).unwrap_or_default();
                if pull.is_some_and(|p| config.ui.colors.contains_key(p)) {
                    return fallback;
                }
                match def.color.as_deref().map(|c| (c, parse_style(c))) {
                    Some((_, Some(s))) => s,
                    Some((c, None)) => {
                        warnings.push(format!("bad colour {c:?} for tag {name}"));
                        fallback
                    }
                    None => fallback,
                }
            })
            .collect();
        if tags[argus_core::config::IGNORE].color.is_some() {
            warnings.push(
                "[tags.ignore] color isn't used: ignored repos take [ui] colors hidden".into(),
            );
        }
        (self, warnings)
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
                ("tag", fg(Rgb(0xe0, 0xaf, 0x68))),
                ("tag.favourite", fg(Rgb(0xe0, 0xaf, 0x68))),
                ("new", fg(Rgb(0x7d, 0xcf, 0xff)).add_modifier(b)),
                ("hidden", fg(Rgb(0x56, 0x5f, 0x89))),
                ("pull", fg(Rgb(0x73, 0x7a, 0xa2))),
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
                ("diff-add", fg(Rgb(0x9e, 0xce, 0x6a))),
                ("diff-remove", fg(Rgb(0xf7, 0x76, 0x8e))),
                ("diff-hunk", fg(Rgb(0x7a, 0xa2, 0xf7))),
                ("selection", Style::new().bg(Rgb(0x28, 0x34, 0x57))),
                ("region", Style::new().bg(Rgb(0x1f, 0x23, 0x35))),
                ("marked", fg(Rgb(0xe0, 0xaf, 0x68)).add_modifier(b)),
                ("marked_repo", fg(Rgb(0x7d, 0xcf, 0xff)).add_modifier(b)),
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
                ("tag", fg(Rgb(0x8f, 0x5e, 0x15))),
                ("tag.favourite", fg(Rgb(0x8f, 0x5e, 0x15))),
                ("new", fg(Rgb(0x00, 0x6f, 0x9f)).add_modifier(b)),
                ("hidden", fg(Rgb(0x9a, 0x9a, 0x9a))),
                ("pull", fg(Rgb(0x80, 0x80, 0x90))),
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
                ("diff-add", fg(Rgb(0x2d, 0x7d, 0x2d))),
                ("diff-remove", fg(Rgb(0xc0, 0x1c, 0x28))),
                ("diff-hunk", fg(Rgb(0x1e, 0x50, 0xa0))),
                ("selection", Style::new().bg(Rgb(0xdd, 0xe4, 0xf0))),
                ("region", Style::new().bg(Rgb(0xee, 0xf1, 0xf7))),
                ("marked", fg(Rgb(0x8f, 0x5e, 0x15)).add_modifier(b)),
                ("marked_repo", fg(Rgb(0x0f, 0x4b, 0x6e)).add_modifier(b)),
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
                ("tag", Style::new().add_modifier(b)),
                ("tag.favourite", Style::new().add_modifier(b)),
                ("new", Style::new().add_modifier(b | Modifier::UNDERLINED)),
                ("hidden", Style::new().add_modifier(d)),
                ("pull", Style::new().add_modifier(d)),
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
                ("diff-add", Style::new().add_modifier(b)),
                ("diff-remove", Style::new().add_modifier(Modifier::ITALIC)),
                ("diff-hunk", Style::new().add_modifier(Modifier::UNDERLINED)),
                ("selection", Style::new().add_modifier(Modifier::REVERSED)),
                ("region", Style::new().add_modifier(Modifier::UNDERLINED)),
                ("marked", Style::new().add_modifier(b)),
                (
                    "marked_repo",
                    Style::new().add_modifier(b | Modifier::UNDERLINED),
                ),
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
                    ("tag", fg(Yellow)),
                    ("tag.favourite", fg(Yellow)),
                    ("new", fg(Cyan).add_modifier(b)),
                    ("hidden", fg(DarkGray)),
                    ("pull", fg(Blue)),
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
                    ("diff-add", fg(Green)),
                    ("diff-remove", fg(Red)),
                    ("diff-hunk", fg(Cyan)),
                    ("selection", Style::new().add_modifier(Modifier::REVERSED)),
                    ("region", Style::new().bg(Indexed(236))),
                    ("marked", fg(Yellow).add_modifier(b)),
                    ("marked_repo", fg(Cyan).add_modifier(b)),
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
            // a tag's: checked against the tags by with_tags
            if !known.contains(&k.as_str()) && !k.starts_with("tag.") {
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
                tags: vec![],
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
            // selection must be visible in every theme, and a region
            // (the cursor's row is its end) apart from it
            assert_ne!(t.ui(Ui::Selection), Style::default(), "{name}");
            assert_ne!(t.ui(Ui::Region), Style::default(), "{name}");
            assert_ne!(t.ui(Ui::Region), t.ui(Ui::Selection), "{name}");
        }
    }

    /// A tag's colour: `[ui] colors` naming it, else the theme's own,
    /// else its `color`, else the theme's for tags.
    #[test]
    fn tags_take_colours_from_config_then_theme_then_their_own() {
        let c = argus_core::Config::parse(
            "[tags.work]\ncolor = 'cyan'\n[tags.plain]\n[tags.bad]\ncolor = 'nocolour'\n\
             [tags.mine]\ncolor = 'cyan'\n\
             [ui.colors]\n'tag.mine' = 'magenta'\n'tag.ghost' = 'red'\n",
        )
        .unwrap();
        let (t, w) = Theme::build("terminal", &c.ui.colors);
        assert!(w.is_empty(), "{w:?}");
        let (t, w) = t.with_tags(&c);
        let fg = |name: &str| t.role(Role::Tag(c.tags.get_index_of(name).unwrap())).fg;
        assert_eq!(fg("favourite"), Some(Color::Yellow), "the theme's own");
        assert_eq!(fg("work"), Some(Color::Cyan), "work: its own");
        assert_eq!(fg("plain"), t.roles["tag"].fg, "the theme's for tags");
        assert_eq!(fg("bad"), t.roles["tag"].fg, "bad: the same, warned");
        assert_eq!(fg("mine"), Some(Color::Magenta), "mine: config first");
        // pull shows in its own role: its tag's colour is that role's,
        // unless `[ui] colors` sets the role (or names the tag)
        let pull = |cfg: &str| {
            let c = argus_core::Config::parse(cfg).unwrap();
            let (t, w) = Theme::build("terminal", &c.ui.colors).0.with_tags(&c);
            (
                t.role(Role::Tag(c.tags.get_index_of("pull").unwrap())).fg,
                w,
            )
        };
        let own = pull("").0;
        assert_ne!(own, Some(Color::Red));
        assert_eq!(pull("[tags.pull]\ncolor = 'red'\n").0, Some(Color::Red));
        assert_eq!(
            pull("[tags.pull]\ncolor = 'red'\n[ui.colors]\npull = 'green'\n").0,
            Some(Color::Green)
        );
        assert_eq!(
            pull("[ui.colors]\npull = 'green'\n'tag.pull' = 'red'\n").0,
            Some(Color::Red)
        );
        // an ignored repo's colour is the hidden role's: said, not dropped
        let said = pull("[tags.ignore]\ncolor = 'red'\n").1;
        assert!(
            said.len() == 1 && said[0].contains("[ui] colors hidden"),
            "{said:?}"
        );
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|w| w.contains("ghost")) && w.iter().any(|w| w.contains("nocolour")));
    }

    #[test]
    fn each_ui_element_gets_its_own_style() {
        let (t, _) = Theme::build("mono", &BTreeMap::new());
        assert_eq!(
            t.ui(Ui::Selection),
            Style::new().add_modifier(Modifier::REVERSED)
        );
        assert_ne!(t.ui(Ui::Header), t.ui(Ui::Selection));
    }

    #[test]
    fn overrides_apply_and_bad_ones_warn() {
        let o = BTreeMap::from([
            ("tag".to_string(), "#ff0000".to_string()),
            ("branch".to_string(), "bold 208 on black".to_string()),
            ("nope".to_string(), "red".to_string()),
            ("ahead".to_string(), "notacolour".to_string()),
        ]);
        let (t, w) = Theme::build("terminal", &o);
        assert_eq!(t.role(Role::Tag(0)).fg, Some(Color::Rgb(255, 0, 0)));
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
