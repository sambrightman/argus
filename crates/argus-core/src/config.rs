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
    /// Directory names never descended into.
    pub skip_dirs: Vec<String>,
    /// Maximum directory depth below a root; deeper directories are skipped
    /// (with a warning).
    pub max_depth: usize,
    /// Maximum concurrent network operations (fetch/pull/push/clone).
    pub concurrency: usize,
    /// Protocol for new clones; unset asks the forge (`gh config get git_protocol`).
    #[schemars(example = crate::forge::Protocol::Ssh)]
    pub clone_protocol: Option<crate::forge::Protocol>,
    /// Pattern rules, applied in order; later rules win. Marks set from the
    /// TUI or `argus mark` override them.
    #[serde(rename = "rule")]
    pub rules: Vec<Rule>,
    /// Forge owners (orgs/users/teams) whose repos should all be cloned and
    /// synced. Their repos are syncable unless a rule says otherwise.
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
            skip_dirs: [
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
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            max_depth: 6,
            concurrency: 8,
            clone_protocol: None,
            rules: vec![],
            tracks: vec![],
            fix: FixConfig::default(),
            tools: Tools::default(),
            ui: UiConfig::default(),
        }
    }
}

/// A rule matches repositories by glob against any of:
/// identity (`github.com/acme/widget`), slug (`acme/widget`) or absolute path.
/// `*` does not cross `/`; use `**` for that.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Glob, e.g. `my-org/*`, `github.com/someone/dotfiles`, `~/dev/scratch/**`.
    #[serde(rename = "match")]
    #[schemars(example = &"my-org/important-*")]
    pub pattern: String,
    /// Sort above everything else (unset: leave as is).
    #[schemars(example = true)]
    pub favourite: Option<bool>,
    /// Pull on sync (unset: leave as is).
    #[schemars(example = true)]
    pub sync: Option<bool>,
    /// Hide and skip in bulk operations (unset: leave as is).
    #[schemars(example = true)]
    pub exclude: Option<bool>,
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
fn example_colors() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([("favourite", "yellow"), ("branch", "bold magenta")])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FixConfig {
    /// Fixes an auto-fix action may apply (one per action), in priority order.
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
    /// Show only repos needing attention at startup.
    pub attention_only: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "terminal".into(),
            colors: BTreeMap::new(),
            icons: "unicode".into(),
            attention_only: false,
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
    #[error("invalid rule pattern {pattern:?}: {source}")]
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
        self.concurrency = self.concurrency.max(1);
    }

    /// Directory new clones for `track` land in.
    pub fn clone_dir(&self, track: &Track) -> Option<PathBuf> {
        track
            .into
            .clone()
            .or_else(|| self.roots.first().map(|r| r.join(&track.owner)))
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
    fn concurrency_never_zero() {
        assert_eq!(Config::parse("concurrency = 0").unwrap().concurrency, 1);
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
