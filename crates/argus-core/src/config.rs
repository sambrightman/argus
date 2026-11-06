//! User configuration (hand-edited TOML). Machine-written data lives in
//! [`crate::state`] so this file is never rewritten behind the user's back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fix::FixKind;
use crate::paths::expand_tilde;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directories scanned (recursively) for repositories. When empty, the
    /// current directory is scanned. Manage with `argus root add|remove|list`.
    #[schemars(example = vec!["~/dev"])]
    pub roots: Vec<PathBuf>,
    /// Directories never descended into (roots are always scanned). Like
    /// gitignore: a pattern without `/` is a directory name at any depth
    /// (`node_modules`, `*.egg-info`); one starting with `/` or `~` is a full
    /// path (`~/dev/archive`, `~/src/*/third_party`); any other with `/` is
    /// below each root (`go/pkg`). Globs: `*` (within one level), `**`, `?`,
    /// `[ab]`, `{a,b}`. Unlike a rule with `exclude = true`, repos there
    /// aren't scanned at all.
    #[serde(alias = "skip_dirs")]
    pub skip: Vec<String>,
    /// Maximum directory depth below a root; deeper directories are skipped
    /// (with a warning). 0: no limit.
    pub max_depth: usize,
    /// Skip directories whose name starts with `.`; add one as a root to
    /// scan it.
    pub skip_hidden: bool,
    /// Follow symlinks to directories (`-L`). Cycles are cut and a repo
    /// reachable by several paths is listed once.
    pub follow_symlinks: bool,
    /// Maximum concurrent network operations (fetch/pull/push/clone). 0:
    /// automatic, i.e. git's `fetch.parallel` if set above 0, else the CPU
    /// count up to 8 (forges and ssh limit simultaneous connections).
    pub concurrency: usize,
    /// Protocol for new clones; unset asks the forge (`gh config get git_protocol`).
    #[schemars(example = crate::forge::Protocol::Ssh)]
    pub clone_protocol: Option<crate::forge::Protocol>,
    /// Repos sorted above others in their group. Each entry is a repo or a
    /// pattern, matched against its identity (`github.com/acme/app`),
    /// `owner/name` or path (`~/dev/x`); `*` stays within one level, `**`
    /// crosses them. Later entries win, and `!` turns it off again:
    /// `["acme/*", "!acme/legacy"]`. Marking from the TUI or `argus mark`
    /// edits these lists.
    #[schemars(example = vec!["acme/app", "~/dev/mine/*"])]
    pub favourite: Vec<String>,
    /// Repos pulled on sync; entries as for `favourite`. Tracked owners'
    /// repos are synced unless an entry here turns them off (`!`).
    #[schemars(example = vec!["acme/*", "!acme/huge-monorepo"])]
    pub sync: Vec<String>,
    /// Repos hidden and skipped by bulk operations; entries as for
    /// `favourite`.
    #[schemars(example = vec!["~/dev/archive/**"])]
    pub exclude: Vec<String>,
    /// Forge owners (orgs/users/teams) whose repos should all be cloned and
    /// synced. Their repos are syncable unless `sync` turns them off.
    #[serde(rename = "track")]
    pub tracks: Vec<Track>,
    /// Auto-fix behaviour.
    pub fix: FixConfig,
    /// External programs.
    pub tools: Tools,
    /// Appearance and startup view of the TUI.
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![],
            skip: [
                "node_modules",
                "target",
                ".venv",
                "venv",
                "__pycache__",
                ".tox",
                "vendor",
                "dist",
                "build",
                ".cache",
                ".direnv",
                ".terraform",
                "Library",
                "go/pkg",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_depth: 6,
            skip_hidden: true,
            follow_symlinks: false,
            concurrency: 0,
            clone_protocol: None,
            favourite: vec![],
            sync: vec![],
            exclude: vec![],
            tracks: vec![],
            fix: FixConfig::default(),
            tools: Tools::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum CloneMode {
    /// Clone repos missing locally during sync (they are flagged as new).
    #[default]
    Auto,
    /// Only report repos missing locally.
    Notify,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Track {
    /// Forge kind; only `github` so far.
    #[serde(default = "default_forge")]
    pub forge: String,
    /// Forge host (GitHub Enterprise: your host).
    #[serde(default = "default_host")]
    pub host: String,
    /// Org or user.
    #[schemars(example = &"my-org")]
    pub owner: String,
    /// Restrict to an org team's repositories (team slug).
    #[schemars(example = &"platform")]
    pub team: Option<String>,
    /// Where new clones go; unset means `<first root>/<owner>`.
    #[schemars(example = &"~/dev/my-org")]
    pub into: Option<PathBuf>,
    /// What sync does with repos missing locally.
    #[serde(default)]
    pub clone: CloneMode,
    /// Pull this owner's repos on sync.
    #[serde(default = "yes")]
    pub sync: bool,
    /// Also clone archived repos.
    #[serde(default)]
    pub include_archived: bool,
    /// Also clone forks.
    #[serde(default = "yes")]
    pub include_forks: bool,
}

impl Track {
    /// Stable key used in state.
    pub fn key(&self) -> String {
        match &self.team {
            Some(t) => format!("{}/{}#{}", self.host, self.owner, t),
            None => format!("{}/{}", self.host, self.owner),
        }
    }
}

fn default_forge() -> String {
    "github".into()
}
fn default_host() -> String {
    "github.com".into()
}
fn yes() -> bool {
    true
}
fn example_icon_overrides() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([("favourite", "★")])
}

fn example_colors() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([("favourite", "yellow"), ("branch", "bold magenta")])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FixConfig {
    /// Fixes an auto-fix action may apply, highest priority first. Each
    /// action applies one: the first in this list that the repo needs.
    pub auto: Vec<FixKind>,
}

impl Default for FixConfig {
    fn default() -> Self {
        Self {
            auto: vec![FixKind::Push, FixKind::SwitchDefault],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Tools {
    /// Shell for "drop to shell"; unset means `$SHELL`.
    #[schemars(example = &"/bin/zsh")]
    pub shell: Option<String>,
    /// Command for a full git UI, run in the repo dir, e.g. `lazygit` or
    /// `emacsclient -t -e '(magit-status)'`.
    #[schemars(example = &"lazygit")]
    pub git_ui: Option<String>,
    /// Editor for opening a repo; unset means `$VISUAL`, then `$EDITOR`.
    #[schemars(example = &"code --wait")]
    pub editor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Built-in theme name (`terminal`, `dark`, `light`, `mono`).
    pub theme: String,
    /// Per-role colour overrides: a name, `#rrggbb`, 0-255, optionally with
    /// modifiers and background, e.g. `"bold black on #ffcc00"`.
    #[schemars(example = example_colors())]
    pub colors: BTreeMap<String, String>,
    /// Icon set: `unicode` (default), `nerd` (Nerd Font glyphs) or `ascii`.
    pub icons: String,
    /// Per-icon overrides on top of the set, by name: favourite, new,
    /// excluded, snoozed, branch, detached, detached_commits, ahead, behind, staged, unstaged,
    /// untracked, conflict, stash, in_progress, gone, no_upstream, branches,
    /// error, sync, worktree, submodule; and in the interface: spinner (one
    /// frame per character), marked, ok, failed, celebrate.
    #[schemars(example = example_icon_overrides())]
    pub icon_overrides: BTreeMap<String, String>,
    /// Show only repos needing attention at startup (`a` toggles).
    pub attention_only: bool,
}

impl UiConfig {
    /// The icon set with overrides applied, plus warnings for unknown names.
    pub fn icons(&self) -> (crate::icons::Icons, Vec<String>) {
        crate::icons::by_name(&self.icons).with_overrides(&self.icon_overrides)
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "terminal".into(),
            colors: BTreeMap::new(),
            icons: "unicode".into(),
            icon_overrides: BTreeMap::new(),
            attention_only: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        source: globset::Error,
    },
}

impl Config {
    /// Load from `path`; a missing file yields the default config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::parse(&s).map_err(|source| ConfigError::Parse {
                path: path.into(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.into(),
                source,
            }),
        }
    }

    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        let mut c: Config = toml::from_str(s)?;
        c.normalize();
        Ok(c)
    }

    fn normalize(&mut self) {
        for r in &mut self.roots {
            *r = expand_tilde(r);
        }
        for t in &mut self.tracks {
            if let Some(p) = &mut t.into {
                *p = expand_tilde(p);
            }
        }
    }

    /// How many network operations to run at once: see `concurrency`.
    pub fn network_jobs(&self) -> usize {
        if self.concurrency > 0 {
            return self.concurrency;
        }
        let fetch_parallel = gix::config::File::from_globals()
            .ok()
            .and_then(|f| f.integer("fetch.parallel").ok().flatten());
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        auto_jobs(fetch_parallel, cpus)
    }

    /// Directory new clones for `track` land in.
    pub fn clone_dir(&self, track: &Track) -> Option<PathBuf> {
        track
            .into
            .clone()
            .or_else(|| self.roots.first().map(|r| r.join(&track.owner)))
    }
}

/// Upper bound for automatic concurrency: OpenSSH allows 10 sessions per
/// multiplexed connection by default, and forges throttle parallel clones.
const AUTO_MAX_JOBS: usize = 8;

/// Automatic concurrency: git's `fetch.parallel` when positive (0 there
/// also means "pick a default"), otherwise the CPU count up to
/// [`AUTO_MAX_JOBS`].
pub fn auto_jobs(fetch_parallel: Option<i64>, cpus: usize) -> usize {
    match fetch_parallel {
        Some(n) if n > 0 => n as usize,
        _ => cpus.clamp(1, AUTO_MAX_JOBS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Config::parse("rootz = []").is_err());
        assert!(
            Config::parse("[[rule]]\nmatch='x'\nfavorite=true").is_err(),
            "US spelling is a typo here"
        );
    }

    #[test]
    fn track_defaults_and_key() {
        let c = Config::parse("roots=['/r']\n[[track]]\nowner='acme'\n[[track]]\nowner='acme'\nteam='core'\nclone='notify'\nsync=false").unwrap();
        assert_eq!(c.tracks[0].host, "github.com");
        assert_eq!(c.tracks[0].clone, CloneMode::Auto);
        assert!(c.tracks[0].sync);
        assert_eq!(c.tracks[0].key(), "github.com/acme");
        assert_eq!(c.tracks[1].key(), "github.com/acme#core");
        assert_eq!(c.tracks[1].clone, CloneMode::Notify);
        assert!(!c.tracks[1].sync);
        assert_eq!(c.clone_dir(&c.tracks[0]), Some(PathBuf::from("/r/acme")));
    }

    #[test]
    fn concurrency_zero_is_automatic() {
        assert_eq!(Config::default().concurrency, 0, "automatic by default");
        assert_eq!(Config::parse("concurrency = 3").unwrap().network_jobs(), 3);
        assert!(Config::default().network_jobs() >= 1);
        // git's fetch.parallel when positive (0 there also means "pick one"),
        // else the CPU count, capped for forge/ssh connection limits
        for (fetch_parallel, cpus, want) in [
            (None, 4, 4),
            (None, 32, 8),
            (Some(0), 32, 8),
            (Some(-1), 2, 2),
            (Some(20), 4, 20),
            (None, 0, 1),
        ] {
            assert_eq!(
                auto_jobs(fetch_parallel, cpus),
                want,
                "{fetch_parallel:?} {cpus}"
            );
        }
    }

    #[test]
    fn missing_file_is_default() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::load(&d.path().join("nope.toml")).unwrap(),
            Config::default()
        );
    }
}
