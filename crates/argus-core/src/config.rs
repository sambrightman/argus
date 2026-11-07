//! User configuration (hand-edited TOML). Machine-written data lives in
//! [`crate::state`] so this file is never rewritten behind the user's back.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fix::FixKind;
use crate::paths::expand_tilde;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Other config files to read first, e.g. `["~/.config/argus/work.toml",
    /// "conf.d/*.toml"]`: paths relative to this file, `~` allowed, `*` etc.
    /// in the file name (matching nothing is fine). This file's settings
    /// override theirs; lists of tables (`[[track]]`) add up. Any list
    /// setting also takes `extend_<name>` to add to the list rather than
    /// replace it, e.g. `extend_skip = ["build-cache"]`.
    pub include: Vec<String>,
    /// Directories scanned (recursively) for repositories. When empty, the
    /// current directory is scanned. Manage with `argus root add|remove|list`.
    #[schemars(example = vec!["~/dev"])]
    pub roots: Vec<PathBuf>,
    /// Directories never descended into (roots are always scanned). Like
    /// gitignore: a pattern without `/` is a directory name at any depth
    /// (`node_modules`, `*.egg-info`); one starting with `/` or `~` is a full
    /// path (`~/dev/archive`, `~/src/*/third_party`); any other with `/` is
    /// below each root (`go/pkg`). Globs: `*` (within one level), `**`, `?`,
    /// `[ab]`, `{a,b}`. Unlike `exclude`, which hides repos, repos there
    /// aren't scanned at all.
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
    /// How much runs at once.
    pub concurrency: Concurrency,
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
    /// Defaults for stashing: the `z` menu, `argus stash`, the stash fix.
    pub stash: StashConfig,
    /// External programs.
    pub tools: Tools,
    /// Appearance and startup view of the TUI.
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            include: vec![],
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
            concurrency: Concurrency::default(),
            clone_protocol: None,
            favourite: vec![],
            sync: vec![],
            exclude: vec![],
            tracks: vec![],
            fix: FixConfig::default(),
            stash: StashConfig::default(),
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

#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(default, deny_unknown_fields)]
pub struct Concurrency {
    /// Discovery and status: file system calls on this machine, mostly
    /// answered from memory. Beyond a point they slow each other down in
    /// the kernel. 0: automatic, i.e. the performance-core count where
    /// cores differ (Apple silicon), else half the CPUs.
    pub disk: usize,
    /// Network operations (fetch/pull/push/clone). 0: automatic, i.e.
    /// git's `fetch.parallel` if set above 0, else the CPU count up to 8
    /// (forges and ssh limit simultaneous connections).
    pub network: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StashConfig {
    /// Include untracked files.
    pub untracked: bool,
    /// Leave staged changes in the index after stashing them (`--keep-index`).
    pub keep_index: bool,
}

impl Default for StashConfig {
    fn default() -> Self {
        Self {
            untracked: true,
            keep_index: false,
        }
    }
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
    /// Highlight the changed words in diffs: in every hunk, only the hunk
    /// under the cursor, or none.
    pub refine: Refine,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Refine {
    #[default]
    All,
    Selected,
    Off,
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
            refine: Refine::All,
        }
    }
}

/// Config files merged as TOML, before becoming a [`Config`]: the defaults,
/// then each file after its includes. Tables merge key by key; values and
/// lists replace, lists of tables add up, and `extend_<list>` appends.
pub(crate) struct Layers {
    pub table: toml::Table,
    /// For each list (dotted key), the file each element came from (`None`:
    /// the defaults).
    pub origins: HashMap<String, Vec<Option<PathBuf>>>,
    /// The main file's own `include`.
    pub main_includes: Vec<String>,
}

impl Layers {
    fn defaults() -> Self {
        let table = toml::Table::try_from(Config::default()).expect("defaults serialize");
        let mut origins = HashMap::new();
        record_lists(&table, "", &mut origins);
        Self {
            table,
            origins,
            main_includes: vec![],
        }
    }

    /// The defaults, then `path` with its includes (read first).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut layers = Self::defaults();
        layers.main_includes = layers.read(path, &mut vec![])?;
        Ok(layers)
    }

    /// Merge `path`, after its includes; returns its own `include`.
    fn read(&mut self, path: &Path, chain: &mut Vec<PathBuf>) -> Result<Vec<String>, ConfigError> {
        let io = |source| ConfigError::Io {
            path: path.into(),
            source,
        };
        let real = std::fs::canonicalize(path).map_err(io)?;
        if chain.contains(&real) {
            let names: Vec<String> = chain
                .iter()
                .chain([&real])
                .map(|p| p.display().to_string())
                .collect();
            return Err(ConfigError::Include {
                path: path.into(),
                what: format!("cycle: {}", names.join(" -> ")),
            });
        }
        let text = std::fs::read_to_string(path).map_err(io)?;
        let mut layer: toml::Table =
            toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.into(),
                source,
            })?;
        let includes = take_include(&mut layer, path)?;
        chain.push(real);
        let dir = path.parent().unwrap_or(Path::new("."));
        for inc in &includes {
            for file in resolve_include(dir, inc, path)? {
                self.read(&file, chain)?;
            }
        }
        chain.pop();
        self.merge(layer, Some(path))?;
        Ok(includes)
    }

    fn merge(&mut self, layer: toml::Table, file: Option<&Path>) -> Result<(), ConfigError> {
        merge_table(&mut self.table, layer, "", file, &mut self.origins)
    }
}

/// Remove and return `include` from a file's table.
fn take_include(layer: &mut toml::Table, path: &Path) -> Result<Vec<String>, ConfigError> {
    let bad = || ConfigError::Include {
        path: path.into(),
        what: "must be a list of paths".into(),
    };
    match layer.remove("include") {
        None => Ok(vec![]),
        Some(toml::Value::Array(a)) => a
            .into_iter()
            .map(|v| v.as_str().map(String::from).ok_or_else(bad))
            .collect(),
        Some(_) => Err(bad()),
    }
}

/// The files `inc` names, relative to `dir`: one path, or the files a glob
/// in its last component matches (sorted; none is fine).
fn resolve_include(dir: &Path, inc: &str, from: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let err = |what: String| ConfigError::Include {
        path: from.into(),
        what,
    };
    let full = dir.join(expand_tilde(Path::new(inc)));
    let name = full
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = full.parent().unwrap_or(Path::new("."));
    let glob_chars = ['*', '?', '[', '{'];
    if parent.to_string_lossy().contains(glob_chars) {
        return Err(err(format!("{inc:?}: wildcards only in the file name")));
    }
    if !name.contains(glob_chars) {
        if !full.is_file() {
            return Err(err(format!("{inc:?}: no such file ({})", full.display())));
        }
        return Ok(vec![full]);
    }
    let matcher = globset::Glob::new(&name)
        .map_err(|e| err(format!("{inc:?}: {e}")))?
        .compile_matcher();
    let mut files: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| matcher.is_match(e.file_name()) && e.path().is_file())
        .map(|e| e.path())
        .collect();
    files.sort();
    Ok(files)
}

fn dotted(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn record_lists(
    t: &toml::Table,
    prefix: &str,
    origins: &mut HashMap<String, Vec<Option<PathBuf>>>,
) {
    for (k, v) in t {
        match v {
            toml::Value::Array(a) => {
                origins.insert(dotted(prefix, k), vec![None; a.len()]);
            }
            toml::Value::Table(sub) => record_lists(sub, &dotted(prefix, k), origins),
            _ => {}
        }
    }
}

/// Top-level settings renamed since: (old, new).
const OLD_NAMES: &[(&str, &str)] = &[("skip_dirs", "skip")];

fn is_tables(a: &[toml::Value]) -> bool {
    !a.is_empty() && a.iter().all(|v| v.is_table())
}

fn merge_table(
    base: &mut toml::Table,
    layer: toml::Table,
    prefix: &str,
    file: Option<&Path>,
    origins: &mut HashMap<String, Vec<Option<PathBuf>>>,
) -> Result<(), ConfigError> {
    let from = |n: usize| vec![file.map(Path::to_path_buf); n];
    let renamed = layer.into_iter().map(|(k, v)| {
        // `concurrency` was a number, the network setting, before the section
        let v = match (prefix, k.as_str(), v) {
            ("", "concurrency", toml::Value::Integer(n)) => {
                toml::Value::Table(toml::Table::from_iter([("network".into(), n.into())]))
            }
            (_, _, v) => v,
        };
        let k = match (prefix, k.as_str()) {
            ("", old) => OLD_NAMES
                .iter()
                .find(|(o, _)| *o == old || format!("extend_{o}") == old)
                .map(|(o, new)| k.replace(o, new))
                .unwrap_or(k),
            _ => k,
        };
        (k, v)
    });
    let (extends, plain): (Vec<_>, Vec<_>) = renamed.partition(|(k, _)| k.starts_with("extend_"));
    for (k, v) in plain {
        let key = dotted(prefix, &k);
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(l)) => {
                merge_table(b, l, &key, file, origins)?
            }
            (Some(toml::Value::Array(b)), toml::Value::Array(l))
                if is_tables(&l) || is_tables(b) =>
            {
                origins.entry(key).or_default().extend(from(l.len()));
                b.extend(l);
            }
            (_, v) => {
                if let toml::Value::Array(a) = &v {
                    origins.insert(key, from(a.len()));
                }
                base.insert(k, v);
            }
        }
    }
    for (k, v) in extends {
        let target = &k["extend_".len()..];
        let err = |reason| ConfigError::Extend {
            key: dotted(prefix, &k),
            reason,
        };
        match (base.get_mut(target), v) {
            (Some(toml::Value::Array(b)), toml::Value::Array(l)) => {
                origins
                    .entry(dotted(prefix, target))
                    .or_default()
                    .extend(from(l.len()));
                b.extend(l);
            }
            (Some(toml::Value::Array(_)), _) => return Err(err("must be a list")),
            (Some(_), _) => return Err(err("only a list setting can be extended")),
            (None, _) => return Err(err("no such setting to extend")),
        }
    }
    Ok(())
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
    #[error("config: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{path} (with its includes): {source}")]
    Invalid {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{key}: {reason}")]
    Extend { key: String, reason: &'static str },
    #[error("{path}: include {what}")]
    Include { path: PathBuf, what: String },
}

impl Config {
    /// Load from `path`, with its includes; a missing file yields the
    /// default config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let layers = Layers::load(path)?;
        let main_includes = layers.main_includes.clone();
        let mut c: Config = toml::Value::Table(layers.table)
            .try_into()
            .map_err(|source| ConfigError::Invalid {
                path: path.into(),
                source,
            })?;
        c.include = main_includes;
        c.normalize();
        Ok(c)
    }

    /// Parse one file's text over the defaults (`extend_*` applies; any
    /// `include` is kept but not read).
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        let mut layer: toml::Table = toml::from_str(s)?;
        let include = take_include(&mut layer, Path::new("config"))?;
        let mut layers = Layers::defaults();
        layers.merge(layer, None)?;
        let mut c: Config = toml::Value::Table(layers.table).try_into()?;
        c.include = include;
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

    /// How many network operations to run at once: see `concurrency.network`.
    pub fn network_jobs(&self) -> usize {
        if self.concurrency.network > 0 {
            return self.concurrency.network;
        }
        let fetch_parallel = gix::config::File::from_globals()
            .ok()
            .and_then(|f| f.integer("fetch.parallel").ok().flatten());
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        auto_jobs(fetch_parallel, cpus)
    }

    /// How many threads discovery and status use: see `concurrency.disk`.
    pub fn disk_jobs(&self) -> usize {
        if self.concurrency.disk > 0 {
            return self.concurrency.disk;
        }
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        auto_disk_jobs(performance_cores(), cpus)
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

/// Automatic disk concurrency: the performance cores when known, else
/// half the CPUs; at least one. Measured on an M-series Mac with 4
/// performance cores of 10: discovery was fastest with 4 threads, 30%
/// slower with 5 and 40% slower with 10, with kernel time tripling.
pub fn auto_disk_jobs(performance_cores: Option<usize>, cpus: usize) -> usize {
    performance_cores
        .filter(|&n| n > 0)
        .unwrap_or(cpus / 2)
        .max(1)
}

/// The fastest kind of core's count, when cores come in kinds.
#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    use sysctl::Sysctl;
    let read = |name: &str| -> Option<usize> {
        sysctl::Ctl::new(name).ok()?.value_string().ok()?.trim().parse().ok()
    };
    if read("hw.nperflevels")? < 2 {
        return None;
    }
    read("hw.perflevel0.logicalcpu")
}

#[cfg(not(target_os = "macos"))]
fn performance_cores() -> Option<usize> {
    None
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
        assert_eq!(Config::default().concurrency.network, 0, "automatic by default");
        let c = Config::parse("[concurrency]\nnetwork = 3").unwrap();
        assert_eq!(c.network_jobs(), 3);
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
    fn disk_concurrency_zero_is_the_performance_cores() {
        assert_eq!(Config::default().concurrency.disk, 0, "automatic by default");
        let c = Config::parse("[concurrency]\ndisk = 3").unwrap();
        assert_eq!((c.disk_jobs(), c.concurrency.network), (3, 0));
        assert!(Config::default().disk_jobs() >= 1);
        // the performance cores when known, else half the CPUs
        for (perf, cpus, want) in [
            (Some(4), 10, 4),
            (Some(12), 16, 12),
            (None, 10, 5),
            (None, 16, 8),
            (Some(0), 10, 5),
            (None, 3, 1),
            (None, 1, 1),
            (None, 0, 1),
        ] {
            assert_eq!(auto_disk_jobs(perf, cpus), want, "{perf:?} {cpus}");
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn apple_silicon_reports_fewer_performance_cores_than_cpus() {
        let cpus = std::thread::available_parallelism().unwrap().get();
        let p = performance_cores().expect("Apple silicon has core kinds");
        assert!(p >= 1 && p < cpus, "{p} of {cpus}");
        // the command, as a second opinion
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.perflevel0.logicalcpu"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), p.to_string());
        assert_eq!(Config::default().disk_jobs(), p);
    }

    #[test]
    fn a_top_level_concurrency_number_is_the_network_setting() {
        let c = Config::parse("concurrency = 3").unwrap();
        assert_eq!(c.concurrency, Concurrency { disk: 0, network: 3 });
        // inverse: the section is read as it is
        let c = Config::parse("[concurrency]\ndisk = 2\nnetwork = 5").unwrap();
        assert_eq!(c.concurrency, Concurrency { disk: 2, network: 5 });
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
