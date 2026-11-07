//! The workspace: every discovered repo with its identity, flags and status.
//! This is the model the CLI, TUI (and a future server) share.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rayon::prelude::*;
use serde::Serialize;

use crate::attention::{self, Issue};
use crate::config::{Config, ConfigError};
use crate::discover::{self, DiscoverOptions, RepoKind, Skip};
use crate::identity::RepoId;
use crate::paths::contract_tilde;
use crate::rules::{self, Flags, IssueList, Mark, RuleSet};
use crate::state::State;
use crate::status::{self, RepoStatus, StatusOptions};

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: RepoKind,
    /// Main repo for worktrees and submodules.
    pub parent: Option<PathBuf>,
    pub id: Option<RepoId>,
    /// Key for marks/snoozes: identity for main clones, path otherwise.
    pub key: String,
    pub flags: Flags,
    pub status: Option<Result<RepoStatus, String>>,
    /// Issues that count: those of the status, less ignored ones.
    pub issues: Vec<Issue>,
    /// Issues the status shows that `[ignore]` turns off for this repo.
    pub ignored_issues: Vec<Issue>,
    /// Every issue of the status, before ignoring.
    all_issues: Vec<Issue>,
}

impl Entry {
    pub fn new(path: PathBuf, kind: RepoKind) -> Self {
        let key = path.display().to_string();
        Self {
            path,
            kind,
            parent: None,
            id: None,
            key,
            flags: Flags::default(),
            status: None,
            issues: vec![],
            ignored_issues: vec![],
            all_issues: vec![],
        }
    }

    pub fn status(&self) -> Option<&RepoStatus> {
        self.status.as_ref().and_then(|s| s.as_ref().ok())
    }

    pub fn error(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|s| s.as_ref().err())
            .map(String::as_str)
    }

    pub fn display_name(&self) -> String {
        match (&self.id, self.kind, &self.parent) {
            (Some(id), RepoKind::Main | RepoKind::Bare, _) => id.slug(),
            // children are shown under their parent: keep them short
            (_, RepoKind::Worktree | RepoKind::Submodule, Some(parent)) => {
                match self.path.strip_prefix(parent) {
                    Ok(rel) => rel.display().to_string(),
                    Err(_) => self.path.file_name().map_or_else(
                        || contract_tilde(&self.path),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                }
            }
            _ => contract_tilde(&self.path),
        }
    }

    pub fn score(&self) -> u32 {
        if self.error().is_some() {
            90
        } else {
            attention::score(&self.issues)
        }
    }

    pub fn needs_attention(&self) -> bool {
        self.error().is_some() || attention::needs_attention(&self.issues) || self.flags.new
    }

    /// The kinds of fix auto-fix may use here: `kinds`, less those that
    /// would undo an ignored state (see [`crate::fix::turned_off_by`]).
    pub fn fix_kinds(&self, kinds: &[crate::fix::FixKind]) -> Vec<crate::fix::FixKind> {
        let off = crate::fix::turned_off_by(&self.ignored_issues);
        kinds.iter().copied().filter(|k| !off.contains(k)).collect()
    }

    pub fn change_count(&self) -> usize {
        self.status().map_or(0, |s| s.changes.total() + s.ahead())
    }

    fn set_status(&mut self, st: Result<RepoStatus, String>) {
        if let Ok(s) = &st {
            self.all_issues = attention::issues(s);
            self.issues = self.all_issues.clone();
            if self.id.is_none() {
                self.id = s.remote_url.as_deref().and_then(RepoId::from_url);
                if (self.kind == RepoKind::Main || self.kind == RepoKind::Bare)
                    && let Some(id) = &self.id
                {
                    self.key = id.to_string();
                }
            }
        } else {
            self.all_issues.clear();
            self.issues.clear();
        }
        self.ignored_issues.clear();
        self.status = Some(st);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ViewFilter {
    pub attention_only: bool,
    pub show_hidden: bool,
}

#[derive(Clone)]
pub struct Workspace {
    pub config: Config,
    pub state: State,
    rules: RuleSet,
    pub entries: Vec<Entry>,
    /// Directories the last scan didn't enter because of `max_depth`.
    pub truncated: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(config: Config, state: State) -> Result<Self, ConfigError> {
        let rules = RuleSet::new(&config)?;
        Skip::new(&config.exclude_paths, &config.roots)
            .map_err(|(pattern, source)| ConfigError::Pattern { pattern, source })?;
        Ok(Self {
            config,
            state,
            rules,
            entries: vec![],
            truncated: vec![],
        })
    }

    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    pub fn discover_options(&self) -> DiscoverOptions {
        DiscoverOptions {
            // validated in `new`; compiled per scan, as roots can change
            skip: Skip::new(&self.config.exclude_paths, &self.config.roots).unwrap_or_default(),
            max_depth: self.config.max_depth,
            scan_inside_repos: false,
            skip_hidden: self.config.skip_hidden,
            follow_symlinks: self.config.follow_symlinks,
            tuning: Default::default(),
        }
    }

    /// Names to complete repo patterns with: `owner/name` for repos with a
    /// forge identity, else the path (`~/…`). Discovery and each repo's
    /// remote only, no status, so it's quick enough to run on every Tab.
    pub fn repo_names(&self) -> Vec<String> {
        let mut names: Vec<String> = in_pool(self.config.disk_jobs(), || {
            discover::discover_all(&self.config.roots, &self.discover_options())
                .found
                .par_iter()
                .map(|f| {
                    let id = match f.kind {
                        RepoKind::Main | RepoKind::Bare => crate::isolation::open(&f.path)
                            .ok()
                            .and_then(|r| status::remote_and_url(&r).1)
                            .and_then(|u| RepoId::from_url(&u)),
                        _ => None,
                    };
                    id.map_or_else(|| contract_tilde(&f.path), |i| i.slug())
                })
                .collect()
        });
        names.sort();
        names.dedup();
        names
    }

    /// Discover repos and compute every status in parallel.
    pub fn scan(&mut self) {
        let discovery = in_pool(self.config.disk_jobs(), || {
            discover::discover_all(&self.config.roots, &self.discover_options())
        });
        self.truncated = discovery.truncated;
        let found = discovery.found;
        let old: HashMap<PathBuf, Entry> = self
            .entries
            .drain(..)
            .map(|e| (e.path.clone(), e))
            .collect();
        self.entries = found
            .into_iter()
            .map(|f| {
                old.get(&f.path)
                    .cloned()
                    .unwrap_or_else(|| Entry::new(f.path, f.kind))
            })
            .collect();
        self.refresh_all();
    }

    /// The entry naming a repo in the mark lists: its identity, or its path
    /// (`~/…`) without one.
    fn mark_name(e: &Entry) -> String {
        match &e.id {
            Some(id) => id.to_string(),
            None => contract_tilde(&e.path),
        }
    }

    /// Mark (or unmark) the repos at `indices` in `config` (see
    /// [`rules::set_mark`]); flags follow. Saving config is the caller's.
    pub fn set_mark(&mut self, indices: &[usize], m: Mark, on: bool) -> Result<(), ConfigError> {
        for &i in indices {
            let e = &self.entries[i];
            let base = m == Mark::Sync
                && e.id
                    .as_ref()
                    .and_then(|id| rules::track_for(&self.config, &self.state, id))
                    .is_some_and(|t| t.sync);
            let (id, path, name) = (e.id.clone(), e.path.clone(), Self::mark_name(e));
            rules::set_mark(&mut self.config, m, id.as_ref(), &path, &name, base, on)?;
        }
        self.marks_changed()
    }

    /// Ignore (`on`) or stop ignoring `list`'s issue for the repos at
    /// `indices`, pinned to each one's current state where the list pins
    /// (see [`rules::set_issue_ignored`]). Saving config is the caller's.
    pub fn set_issue_ignored(
        &mut self,
        indices: &[usize],
        list: IssueList,
        on: bool,
    ) -> Result<(), ConfigError> {
        for &i in indices {
            let e = &self.entries[i];
            let values = e.status().map(|s| list.values(s)).unwrap_or_default();
            let (id, path, name) = (e.id.clone(), e.path.clone(), Self::mark_name(e));
            rules::set_issue_ignored(
                &mut self.config,
                list,
                id.as_ref(),
                &path,
                &name,
                &values,
                on,
            )?;
        }
        self.marks_changed()
    }

    /// Drop the entries naming the repos at `indices` from every mark list,
    /// so patterns decide again.
    pub fn clear_marks(&mut self, indices: &[usize]) -> Result<(), ConfigError> {
        for &i in indices {
            let (id, path) = (self.entries[i].id.clone(), self.entries[i].path.clone());
            for m in Mark::ALL {
                rules::clear_mark(&mut self.config, m, id.as_ref(), &path)?;
            }
        }
        self.marks_changed()
    }

    /// How many of the repos at `indices` have entries of their own in the
    /// mark lists: clearing those of more than one needs confirmation.
    pub fn marked_count(&self, indices: &[usize]) -> usize {
        indices
            .iter()
            .filter(|&&i| {
                let e = &self.entries[i];
                Mark::ALL
                    .iter()
                    .any(|&m| !self.rules.own_entries(m, e.id.as_ref(), &e.path).is_empty())
            })
            .count()
    }

    fn marks_changed(&mut self) -> Result<(), ConfigError> {
        self.rules = RuleSet::new(&self.config)?;
        self.resolve_flags();
        Ok(())
    }

    pub fn refresh_all(&mut self) {
        let results = statuses(&self.config, self.entries.iter().map(|e| e.path.as_path()));
        for (e, r) in self.entries.iter_mut().zip(results) {
            e.set_status(r);
        }
        self.add_children();
        self.link_parents();
        self.resolve_flags();
    }

    /// Re-read one repo's status (after an operation on it).
    pub fn refresh(&mut self, path: &Path) {
        let r = status::status(path, StatusOptions::default()).map_err(|e| e.to_string());
        self.update_status(path, r);
    }

    /// Apply a status computed elsewhere (e.g. on a worker thread).
    pub fn update_status(&mut self, path: &Path, r: Result<RepoStatus, String>) {
        let Some(i) = self.index_of(path) else { return };
        self.entries[i].set_status(r);
        self.add_children();
        self.link_parents();
        self.resolve_flags();
    }

    /// Add an entry (e.g. a fresh clone) and compute its status.
    pub fn add(&mut self, path: PathBuf) {
        if self.index_of(&path).is_none() {
            let kind = discover::classify(&path).unwrap_or(RepoKind::Main);
            self.entries.push(Entry::new(path.clone(), kind));
            self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.refresh(&path);
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.index_of(path).map(|i| &self.entries[i])
    }

    /// Worktrees and submodules living outside the roots (or not walked into)
    /// are learnt from their main repo's status.
    fn add_children(&mut self) {
        let mut extra = vec![];
        for e in &self.entries {
            let Some(s) = e.status() else { continue };
            let kids = s.worktrees.iter().map(|p| (p, RepoKind::Worktree));
            let kids = kids.chain(s.submodules.iter().map(|p| (p, RepoKind::Submodule)));
            for (p, kind) in kids {
                let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                if !self.entries.iter().any(|x| x.path == p)
                    && !extra.iter().any(|x: &Entry| x.path == p)
                {
                    let mut child = Entry::new(p, kind);
                    child.parent = Some(e.path.clone());
                    extra.push(child);
                }
            }
        }
        if extra.is_empty() {
            return;
        }
        let results = statuses(&self.config, extra.iter().map(|e| e.path.as_path()));
        for (mut e, r) in extra.into_iter().zip(results) {
            e.set_status(r);
            self.entries.push(e);
        }
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        // Submodules may themselves have submodules.
        self.add_children();
    }

    fn link_parents(&mut self) {
        let mut parent_of: HashMap<PathBuf, PathBuf> = HashMap::new();
        for e in &self.entries {
            if let Some(s) = e.status() {
                for c in s.worktrees.iter().chain(&s.submodules) {
                    let c = std::fs::canonicalize(c).unwrap_or_else(|_| c.clone());
                    if c != e.path {
                        parent_of.insert(c, e.path.clone());
                    }
                }
            }
        }
        for e in &mut self.entries {
            if e.parent.is_none() {
                e.parent = parent_of.get(&e.path).cloned();
            }
        }
    }

    pub fn resolve_flags(&mut self) {
        let now = Timestamp::now();
        for e in &mut self.entries {
            e.flags = rules::resolve(
                &self.config,
                &self.rules,
                &self.state,
                e.id.as_ref(),
                &e.key,
                &e.path,
                now,
            );
            let Some(s) = e.status.as_ref().and_then(|s| s.as_ref().ok()) else {
                continue;
            };
            let ignored = |i: &Issue| {
                IssueList::of(*i).is_some_and(|l| {
                    self.rules
                        .ignores_issue(l, e.id.as_ref(), &e.path, &l.values(s))
                })
            };
            let (off, on): (Vec<Issue>, Vec<Issue>) = e.all_issues.iter().partition(|i| ignored(i));
            e.issues = on;
            e.ignored_issues = off;
        }
    }

    /// Indices into `entries` in display order: top-level repos sorted by
    /// favourite, severity, amount of change and name; each followed by its
    /// children (worktrees, submodules).
    pub fn ordered(&self, filter: ViewFilter) -> Vec<usize> {
        let visible = |e: &Entry| {
            (filter.show_hidden || !e.flags.hidden())
                && (!filter.attention_only || e.needs_attention())
        };
        let is_top = |e: &Entry| e.parent.as_ref().is_none_or(|p| self.index_of(p).is_none());
        let mut top: Vec<usize> = (0..self.entries.len())
            .filter(|&i| is_top(&self.entries[i]))
            .collect();
        top.sort_by(|&a, &b| {
            let (a, b) = (&self.entries[a], &self.entries[b]);
            b.needs_attention()
                .cmp(&a.needs_attention())
                .then(b.flags.favourite.cmp(&a.flags.favourite))
                .then(b.score().cmp(&a.score()))
                .then(b.change_count().cmp(&a.change_count()))
                .then_with(|| {
                    a.display_name()
                        .to_lowercase()
                        .cmp(&b.display_name().to_lowercase())
                })
        });
        let mut out = vec![];
        for t in top {
            let mut fam = vec![];
            self.collect_children(t, &mut fam, &mut vec![t]);
            let parent_visible = visible(&self.entries[t]);
            let visible_kids: Vec<usize> = fam
                .into_iter()
                .filter(|&c| visible(&self.entries[c]))
                .collect();
            // A parent is shown if it, or any child, is visible.
            if parent_visible || !visible_kids.is_empty() {
                out.push(t);
                out.extend(visible_kids);
            }
        }
        out
    }

    /// `seen` guards against parent cycles from inconsistent on-disk data.
    fn collect_children(&self, i: usize, out: &mut Vec<usize>, seen: &mut Vec<usize>) {
        let p = &self.entries[i].path;
        let mut kids: Vec<usize> = (0..self.entries.len())
            .filter(|&c| self.entries[c].parent.as_ref() == Some(p) && !seen.contains(&c))
            .collect();
        kids.sort_by_key(|&c| (self.entries[c].kind, self.entries[c].path.clone()));
        seen.extend(&kids);
        for c in kids {
            out.push(c);
            self.collect_children(c, out, seen);
        }
    }

    /// One-line warning about directories skipped by `max_depth`, if any.
    pub fn truncation_warning(&self) -> Option<String> {
        let first = self.truncated.first()?;
        let n = self.truncated.len();
        Some(format!(
            "{n} director{} below max_depth ({}) not scanned, e.g. {}; raise max_depth to include {}",
            if n == 1 { "y" } else { "ies" },
            self.config.max_depth,
            contract_tilde(first),
            if n == 1 { "it" } else { "them" },
        ))
    }

    pub fn depth(&self, i: usize) -> usize {
        let mut d = 0;
        let mut cur = &self.entries[i];
        while let Some(p) = &cur.parent {
            match self.get(p) {
                Some(e) if d < 16 => {
                    d += 1;
                    cur = e;
                }
                _ => break,
            }
        }
        d
    }
}

impl Workspace {
    /// Entries matching any of `patterns` (glob against display name, key and
    /// path, or an existing path). Empty `patterns` selects every visible entry.
    pub fn select(&self, patterns: &[String], filter: ViewFilter) -> Vec<usize> {
        let order = self.ordered(filter);
        if patterns.is_empty() {
            return order;
        }
        let matchers: Vec<_> = patterns
            .iter()
            .filter_map(|p| {
                let p = crate::paths::expand_tilde(Path::new(p))
                    .display()
                    .to_string();
                globset::GlobBuilder::new(&p)
                    .case_insensitive(true)
                    .build()
                    .ok()
                    .map(|g| g.compile_matcher())
            })
            .collect();
        let paths: Vec<PathBuf> = patterns
            .iter()
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .collect();
        // Explicit selection also reaches hidden entries.
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                paths.contains(&e.path)
                    || matchers.iter().any(|m| {
                        m.is_match(e.display_name()) || m.is_match(&e.key) || m.is_match(&e.path)
                    })
            })
            .collect()
    }
}

/// Run `f` on a pool of `n` threads, so its parallel iterators use at most
/// that many (on the current thread if the pool can't be made).
pub fn in_pool<R: Send>(n: usize, f: impl FnOnce() -> R + Send) -> R {
    match rayon::ThreadPoolBuilder::new().num_threads(n).build() {
        Ok(pool) => pool.install(f),
        Err(_) => f(),
    }
}

/// The status of each of `paths`, in order, [`Config::disk_jobs`] at a time.
fn statuses<'a>(
    config: &Config,
    paths: impl Iterator<Item = &'a Path>,
) -> Vec<Result<RepoStatus, String>> {
    let paths: Vec<&Path> = paths.collect();
    in_pool(config.disk_jobs(), || {
        paths
            .par_iter()
            .map(|p| status::status(p, StatusOptions::default()).map_err(|e| e.to_string()))
            .collect()
    })
}
