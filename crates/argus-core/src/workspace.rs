//! The workspace: every discovered repo with its identity, flags and status.
//! This is the model the CLI, TUI (and a future server) share.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rayon::prelude::*;
use serde::Serialize;

use crate::attention::{self, Issue};
use crate::config::{Config, ConfigError};
use crate::discover::{self, DiscoverOptions, RepoKind};
use crate::identity::RepoId;
use crate::paths::contract_tilde;
use crate::rules::{self, Flags, RuleSet};
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
    pub issues: Vec<Issue>,
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
        match (&self.id, self.kind) {
            (Some(id), RepoKind::Main | RepoKind::Bare) => id.slug(),
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

    pub fn change_count(&self) -> usize {
        self.status().map_or(0, |s| s.changes.total() + s.ahead())
    }

    fn set_status(&mut self, st: Result<RepoStatus, String>) {
        if let Ok(s) = &st {
            self.issues = attention::issues(s);
            if self.id.is_none() {
                self.id = s.remote_url.as_deref().and_then(RepoId::from_url);
                if (self.kind == RepoKind::Main || self.kind == RepoKind::Bare)
                    && let Some(id) = &self.id
                {
                    self.key = id.to_string();
                }
            }
        } else {
            self.issues.clear();
        }
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
}

impl Workspace {
    pub fn new(config: Config, state: State) -> Result<Self, ConfigError> {
        let rules = RuleSet::new(&config.rules)?;
        Ok(Self {
            config,
            state,
            rules,
            entries: vec![],
        })
    }

    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    pub fn discover_options(&self) -> DiscoverOptions {
        DiscoverOptions {
            skip_dirs: self.config.skip_dirs.clone(),
            max_depth: self.config.max_depth,
            scan_inside_repos: false,
        }
    }

    /// Discover repos and compute every status in parallel.
    pub fn scan(&mut self) {
        let found = discover::discover(&self.config.roots, &self.discover_options());
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

    pub fn refresh_all(&mut self) {
        let opts = StatusOptions::default();
        let results: Vec<_> = self
            .entries
            .par_iter()
            .map(|e| status::status(&e.path, opts).map_err(|e| e.to_string()))
            .collect();
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
        let opts = StatusOptions::default();
        let results: Vec<_> = extra
            .par_iter()
            .map(|e| status::status(&e.path, opts).map_err(|e| e.to_string()))
            .collect();
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
            b.flags
                .favourite
                .cmp(&a.flags.favourite)
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
