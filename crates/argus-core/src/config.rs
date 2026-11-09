//! User configuration (hand-edited TOML). Machine-written data lives in
//! [`crate::state`] so this file is never rewritten behind the user's back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fix::FixKind;
use crate::paths::expand_tilde;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "argus settings: a table of keys"
)]
pub struct Config {
    /// Config files this one builds on, e.g. `["~/.config/argus/work.toml",
    /// "conf.d/*.toml"]`: read first, in order (later ones win), then this
    /// file's settings override theirs, wherever this line is. Paths are
    /// relative to this file, `~` allowed, `*` etc. in the file name
    /// (matching nothing is fine). Lists of tables (`[[track]]`) add up.
    /// Separately, any list setting also takes `extend_<name>` to add to
    /// the list rather than replace it, e.g. `extend_exclude_paths =
    /// ["build-cache"]`.
    pub extend: Vec<String>,
    /// Directories scanned (recursively) for repositories. When empty, the
    /// current directory is scanned. Manage with `argus root add|remove|list`.
    #[schemars(example = vec!["~/dev"])]
    pub roots: Vec<PathBuf>,
    /// Directories never descended into (roots are always scanned). Like
    /// gitignore: a pattern without `/` is a directory name at any depth
    /// (`node_modules`, `*.egg-info`); one starting with `/` or `~` is a full
    /// path (`~/dev/archive`, `~/src/*/third_party`); any other with `/` is
    /// below each root (`go/pkg`). Globs: `*` (within one level), `**`, `?`,
    /// `[ab]`, `{a,b}`. Unlike `ignore`, which hides repos, repos there
    /// aren't scanned at all.
    pub exclude_paths: Vec<String>,
    /// Maximum directory depth below a root; deeper directories are skipped
    /// (with a warning). 0: no limit.
    pub max_depth: usize,
    /// Skip directories whose name starts with `.`; add one as a root to
    /// scan it.
    pub skip_hidden: bool,
    /// Follow symlinks to directories (`-L`). Cycles are cut and a repo
    /// reachable by several paths is listed once.
    pub follow_symlinks: bool,
    /// Stay on each root's filesystem (`-x`): don't enter other mounts
    /// (network shares, external disks) below a root, or follow links to
    /// them. Unix only.
    pub one_file_system: bool,
    /// How much runs at once.
    pub concurrency: Concurrency,
    /// How many commits back to look for a tag to describe a detached HEAD
    /// by (`v1.2-3-g1a2b3c4`, as `git describe --tags`). Opening the repo's
    /// details looks all the way back. 0: only when the details open.
    pub describe_limit: usize,
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
    /// Repos pulled by `argus pull`; entries as for `favourite`. Tracked
    /// owners' repos are pulled unless an entry here turns them off (`!`).
    #[schemars(example = vec!["acme/*", "!acme/huge-monorepo"])]
    pub pull: Vec<String>,
    /// What not to flag: whole repos, or particular issues in some repos.
    pub ignore: Ignore,
    /// Forge owners (orgs/users/teams) whose repos should all be cloned and
    /// pulled. Their repos are pulled unless `pull` turns them off.
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
            extend: vec![],
            roots: vec![],
            exclude_paths: [
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
            one_file_system: false,
            concurrency: Concurrency::default(),
            describe_limit: DESCRIBE_LIMIT,
            clone_protocol: None,
            favourite: vec![],
            pull: vec![],
            ignore: Ignore::default(),
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
    /// Clone repos missing locally during `argus pull` (flagged as new).
    #[default]
    Auto,
    /// Only report repos missing locally.
    Notify,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(
    deny_unknown_fields,
    expecting = "a [[track]] table with owner and its options"
)]
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
    /// What `argus pull` does with repos missing locally.
    #[serde(default)]
    pub clone: CloneMode,
    /// Pull this owner's repos with `argus pull`.
    #[serde(default = "yes")]
    pub pull: bool,
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

/// `[ignore]`: entries as for `favourite` (a repo or a pattern, matched
/// against identity, `owner/name` or path; the last match wins, `!` turns
/// it off).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [ignore] with repos and issue lists"
)]
pub struct Ignore {
    /// Repos hidden, and left out of pulls, clones and bulk operations.
    #[schemars(example = vec!["~/dev/archive/**"])]
    pub repos: Vec<String>,
    /// Repos normally on a branch other than the default. `repo@branch`
    /// ignores only that branch (glob allowed), so another is flagged.
    #[schemars(example = vec!["acme/legacy@release-*"])]
    pub non_default_branch: Vec<String>,
    /// Repos normally on a detached HEAD. `repo@value` ignores only that:
    /// a tag on HEAD, its describe (`v1.2-3-g1a2b3c4`) or its commit id
    /// (7 characters or all of it); glob allowed.
    #[schemars(example = vec!["acme/pinned-dep@v1.2.0"])]
    pub detached: Vec<String>,
    /// Repos with no remote.
    pub no_remote: Vec<String>,
    /// Repos whose branch has no upstream. `repo@branch` as for
    /// `non_default_branch`.
    pub no_upstream: Vec<String>,
    /// Repos with stashes, stash by stash: `repo@message` covers stashes
    /// whose message (after git's `On <branch>: `) matches (glob allowed).
    /// By default, every stash but those argus's stash fix made.
    pub stashes: Vec<String>,
    /// Repos with other local branches holding unpushed commits.
    pub unpushed_branches: Vec<String>,
    /// Repos behind their upstream.
    pub behind: Vec<String>,
}

impl Default for Ignore {
    fn default() -> Self {
        Self {
            repos: vec![],
            non_default_branch: vec![],
            detached: vec![],
            no_remote: vec![],
            no_upstream: vec![],
            stashes: vec!["**".into(), format!("!**@{} *", crate::ops::STASH_PREFIX)],
            unpushed_branches: vec![],
            behind: vec![],
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [concurrency] with disk and network"
)]
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
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [stash] with untracked and keep_index"
)]
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
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [fix] with its options"
)]
pub struct FixConfig {
    /// Fixes an auto-fix action may apply, highest priority first. Each
    /// action applies one: the first in this list that the repo needs.
    pub auto: Vec<FixKind>,
}

impl FixConfig {
    /// What's wrong with `auto` that isn't an error: kinds listed that are
    /// never automatic, so are left out.
    pub fn warnings(&self) -> Vec<String> {
        let manual: Vec<&str> = self
            .auto
            .iter()
            .filter(|k| k.manual())
            .map(|k| k.name())
            .collect();
        if manual.is_empty() {
            return vec![];
        }
        vec![format!(
            "[fix] auto lists {}, which need input or confirmation, so are never automatic: \
             they're left out (choose them from the TUI's menus, or `argus fix -k` for abort and delete-branches)",
            manual.join(", ")
        )]
    }
}

impl Default for FixConfig {
    fn default() -> Self {
        Self {
            auto: vec![
                FixKind::Push,
                FixKind::PushBranches,
                FixKind::SwitchDefault,
                FixKind::Reattach,
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [tools] with its options"
)]
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
#[serde(
    default,
    deny_unknown_fields,
    expecting = "a table: [ui] with its options"
)]
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
    /// ignored, snoozed, branch, detached, detached_commits, ahead, behind, staged, unstaged,
    /// untracked, conflict, stash, in_progress, gone, no_upstream, branches,
    /// error, pull, worktree, submodule; and in the interface: spinner (one
    /// frame per character), marked, ok, failed, celebrate, and the ignore
    /// menu's issue states issue_counted, issue_pinned, issue_ignored.
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
/// then each file after the files it extends. Tables merge key by key; values and
/// lists replace, lists of tables add up, and `extend_<list>` appends.
pub(crate) struct Layers {
    pub table: toml::Table,
    /// The main file's own `extend`.
    pub main_extends: Vec<String>,
    /// The files read, in the order merged: a file after those it extends, so
    /// the main file last.
    pub files: Vec<PathBuf>,
}

impl Layers {
    fn defaults() -> Self {
        let table = toml::Table::try_from(Config::default()).expect("defaults serialize");
        Self {
            table,
            main_extends: vec![],
            files: vec![],
        }
    }

    /// The defaults, then `path` with the files it extends (read first).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut layers = Self::defaults();
        layers.main_extends = layers.read(path, &mut vec![])?;
        Ok(layers)
    }

    /// Merge `path`, after the files it extends; returns its own `extend`.
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
            return Err(ConfigError::ExtendFile {
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
        let bases = take_extend(&mut layer, path)?;
        chain.push(real);
        let dir = path.parent().unwrap_or(Path::new("."));
        for inc in &bases {
            for file in resolve_extend(dir, inc, path)? {
                self.read(&file, chain)?;
            }
        }
        chain.pop();
        self.merge(layer)?;
        self.files.push(path.to_path_buf());
        Ok(bases)
    }

    fn merge(&mut self, layer: toml::Table) -> Result<(), ConfigError> {
        merge_table(&mut self.table, layer, "")
    }
}

/// Remove and return `extend` from a file's table.
fn take_extend(layer: &mut toml::Table, path: &Path) -> Result<Vec<String>, ConfigError> {
    let bad = || ConfigError::ExtendFile {
        path: path.into(),
        what: "must be a list of paths".into(),
    };
    match layer.remove("extend") {
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
fn resolve_extend(dir: &Path, inc: &str, from: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let err = |what: String| ConfigError::ExtendFile {
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

fn is_tables(a: &[toml::Value]) -> bool {
    !a.is_empty() && a.iter().all(|v| v.is_table())
}

fn merge_table(
    base: &mut toml::Table,
    layer: toml::Table,
    prefix: &str,
) -> Result<(), ConfigError> {
    let (extends, plain): (Vec<_>, Vec<_>) = layer
        .into_iter()
        .partition(|(k, _)| k.starts_with("extend_"));
    for (k, v) in plain {
        let key = dotted(prefix, &k);
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(l)) => merge_table(b, l, &key)?,
            (Some(toml::Value::Array(b)), toml::Value::Array(l))
                if is_tables(&l) || is_tables(b) =>
            {
                b.extend(l);
            }
            (_, v) => {
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
            (Some(toml::Value::Array(b)), toml::Value::Array(l)) => b.extend(l),
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
    #[error("parsing {path}: {}", plain(source))]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        source: globset::Error,
    },
    #[error("config: {}", plain(.0))]
    Toml(#[from] toml::de::Error),
    #[error("{path} (with the files it extends): {}", plain(source))]
    Invalid {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{key}: {reason}")]
    Extend { key: String, reason: &'static str },
    #[error("[ignore] {list}: {entry:?}: {reason}")]
    IgnoreEntry {
        list: &'static str,
        entry: String,
        reason: &'static str,
    },
    #[error("{path}: extend {what}")]
    ExtendFile { path: PathBuf, what: String },
}

/// A TOML or type error in plain words: Rust's type names (`usize`,
/// "a sequence") mean nothing to someone editing a config file.
fn plain(e: &toml::de::Error) -> String {
    let mut s = e.to_string();
    for (rust, words) in [
        ("expected usize", "expected a whole number, 0 or more"),
        ("expected u64", "expected a whole number, 0 or more"),
        ("expected i64", "expected a whole number"),
        ("expected a sequence", "expected a list"),
        ("expected a boolean", "expected true or false"),
    ] {
        s = s.replace(rust, words);
    }
    s
}

impl Config {
    /// Load from `path`, with the files it extends; a missing file yields the
    /// default config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let layers = Layers::load(path)?;
        let main_extends = layers.main_extends.clone();
        let mut c: Config = toml::Value::Table(layers.table)
            .try_into()
            .map_err(|source| ConfigError::Invalid {
                path: path.into(),
                source,
            })?;
        c.extend = main_extends;
        c.normalize();
        Ok(c)
    }

    /// Parse one file's text over the defaults (`extend_*` applies; any
    /// `extend` is kept but not read).
    pub fn parse(s: &str) -> Result<Self, ConfigError> {
        let mut layer: toml::Table = toml::from_str(s)?;
        let extend = take_extend(&mut layer, Path::new("config"))?;
        let mut layers = Layers::defaults();
        layers.merge(layer)?;
        let mut c: Config = toml::Value::Table(layers.table).try_into()?;
        c.extend = extend;
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

    /// What status should do, as configured.
    pub fn status_options(&self) -> crate::status::StatusOptions {
        crate::status::StatusOptions {
            describe_limit: self.describe_limit,
            ..Default::default()
        }
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

/// Default for `describe_limit`: about 2.4 ms at worst without a
/// commit-graph file (measured: `describe, no reachable tag` benchmark).
pub const DESCRIBE_LIMIT: usize = 1000;

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
        sysctl::Ctl::new(name)
            .ok()?
            .value_string()
            .ok()?
            .trim()
            .parse()
            .ok()
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
        let c = Config::parse("roots=['/r']\n[[track]]\nowner='acme'\n[[track]]\nowner='acme'\nteam='core'\nclone='notify'\npull=false").unwrap();
        assert_eq!(c.tracks[0].host, "github.com");
        assert_eq!(c.tracks[0].clone, CloneMode::Auto);
        assert!(c.tracks[0].pull);
        assert_eq!(c.tracks[0].key(), "github.com/acme");
        assert_eq!(c.tracks[1].key(), "github.com/acme#core");
        assert_eq!(c.tracks[1].clone, CloneMode::Notify);
        assert!(!c.tracks[1].pull);
        assert_eq!(c.clone_dir(&c.tracks[0]), Some(PathBuf::from("/r/acme")));
    }

    #[test]
    fn concurrency_zero_is_automatic() {
        assert_eq!(
            Config::default().concurrency.network,
            0,
            "automatic by default"
        );
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
        assert_eq!(
            Config::default().concurrency.disk,
            0,
            "automatic by default"
        );
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
    fn a_misused_section_says_what_it_expects() {
        for (key, toml) in [
            ("concurrency", "concurrency = 3"),
            ("stash", "stash = 3"),
            ("fix", "fix = 3"),
            ("tools", "tools = 3"),
            ("ui", "ui = 3"),
            ("[track]", "track = [3]"),
        ] {
            let e = Config::parse(toml).unwrap_err().to_string();
            assert!(e.contains(&format!("[{key}]")), "{toml}: {e}");
            assert!(!e.contains("struct"), "{toml}: {e}");
        }
        // the top level too
        let e = toml::Value::Integer(3).try_into::<Config>().unwrap_err();
        assert!(!e.to_string().contains("struct"), "{e}");
    }

    #[test]
    fn type_errors_are_in_plain_words() {
        for (toml, want) in [
            ("max_depth = 'deep'", "expected a whole number, 0 or more"),
            (
                "[concurrency]\ndisk = -1",
                "expected a whole number, 0 or more",
            ),
            ("roots = '~/dev'", "expected a list"),
            ("skip_hidden = 'yes'", "expected true or false"),
        ] {
            let e = Config::parse(toml).unwrap_err().to_string();
            assert!(e.contains(want), "{toml}: {e}");
            assert!(
                !e.contains("usize") && !e.contains("sequence"),
                "{toml}: {e}"
            );
        }
    }

    #[test]
    fn old_setting_names_are_errors_not_silently_ignored() {
        for old in [
            "skip = ['x']",
            "skip_dirs = ['x']",
            "extend_skip = ['x']",
            "exclude = ['x']",
            "concurrency = 3",
            "status_concurrency = 3",
            "include = ['x.toml']",
            "sync = ['a/b']",
            "extend_sync = ['a/b']",
            "[[track]]\nowner = 'acme'\nsync = false",
            "[ignore]\noff_default = ['a/b']",
            "[ignore]\nextend_off_default = ['a/b']",
        ] {
            assert!(Config::parse(old).is_err(), "{old}");
        }
        // inverse: the new names
        let c = Config::parse("[ignore]\nnon_default_branch = ['a/b']").unwrap();
        assert_eq!(c.ignore.non_default_branch, ["a/b"]);
        let c = Config::parse(
            "exclude_paths = ['x']\nignore.repos = ['a/b']\n[concurrency]\ndisk = 2\nnetwork = 5",
        )
        .unwrap();
        assert_eq!(
            (c.exclude_paths, c.ignore.repos),
            (vec!["x".into()], vec!["a/b".into()])
        );
        assert_eq!(
            c.concurrency,
            Concurrency {
                disk: 2,
                network: 5
            }
        );
        let c = Config::parse(
            "pull = ['a/b']\nextend_pull = ['c/d']\n[[track]]\nowner = 'acme'\npull = false",
        )
        .unwrap();
        assert_eq!(c.pull, ["a/b", "c/d"]);
        assert!(!c.tracks[0].pull);
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
