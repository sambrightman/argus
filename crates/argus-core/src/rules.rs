//! Resolution of per-repo flags from tracked owners, the `favourite`, `sync`
//! and `exclude` lists in config, and state (snoozes, "new").

use std::path::Path;

use globset::GlobMatcher;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigError, Track};
use crate::identity::RepoId;
use crate::paths::expand_tilde;
use crate::state::State;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    pub favourite: bool,
    pub sync: bool,
    pub excluded: bool,
    pub snoozed: bool,
    /// Recently cloned/discovered by argus and not acknowledged.
    pub new: bool,
    /// Key of the tracked owner this repo belongs to.
    pub tracked: Option<String>,
}

impl Flags {
    /// Hidden from the default view and skipped by bulk operations.
    pub fn hidden(&self) -> bool {
        self.excluded || self.snoozed
    }
}

/// The per-repo settings kept as lists in config: `favourite`, `sync`,
/// `exclude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mark {
    Favourite,
    Sync,
    Exclude,
}

impl Mark {
    pub const ALL: [Mark; 3] = [Mark::Favourite, Mark::Sync, Mark::Exclude];

    /// The config key.
    pub fn name(self) -> &'static str {
        match self {
            Mark::Favourite => "favourite",
            Mark::Sync => "sync",
            Mark::Exclude => "exclude",
        }
    }

    pub fn list(self, c: &Config) -> &Vec<String> {
        match self {
            Mark::Favourite => &c.favourite,
            Mark::Sync => &c.sync,
            Mark::Exclude => &c.exclude,
        }
    }

    pub fn list_mut(self, c: &mut Config) -> &mut Vec<String> {
        match self {
            Mark::Favourite => &mut c.favourite,
            Mark::Sync => &mut c.sync,
            Mark::Exclude => &mut c.exclude,
        }
    }

    pub fn of(self, f: &Flags) -> bool {
        match self {
            Mark::Favourite => f.favourite,
            Mark::Sync => f.sync,
            Mark::Exclude => f.excluded,
        }
    }
}

/// One list entry: a repo or a pattern, `!` to turn the mark off.
#[derive(Clone)]
struct Entry {
    text: String,
    negate: bool,
    /// No glob characters: names one repo (identity, `owner/name` or path).
    literal: bool,
    matcher: GlobMatcher,
}

impl Entry {
    fn new(text: &str) -> Result<Self, ConfigError> {
        let (negate, pattern) = match text.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, text),
        };
        let pattern = expand_tilde(Path::new(pattern)).display().to_string();
        let matcher = globset::GlobBuilder::new(&pattern)
            .literal_separator(true)
            .case_insensitive(true)
            .build()
            .map_err(|source| ConfigError::Pattern {
                pattern: text.to_string(),
                source,
            })?
            .compile_matcher();
        Ok(Self {
            text: text.to_string(),
            negate,
            literal: !pattern.contains(['*', '?', '[', '{']),
            matcher,
        })
    }

    /// Matches the repo's identity (`github.com/acme/app`), `owner/name`, or
    /// path.
    fn matches(&self, id: Option<&RepoId>, path: &Path) -> bool {
        id.is_some_and(|i| self.matcher.is_match(i.to_string()) || self.matcher.is_match(i.slug()))
            || self.matcher.is_match(path)
    }
}

/// The `favourite`, `sync` and `exclude` lists, compiled.
#[derive(Clone)]
pub struct RuleSet {
    lists: [Vec<Entry>; 3],
}

impl RuleSet {
    pub fn new(config: &Config) -> Result<Self, ConfigError> {
        let compile = |m: Mark| -> Result<Vec<Entry>, ConfigError> {
            m.list(config).iter().map(|t| Entry::new(t)).collect()
        };
        Ok(Self {
            lists: [
                compile(Mark::Favourite)?,
                compile(Mark::Sync)?,
                compile(Mark::Exclude)?,
            ],
        })
    }

    fn entries(&self, m: Mark) -> &[Entry] {
        &self.lists[m as usize]
    }

    /// What `m`'s list says about the repo: its last matching entry, if any.
    pub fn decide(&self, m: Mark, id: Option<&RepoId>, path: &Path) -> Option<bool> {
        self.entries(m)
            .iter()
            .rev()
            .find(|e| e.matches(id, path))
            .map(|e| !e.negate)
    }

    /// Whether a (possibly not yet cloned) repo is excluded.
    pub fn excludes(&self, id: &RepoId, path: &Path) -> bool {
        self.decide(Mark::Exclude, Some(id), path).unwrap_or(false)
    }

    /// Entries of `m`'s list naming this repo itself (not patterns), as
    /// written, `!` included.
    pub fn own_entries(&self, m: Mark, id: Option<&RepoId>, path: &Path) -> Vec<String> {
        self.entries(m)
            .iter()
            .filter(|e| e.literal && e.matches(id, path))
            .map(|e| e.text.clone())
            .collect()
    }
}

/// Edit `m`'s list in `config` so the repo ends up `on`: drop the entries
/// naming it, then add `name` (or `!name`) only if what's left (patterns,
/// or `base` when nothing matches) doesn't already give `on`.
pub fn set_mark(
    config: &mut Config,
    m: Mark,
    id: Option<&RepoId>,
    path: &Path,
    name: &str,
    base: bool,
    on: bool,
) -> Result<(), ConfigError> {
    clear_mark(config, m, id, path)?;
    let now = RuleSet::new(config)?.decide(m, id, path).unwrap_or(base);
    if now != on {
        let entry = if on {
            name.to_string()
        } else {
            format!("!{name}")
        };
        m.list_mut(config).push(entry);
    }
    Ok(())
}

/// Drop the entries of `m`'s list naming the repo, so patterns decide.
pub fn clear_mark(
    config: &mut Config,
    m: Mark,
    id: Option<&RepoId>,
    path: &Path,
) -> Result<(), ConfigError> {
    let own = RuleSet::new(config)?.own_entries(m, id, path);
    m.list_mut(config).retain(|e| !own.contains(e));
    Ok(())
}

/// The tracked owner a repo belongs to (first match), using the last forge
/// listing for team-restricted tracks.
pub fn track_for<'a>(config: &'a Config, state: &State, id: &RepoId) -> Option<&'a Track> {
    config.tracks.iter().find(|t| {
        if !t.host.eq_ignore_ascii_case(&id.host) || !t.owner.eq_ignore_ascii_case(&id.owner) {
            return false;
        }
        match &t.team {
            None => true,
            Some(_) => state.tracks.get(&t.key()).is_some_and(|ts| {
                ts.repos
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(&id.to_string()))
            }),
        }
    })
}

/// Precedence (lowest to highest): track membership (sync), then each list's
/// last matching entry. Snoozes and "new" come from state.
pub fn resolve(
    config: &Config,
    rules: &RuleSet,
    state: &State,
    id: Option<&RepoId>,
    key: &str,
    path: &Path,
    now: Timestamp,
) -> Flags {
    let mut f = Flags::default();
    if let Some(t) = id.and_then(|id| track_for(config, state, id)) {
        f.sync = t.sync;
        f.tracked = Some(t.key());
    }
    for m in Mark::ALL {
        if let Some(v) = rules.decide(m, id, path) {
            match m {
                Mark::Favourite => f.favourite = v,
                Mark::Sync => f.sync = v,
                Mark::Exclude => f.excluded = v,
            }
        }
    }
    f.snoozed = state.is_snoozed(key, now);
    f.new = state.new.contains(key);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TrackState;

    fn now() -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn flags(cfg: &str, state: &State, url: &str, path: &str) -> Flags {
        let c = Config::parse(cfg).unwrap();
        let rs = RuleSet::new(&c).unwrap();
        let id = RepoId::from_url(url);
        let key = id
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or(path.to_string());
        resolve(&c, &rs, state, id.as_ref(), &key, Path::new(path), now())
    }

    fn fav(cfg: &str, url: &str, path: &str) -> bool {
        flags(cfg, &State::default(), url, path).favourite
    }

    const URL: &str = "git@github.com:acme/widget.git";

    #[test]
    fn nothing_configured_means_no_flags() {
        assert_eq!(
            flags("", &State::default(), URL, "/r/widget"),
            Flags::default()
        );
    }

    #[test]
    fn entries_match_identity_slug_and_path() {
        assert!(fav("favourite = ['github.com/acme/*']", URL, "/x"));
        assert!(fav("favourite = ['acme/widget']", URL, "/x"));
        assert!(fav("favourite = ['/r/**']", URL, "/r/a/widget"));
        assert!(fav("favourite = ['ACME/*']", URL, "/x"), "case-insensitive");
        // inverse: other owners, and `*` must not cross `/`
        assert!(!fav("favourite = ['github.com/other/*']", URL, "/x"));
        assert!(!fav("favourite = ['github.com/*']", URL, "/x"));
        assert!(!fav("favourite = ['/r/*']", URL, "/r/a/widget"));
    }

    #[test]
    fn later_entries_win_and_bang_turns_off() {
        let cfg = "sync = ['acme/*', '!acme/widget']";
        assert!(!flags(cfg, &State::default(), URL, "/x").sync);
        assert!(
            flags(
                cfg,
                &State::default(),
                "git@github.com:acme/other.git",
                "/x"
            )
            .sync
        );
        // order matters: a later pattern turns it back on
        assert!(fav("favourite = ['!acme/widget', 'acme/*']", URL, "/x"));
        // the lists are independent
        let f = flags(
            "favourite = ['acme/*']\nexclude = ['!acme/*']",
            &State::default(),
            URL,
            "/x",
        );
        assert!(f.favourite && !f.excluded && !f.sync);
    }

    #[test]
    fn tracked_owner_implies_sync_unless_overridden() {
        let s = State::default();
        let f = flags("[[track]]\nowner='acme'", &s, URL, "/x");
        assert!(f.sync);
        assert_eq!(f.tracked.as_deref(), Some("github.com/acme"));
        assert!(!flags("[[track]]\nowner='acme'\nsync=false", &s, URL, "/x").sync);
        assert!(
            !flags(
                "sync = ['!acme/widget']\n[[track]]\nowner='acme'",
                &s,
                URL,
                "/x"
            )
            .sync
        );
        assert_eq!(
            flags("[[track]]\nowner='other'", &s, URL, "/x").tracked,
            None
        );
        assert_eq!(
            flags("[[track]]\nowner='acme'\nhost='gitlab.com'", &s, URL, "/x").tracked,
            None
        );
    }

    #[test]
    fn team_track_uses_last_listing() {
        let cfg = "[[track]]\nowner='acme'\nteam='core'";
        assert_eq!(
            flags(cfg, &State::default(), URL, "/x").tracked,
            None,
            "unknown membership"
        );
        let mut s = State::default();
        s.tracks.insert(
            "github.com/acme#core".into(),
            TrackState {
                repos: ["github.com/acme/widget".to_string()].into(),
                listed_at: None,
            },
        );
        assert!(flags(cfg, &s, URL, "/x").sync);
        assert!(!flags(cfg, &s, "git@github.com:acme/notinteam.git", "/x").sync);
    }

    #[test]
    fn snooze_and_new_from_state() {
        let mut s = State::default();
        s.snoozed.insert(
            "github.com/acme/widget".into(),
            "2026-02-01T00:00:00Z".parse().unwrap(),
        );
        s.new.insert("github.com/acme/widget".into());
        let f = flags("", &s, URL, "/x");
        assert!(f.snoozed && f.new && f.hidden());
        s.snoozed.insert(
            "github.com/acme/widget".into(),
            "2025-12-01T00:00:00Z".parse().unwrap(),
        );
        assert!(!flags("", &s, URL, "/x").snoozed, "expired snooze");
    }

    #[test]
    fn repos_without_identity_match_by_path_only() {
        let f = flags(
            "exclude = ['/local/**']",
            &State::default(),
            "",
            "/local/thing",
        );
        assert!(f.excluded);
    }

    #[test]
    fn each_mark_reads_its_own_flag() {
        let f = |favourite, sync, excluded| Flags {
            favourite,
            sync,
            excluded,
            ..Flags::default()
        };
        assert!(Mark::Favourite.of(&f(true, false, false)));
        assert!(Mark::Sync.of(&f(false, true, false)));
        assert!(Mark::Exclude.of(&f(false, false, true)));
        // inverse: not the others'
        for m in Mark::ALL {
            assert!(!m.of(&f(false, false, false)), "{m:?}");
        }
        assert!(!Mark::Favourite.of(&f(false, true, true)));
        assert!(!Mark::Sync.of(&f(true, false, true)));
        assert!(!Mark::Exclude.of(&f(true, true, false)));
    }

    #[test]
    fn bad_patterns_are_config_errors() {
        let c = Config::parse("favourite = ['acme/[oops']").unwrap();
        let Err(e) = RuleSet::new(&c) else {
            panic!("accepted")
        };
        assert!(e.to_string().contains("[oops"), "{e}");
    }

    /// `set_mark` on the widget, returning the favourite list after.
    fn set(list: &[&str], on: bool) -> Vec<String> {
        let mut c = Config {
            favourite: list.iter().map(|s| s.to_string()).collect(),
            ..Config::default()
        };
        let id = RepoId::from_url(URL).unwrap();
        set_mark(
            &mut c,
            Mark::Favourite,
            Some(&id),
            Path::new("/r/widget"),
            "github.com/acme/widget",
            false,
            on,
        )
        .unwrap();
        c.favourite
    }

    #[test]
    fn marking_adds_or_removes_the_repo_itself_only() {
        // on: added once; already on: unchanged
        assert_eq!(set(&[], true), ["github.com/acme/widget"]);
        assert_eq!(
            set(&["acme/widget"], true),
            ["github.com/acme/widget"],
            "one spelling"
        );
        assert_eq!(
            set(&["acme/*"], true),
            ["acme/*"],
            "a pattern already says so"
        );
        // off: its own entries go; a pattern still matching gets a `!`
        assert_eq!(set(&["acme/widget", "other/x"], false), ["other/x"]);
        assert_eq!(
            set(&["acme/*"], false),
            ["acme/*", "!github.com/acme/widget"]
        );
        assert_eq!(set(&["acme/*", "!acme/widget"], true), ["acme/*"]);
        // spellings by identity, owner/name and path all count as its own
        assert_eq!(
            set(
                &["/r/widget", "ACME/Widget", "github.com/acme/widget"],
                false
            ),
            Vec::<String>::new()
        );
        // inverse: patterns and other repos are never touched
        assert_eq!(
            set(&["acme/wid*", "acme/widget2"], false),
            ["acme/wid*", "acme/widget2", "!github.com/acme/widget"]
        );
    }

    #[test]
    fn marking_off_a_tracked_repo_needs_a_bang() {
        let mut c = Config::parse("[[track]]\nowner='acme'").unwrap();
        let id = RepoId::from_url(URL).unwrap();
        let p = Path::new("/r/widget");
        set_mark(
            &mut c,
            Mark::Sync,
            Some(&id),
            p,
            "github.com/acme/widget",
            true,
            false,
        )
        .unwrap();
        assert_eq!(c.sync, ["!github.com/acme/widget"]);
        set_mark(
            &mut c,
            Mark::Sync,
            Some(&id),
            p,
            "github.com/acme/widget",
            true,
            true,
        )
        .unwrap();
        assert!(c.sync.is_empty());
        // clearing drops its own entries only
        c.sync = vec!["acme/*".into(), "!acme/widget".into()];
        clear_mark(&mut c, Mark::Sync, Some(&id), p).unwrap();
        assert_eq!(c.sync, ["acme/*"]);
    }
}
