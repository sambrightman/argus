//! The workspace: every discovered repo with its identity, flags and status.
//! This is the model the CLI, TUI (and a future server) share.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::Timestamp;
use rayon::prelude::*;
use serde::Serialize;

use crate::attention::{self, Issue};
use crate::config::{Config, ConfigError};
use crate::discover::{self, DiscoverOptions, RepoKind, Skip};
use crate::forge::LocalForges;
use crate::identity::RepoId;
use crate::paths::contract_tilde;
use crate::rules::{self, Flags, IssueList, RuleSet, Tag};
use crate::state::State;
use crate::status::{self, RepoStatus};

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: RepoKind,
    /// Main repo for worktrees and submodules, and the repo a nested one is in.
    pub parent: Option<PathBuf>,
    /// A repo nested untracked in its parent's work tree: shown only under
    /// its line there, not listed, and left out of everything done to all.
    pub nested: bool,
    pub id: Option<RepoId>,
    /// Key for tags/snoozes: identity for main clones, path otherwise.
    pub key: String,
    pub flags: Flags,
    pub status: Option<Result<RepoStatus, String>>,
    /// Issues that count: those of the status, less ignored ones.
    pub issues: Vec<Issue>,
    /// Issues the status shows that `[tags.ignore.issues]` turns off for this repo.
    pub ignored_issues: Vec<Issue>,
    /// Issues the status shows that are snoozed (until their snooze ends).
    pub snoozed_issues: Vec<Issue>,
    /// For each of the status's stashes, whether `[tags.ignore.issues] stashes` covers it.
    pub stash_ignored: Vec<bool>,
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
            nested: false,
            id: None,
            key,
            flags: Flags::default(),
            status: None,
            issues: vec![],
            ignored_issues: vec![],
            snoozed_issues: vec![],
            stash_ignored: vec![],
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
    /// would undo an ignored or snoozed state (see
    /// [`crate::fix::turned_off_by`]).
    pub fn fix_kinds(&self, kinds: &[crate::fix::FixKind]) -> Vec<crate::fix::FixKind> {
        let both: Vec<Issue> = self
            .ignored_issues
            .iter()
            .chain(&self.snoozed_issues)
            .copied()
            .collect();
        let off = crate::fix::turned_off_by(&both);
        kinds.iter().copied().filter(|k| !off.contains(k)).collect()
    }

    pub fn change_count(&self) -> usize {
        self.status().map_or(0, |s| s.changes.total() + s.ahead())
    }

    fn set_status(&mut self, st: Result<RepoStatus, String>, ids: &LocalForges) {
        if let Ok(s) = &st {
            self.all_issues = attention::issues(s);
            self.issues = self.all_issues.clone();
            if self.id.is_none() {
                self.id = s.remote_url.as_deref().and_then(|u| ids.id(u));
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
        self.snoozed_issues.clear();
        self.stash_ignored.clear();
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
    /// The state as it was read, or last saved: what has changed since
    /// is what a save adds to the file (see [`Self::save_state`]).
    state_base: State,
    rules: RuleSet,
    pub entries: Vec<Entry>,
    /// Directories the last scan didn't enter because of `max_depth`.
    pub truncated: Vec<PathBuf>,
    /// Folders walked for nothing, with how many (see
    /// [`discover::Discovery::barren`]).
    pub barren: Vec<(PathBuf, usize)>,
    /// Repos kept open between status reads, if wanted (shared by clones).
    kept: Option<Arc<status::KeptRepos>>,
    /// See [`Self::check_branches_later`].
    branches_first: Option<usize>,
}

/// How many branches besides HEAD's and the default a status checks at
/// once, the newest, when the rest are checked later (see
/// [`Workspace::check_branches_later`]).
pub const BRANCHES_FIRST: usize = 10;

/// Reads repos' status the way the workspace does: through its kept repos,
/// if any, with its config's options. Cheap to clone and send to workers.
#[derive(Clone)]
pub struct StatusReader {
    kept: Option<Arc<status::KeptRepos>>,
    opts: status::StatusOptions,
}

impl StatusReader {
    pub fn read(&self, path: &Path) -> Result<RepoStatus, String> {
        match &self.kept {
            Some(k) => k.status(path, self.opts),
            None => status::status(path, self.opts),
        }
        .map_err(|e| e.to_string())
    }

    /// Reading `n` statuses side by side: each takes one thread for its
    /// files, rather than every core, the statuses being parallel already
    /// (with them all, a scan waited on its own threads).
    pub fn together(mut self, n: usize) -> Self {
        if n > 1 {
            self.opts.threads = Some(1);
        }
        self
    }

    /// Every branch of a repo, checked (see [`status::branches_of`]), for
    /// [`Workspace::update_branches`].
    pub fn branches(&self, path: &Path) -> Result<Vec<status::BranchInfo>, String> {
        match &self.kept {
            Some(k) => k.branches(path),
            None => status::branches_of(path),
        }
        .map_err(|e| e.to_string())
    }
}

impl Workspace {
    pub fn new(config: Config, state: State) -> Result<Self, ConfigError> {
        let rules = RuleSet::new(&config)?;
        Skip::new(&config.exclude_paths, &config.roots)
            .map_err(|(pattern, source)| ConfigError::Pattern { pattern, source })?;
        Ok(Self {
            config,
            state_base: state.clone(),
            state,
            rules,
            entries: vec![],
            truncated: vec![],
            barren: vec![],
            kept: None,
            branches_first: None,
        })
    }

    /// Save the state to `path`, on top of what the file holds by now:
    /// what another argus wrote since this one read it is kept (see
    /// [`State::save_merged`]).
    pub fn save_state(&mut self, path: &Path) -> Result<(), crate::state::StateError> {
        self.state.save_merged(path, &mut self.state_base)
    }

    /// Keep repos open between status reads (see [`status::KeptRepos`]),
    /// for front-ends that refresh again and again.
    pub fn keep_repos_open(&mut self) {
        self.kept = Some(Arc::default());
    }

    /// How many times kept repos were opened (rather than reused), for tests.
    #[doc(hidden)]
    pub fn kept_opens(&self) -> Option<usize> {
        self.kept.as_ref().map(|k| k.opens())
    }

    /// Have each status check only the newest [`BRANCHES_FIRST`] branches
    /// and HEAD's and the default, leaving the rest
    /// [unchecked](status::BranchInfo::unchecked) for a front-end to check
    /// after showing the statuses ([`Self::unchecked`],
    /// [`StatusReader::branches`], [`Self::update_branches`]).
    pub fn check_branches_later(&mut self) {
        self.branches_first = Some(BRANCHES_FIRST);
    }

    pub fn status_reader(&self) -> StatusReader {
        StatusReader {
            kept: self.kept.clone(),
            opts: status::StatusOptions {
                branches_first: self.branches_first,
                ..self.config.status_options()
            },
        }
    }

    /// The repos whose statuses have branches left unchecked.
    pub fn unchecked(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| {
                e.status()
                    .is_some_and(|s| s.branches.iter().any(|b| b.unchecked))
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// The repos whose last status found their index stale
    /// ([`crate::status::Changes::stale`]), to refresh
    /// ([`crate::ops::refresh_index`]).
    pub fn stale(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|e| e.status().is_some_and(|s| s.changes.stale > 0))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Refresh the indexes of the repos [`stale`](Self::stale) lists, a few
    /// at a time: those that couldn't be, and why.
    pub fn refresh_stale(&self) -> Vec<(PathBuf, String)> {
        use rayon::prelude::*;
        let stale = self.stale();
        in_pool(self.config.disk_jobs(), || {
            stale
                .into_par_iter()
                .filter_map(|p| {
                    let e = crate::ops::refresh_index(&p).err()?;
                    Some((p, e))
                })
                .collect()
        })
    }

    /// Show each repo's branches, all checked ([`StatusReader::branches`]),
    /// in place of those its status has, and what follows from them: unless
    /// they've moved since (another status has its own), or couldn't be
    /// checked (they stay unchecked).
    pub fn update_branches(
        &mut self,
        checked: impl IntoIterator<Item = (PathBuf, Result<Vec<status::BranchInfo>, String>)>,
    ) {
        let mut any = false;
        let at = self.positions();
        for (path, r) in checked {
            let (Some(&i), Ok(branches)) = (at.get(&path), r) else {
                continue;
            };
            let e = &mut self.entries[i];
            let Some(Ok(s)) = &mut e.status else {
                continue;
            };
            let same = |a: &status::BranchInfo, b: &status::BranchInfo| {
                (&a.name, &a.tip, a.is_head) == (&b.name, &b.tip, b.is_head)
            };
            let moved = s.branches.len() != branches.len()
                || s.branches.iter().zip(&branches).any(|(a, b)| !same(a, b));
            if moved || s.branches == branches {
                continue;
            }
            s.branches = branches;
            e.all_issues = attention::issues(s);
            e.issues = e.all_issues.clone();
            any = true;
        }
        if any {
            self.resolve_flags();
        }
    }

    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    pub fn discover_options(&self) -> DiscoverOptions {
        DiscoverOptions {
            // validated in `new`; compiled per scan, as roots can change
            skip: Skip::new(&self.config.exclude_paths, &self.config.roots).unwrap_or_default(),
            max_depth: self.config.max_depth,
            scan_inside_repos: self.config.scan_inside_repos,
            skip_hidden: self.config.skip_hidden,
            follow_symlinks: self.config.follow_symlinks,
            one_file_system: self.config.one_file_system,
            barren_at: discover::BARREN_AT,
            tuning: Default::default(),
        }
    }

    /// Names to complete repo patterns with: `owner/name` for repos with a
    /// forge identity, else the path (`~/…`). Discovery and each repo's
    /// remote only, no status, so it's quick enough to run on every Tab.
    pub fn repo_names(&self) -> Vec<String> {
        let ids = self.config.local_forges();
        let mut names: Vec<String> = in_pool(self.config.disk_jobs(), || {
            discover::discover_all(&self.config.roots, &self.discover_options())
                .found
                .par_iter()
                .map(|f| {
                    let id = match f.kind {
                        RepoKind::Main | RepoKind::Bare => crate::isolation::open(&f.path)
                            .ok()
                            .and_then(|r| status::remote_and_url(&r).1)
                            .and_then(|u| ids.id(&u)),
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
        // (named from the start, so its parts are said as its: `scan: flags`)
        crate::trace::step(
            "scan",
            || "scan".into(),
            || {
                self.scan_untimed();
                let n = self.entries.len();
                crate::trace::amend(|| format!("scan: {n} {}", crate::plural(n, "repo", "repos")));
            },
        );
    }

    fn scan_untimed(&mut self) {
        let discovery = crate::trace::timed(
            "discover",
            |d: &discover::Discovery| {
                let roots: Vec<String> = self
                    .config
                    .roots
                    .iter()
                    .map(|r| contract_tilde(r))
                    .collect();
                let cut = match d.truncated.len() {
                    0 => String::new(),
                    n => format!(", {n} cut short by max_depth"),
                };
                format!(
                    "discover in {}: {} {}{cut}",
                    roots.join(", "),
                    d.found.len(),
                    crate::plural(d.found.len(), "repo", "repos")
                )
            },
            || {
                in_pool(self.config.disk_jobs(), || {
                    discover::discover_all(&self.config.roots, &self.discover_options())
                })
            },
        );
        self.truncated = discovery.truncated;
        self.barren = discovery.barren;
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
        // read with git's installation config as remembered: if git now
        // says another, again with that
        if crate::installation::settle() {
            crate::trace::note(|| "git's installation config moved: statuses read again".into());
            // (shared with the workspaces cloned from this one)
            if let Some(k) = &self.kept {
                k.forget();
            }
            self.refresh_all();
        }
        if let Some(k) = &self.kept {
            let found: std::collections::HashSet<&Path> =
                self.entries.iter().map(|e| e.path.as_path()).collect();
            k.retain(|p| found.contains(p));
        }
    }

    /// The entry naming a repo in the tag lists: its identity, or its path
    /// (`~/…`) without one.
    fn tag_name(e: &Entry) -> String {
        match &e.id {
            Some(id) => id.to_string(),
            None => contract_tilde(&e.path),
        }
    }

    /// Tag (or untag) the repos at `indices` in `config` (see
    /// [`rules::set_tag`]); flags follow. Saving config is the caller's.
    /// Ignore excludes pull and tags with sort points: ignoring turns them
    /// off, and turning one on stops ignoring (labels don't).
    pub fn set_tag(&mut self, indices: &[usize], m: Tag, on: bool) -> Result<(), ConfigError> {
        let tags = exclusive(&self.config, m, on);
        for &i in indices {
            let e = &self.entries[i];
            let tracked_pull =
                e.id.as_ref()
                    .and_then(|id| rules::track_for(&self.config, &self.state, id))
                    .is_some_and(|t| t.pull);
            let (id, path, name) = (e.id.clone(), e.path.clone(), Self::tag_name(e));
            for &(m, on) in &tags {
                let base = m == Tag::Pull && tracked_pull;
                rules::set_tag(&mut self.config, m, id.as_ref(), &path, &name, base, on)?;
            }
        }
        self.tags_changed()
    }

    /// Ignore (`on`) or stop ignoring `list`'s issue for the repos at
    /// `indices`, pinned to each one's current state where the list pins
    /// (see [`rules::set_issue_ignored`]). Saving config is the caller's.
    /// Ignore `list`'s issue (`on`) or not for the repos at `indices`,
    /// pinned to its current value(s) or not: `pin` `None` is the list's
    /// default ([`IssueList::pins_by_default`]).
    pub fn set_issue_ignored(
        &mut self,
        indices: &[usize],
        list: IssueList,
        on: bool,
        pin: Option<bool>,
    ) -> Result<(), ConfigError> {
        let pin = pin.unwrap_or(list.pins_by_default());
        for &i in indices {
            let e = &self.entries[i];
            let values = e.status().map(|s| list.values(s)).unwrap_or_default();
            let (id, path, name) = (e.id.clone(), e.path.clone(), Self::tag_name(e));
            rules::set_issue_ignored(
                &mut self.config,
                list,
                id.as_ref(),
                &path,
                &name,
                &values,
                on,
                pin,
            )?;
        }
        self.tags_changed()
    }

    /// Where repo `i`'s `list` issue stands (see [`rules::IgnoreState`]).
    pub fn issue_state(
        &self,
        i: usize,
        list: IssueList,
    ) -> Result<rules::IgnoreState, ConfigError> {
        let e = &self.entries[i];
        let values = e.status().map(|s| list.values(s)).unwrap_or_default();
        Ok(RuleSet::new(&self.config)?.issue_state(list, e.id.as_ref(), &e.path, &values))
    }

    /// Put `list`'s issue in `state` for the repos at `indices` (see
    /// [`rules::set_issue_state`]). Saving config is the caller's.
    pub fn set_issue_state(
        &mut self,
        indices: &[usize],
        list: IssueList,
        state: rules::IgnoreState,
    ) -> Result<(), ConfigError> {
        for &i in indices {
            let e = &self.entries[i];
            let values = e.status().map(|s| list.values(s)).unwrap_or_default();
            let (id, path, name) = (e.id.clone(), e.path.clone(), Self::tag_name(e));
            rules::set_issue_state(
                &mut self.config,
                list,
                id.as_ref(),
                &path,
                &name,
                &values,
                state,
            )?;
        }
        self.tags_changed()
    }

    /// Drop the entries naming the repos at `indices` from every tag list,
    /// so patterns decide again.
    pub fn clear_tags(&mut self, indices: &[usize]) -> Result<(), ConfigError> {
        for &i in indices {
            let (id, path) = (self.entries[i].id.clone(), self.entries[i].path.clone());
            for m in Tag::all(&self.config) {
                rules::clear_tag(&mut self.config, m, id.as_ref(), &path)?;
            }
        }
        self.tags_changed()
    }

    /// Tag (or untag) the owners of the repos at `indices` as a whole,
    /// with their pattern (`github.com/acme/*`), ignore excluding the
    /// others as for a repo. Repos without an identity have no owner.
    pub fn set_owner_tag(
        &mut self,
        indices: &[usize],
        m: Tag,
        on: bool,
    ) -> Result<(), ConfigError> {
        // (an owner several repos share gets the same edit again: a no-op)
        let owners: Vec<RepoId> = indices
            .iter()
            .filter_map(|&i| self.entries[i].id.clone())
            .collect();
        for id in &owners {
            for (m, on) in exclusive(&self.config, m, on) {
                let base = m == Tag::Pull && self.tracked_pull(id);
                rules::set_owner_tag(&mut self.config, m, id, base, on)?;
            }
        }
        self.tags_changed()
    }

    /// Whether the owner of the repo at `i` is tagged `m` as a whole;
    /// `None` for a repo without an identity.
    pub fn owner_tagged(&self, i: usize, m: Tag) -> Option<bool> {
        let id = self.entries[i].id.as_ref()?;
        let decided = |m: Tag| -> Option<bool> {
            let base = m == Tag::Pull && self.tracked_pull(id);
            Some(
                rules::owner_decides(&self.config, m, id)
                    .ok()?
                    .unwrap_or(base),
            )
        };
        let ignored = decided(Tag::Ignore)?;
        // ignore wins, as for a repo, except over a label
        Some(match m {
            Tag::Ignore => ignored,
            Tag::Label(_) if m.points(&self.config) == 0 => decided(m)?,
            _ => decided(m)? && !ignored,
        })
    }

    /// Whether `id`'s owner is tracked with pulls on.
    fn tracked_pull(&self, id: &RepoId) -> bool {
        rules::track_for(&self.config, &self.state, id).is_some_and(|t| t.pull)
    }

    /// The tags the repo at `i` has without entries of its own (as after
    /// [`Self::clear_tags`]), each with what gives it: `favourite
    /// (acme/*)`, or for pull a tracked owner, `pull (tracking
    /// github.com/acme)`.
    pub fn remaining_tags(&self, i: usize) -> Vec<String> {
        let e = &self.entries[i];
        Tag::all(&self.config)
            .into_iter()
            .filter(|&m| m.of(&e.flags, &self.config))
            .filter_map(|m| {
                let why = match self.rules.tagging(m, e.id.as_ref(), &e.path) {
                    Some(entry) => entry.to_string(),
                    None => format!("tracking {}", e.flags.tracked.as_ref()?),
                };
                Some(format!("{} ({why})", m.label_text(&self.config)))
            })
            .collect()
    }

    /// How many of the repos at `indices` have entries of their own in the
    /// tag lists: clearing those of more than one needs confirmation.
    pub fn tagged_count(&self, indices: &[usize]) -> usize {
        indices
            .iter()
            .filter(|&&i| {
                let e = &self.entries[i];
                Tag::all(&self.config)
                    .into_iter()
                    .any(|m| !self.rules.own_entries(m, e.id.as_ref(), &e.path).is_empty())
            })
            .count()
    }

    fn tags_changed(&mut self) -> Result<(), ConfigError> {
        self.rules = RuleSet::new(&self.config)?;
        self.resolve_flags();
        Ok(())
    }

    pub fn refresh_all(&mut self) {
        let n = self.entries.len();
        let results = crate::trace::timed(
            "statuses",
            |_| format!("statuses of {n} {}", crate::plural(n, "repo", "repos")),
            || self.statuses(self.entries.iter().map(|e| e.path.as_path())),
        );
        let ids = self.config.local_forges();
        for (e, r) in self.entries.iter_mut().zip(results) {
            e.set_status(r, &ids);
        }
        let all = self.entries.iter().map(|e| e.path.clone()).collect();
        self.follow_statuses(all);
    }

    /// What follows from the statuses of `read`: the submodules and
    /// worktrees they name, listed with statuses of their own; each one's
    /// parent; a parent's lines for what's changed inside its submodules;
    /// flags.
    fn follow_statuses(&mut self, read: Vec<PathBuf>) {
        crate::trace::timed(
            "children",
            |n: &usize| format!("children: {n} found"),
            || self.add_children(read),
        );
        crate::trace::part("parents", || self.link_parents());
        crate::trace::part("submodule content", || self.show_submodule_content());
        crate::trace::part("flags", || self.resolve_flags());
    }

    /// Each entry's position by its path, for many lookups at once
    /// ([`Self::index_of`] searches them all).
    fn positions(&self) -> HashMap<PathBuf, usize> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect()
    }

    /// Re-read one repo's status (after an operation on it).
    pub fn refresh(&mut self, path: &Path) {
        let r = self.status_reader().read(path);
        self.update_status(path, r);
    }

    /// Apply a status computed elsewhere (e.g. on a worker thread).
    /// A repo whose folder is gone (deleted outside argus) shows its error
    /// once: it leaves the list when it was gone at the last refresh too.
    pub fn update_status(&mut self, path: &Path, r: Result<RepoStatus, String>) {
        self.update_statuses([(path.to_path_buf(), r)]);
    }

    /// [`Self::update_status`] for several at once: each applied, then
    /// what follows from them (children, parents, flags) worked out once
    /// for all, as a refresh of everything brings them in together.
    pub fn update_statuses(
        &mut self,
        statuses: impl IntoIterator<Item = (PathBuf, Result<RepoStatus, String>)>,
    ) {
        let ids = self.config.local_forges();
        let at = self.positions();
        let (mut gone, mut read) = (vec![], vec![]);
        for (path, r) in statuses {
            let Some(&i) = at.get(&path) else {
                continue;
            };
            if r.is_err() && path.symlink_metadata().is_err() && self.entries[i].error().is_some() {
                gone.push(i);
            } else {
                self.entries[i].set_status(r, &ids);
                read.push(i);
            }
        }
        gone.sort_unstable();
        gone.dedup();
        // (in the entries' order, as a scan takes them)
        read.sort_unstable();
        read.dedup();
        let read: Vec<PathBuf> = read
            .into_iter()
            .filter(|i| gone.binary_search(i).is_err())
            .map(|i| self.entries[i].path.clone())
            .collect();
        for i in gone.into_iter().rev() {
            self.entries.remove(i);
        }
        self.follow_statuses(read);
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

    /// Drop a repo from the list (one removed, or gone and acknowledged);
    /// a rescan finds it again if it's there.
    pub fn forget(&mut self, path: &Path) {
        if let Some(i) = self.index_of(path) {
            self.entries.remove(i);
        }
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    pub fn get(&self, path: &Path) -> Option<&Entry> {
        self.index_of(path).map(|i| &self.entries[i])
    }

    /// The repo at `rel` in `parent`'s work tree, as a nested entry (added,
    /// with its status, the first time); none if there's no repo there.
    pub fn nested(&mut self, parent: &Path, rel: &str) -> Option<usize> {
        let path = std::fs::canonicalize(parent.join(rel)).ok()?;
        if let Some(i) = self.index_of(&path) {
            return Some(i);
        }
        let kind = discover::classify(&path)?;
        let mut e = Entry::new(path.clone(), kind);
        e.parent = Some(parent.to_path_buf());
        e.nested = true;
        self.entries.push(e);
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        self.refresh(&path);
        self.index_of(&path)
    }

    /// The nested entry for the repo at `rel` in `parent`'s work tree, if
    /// [`Self::nested`] has added it.
    pub fn nested_index(&self, parent: &Path, rel: &str) -> Option<usize> {
        let path = std::fs::canonicalize(parent.join(rel)).ok()?;
        let i = self.index_of(&path)?;
        let e = &self.entries[i];
        (e.nested && e.parent.as_deref() == Some(parent)).then_some(i)
    }

    /// `path`, then its parent, grandparent…: what a change in a submodule
    /// or worktree may change the status of.
    pub fn with_parents(&self, path: &Path) -> Vec<PathBuf> {
        let mut v = vec![path.to_path_buf()];
        while let Some(p) = self.get(&v[v.len() - 1]).and_then(|e| e.parent.clone()) {
            if v.contains(&p) {
                break;
            }
            v.push(p);
        }
        v
    }

    /// Worktrees and submodules living outside the roots (or not walked into)
    /// are learnt from their main repo's status, of those `read`; then
    /// theirs from theirs (submodules may have submodules). How many were
    /// added.
    fn add_children(&mut self, read: Vec<PathBuf>) -> usize {
        let mut known: std::collections::HashSet<PathBuf> =
            self.entries.iter().map(|e| e.path.clone()).collect();
        // whose children to look for: those read, then those just added
        // (in path order, as the first to name a child is its parent)
        let mut parents = read;
        let mut added = 0;
        loop {
            let at = self.positions();
            let mut extra: BTreeMap<PathBuf, Entry> = BTreeMap::new();
            for parent in &parents {
                let Some(&i) = at.get(parent) else { continue };
                let e = &self.entries[i];
                if e.nested {
                    continue;
                }
                let Some(s) = e.status() else { continue };
                let kids = s.worktrees.iter().map(|p| (p, RepoKind::Worktree, false));
                let kids = kids.chain(s.submodules.iter().map(|p| (p, RepoKind::Submodule, false)));
                // a stale record's folder too: its row shows it's gone
                let kids = kids.chain(
                    s.stale_worktrees
                        .iter()
                        .map(|p| (p, RepoKind::Worktree, true)),
                );
                for (p, kind, stale) in kids {
                    // (listed already as it's named: no need to canonicalize)
                    if known.contains(p) {
                        continue;
                    }
                    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                    // listed as there, gone since the parent's status was read
                    // (a worktree just removed): not brought back
                    let removed = !stale && p.symlink_metadata().is_err();
                    if !known.contains(&p) && !removed {
                        extra.entry(p.clone()).or_insert_with(|| {
                            let mut child = Entry::new(p, kind);
                            child.parent = Some(e.path.clone());
                            child
                        });
                    }
                }
            }
            if extra.is_empty() {
                return added;
            }
            let extra: Vec<Entry> = extra.into_values().collect();
            let n = extra.len();
            let results = crate::trace::timed(
                "statuses",
                |_| format!("statuses of {n} more (submodules and worktrees of those before)"),
                || self.statuses(extra.iter().map(|e| e.path.as_path())),
            );
            let ids = self.config.local_forges();
            parents = extra.iter().map(|e| e.path.clone()).collect();
            for (mut e, r) in extra.into_iter().zip(results) {
                e.set_status(r, &ids);
                known.insert(e.path.clone());
                self.entries.push(e);
            }
            added += n;
            self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
    }

    /// Add to each parent's changes what's changed inside its submodules,
    /// from their own statuses (see
    /// [`StatusOptions::submodule_content_from_own`]), and work out its
    /// issues again: after every update, as either may have changed.
    /// Deepest first, as a submodule's changes include its own
    /// submodules'.
    ///
    /// [`StatusOptions::submodule_content_from_own`]: status::StatusOptions::submodule_content_from_own
    fn show_submodule_content(&mut self) {
        let at = self.positions();
        let mut parents: Vec<usize> = (0..self.entries.len())
            .filter(|&i| {
                self.entries[i]
                    .status()
                    .is_some_and(|s| s.own_changes.is_some())
            })
            .collect();
        parents.sort_by_key(|&i| std::cmp::Reverse(self.entries[i].path.components().count()));
        for i in parents {
            let Some(s) = self.entries[i].status() else {
                continue;
            };
            let Some(mut changes) = s.own_changes.clone() else {
                continue;
            };
            for sm in &s.submodules_shown {
                // (its path is the parent's, canonical as entries' are, and
                // where it is in it)
                let inside = at
                    .get(&sm.path)
                    .and_then(|&j| self.entries[j].status())
                    .and_then(|sub| sub.changes.as_submodule(sm.ignore));
                if let Some(code) = inside {
                    changes.add_submodule_content(&sm.rela, code);
                }
            }
            let e = &mut self.entries[i];
            if let Some(Ok(s)) = &mut e.status
                && s.changes != changes
            {
                s.changes = changes;
                // (none of today's issues counts what's inside a submodule,
                // but they're worked out from the status as shown)
                e.all_issues = attention::issues(s);
                e.issues = e.all_issues.clone();
            }
        }
    }

    fn link_parents(&mut self) {
        let listed: std::collections::HashSet<&Path> =
            self.entries.iter().map(|e| e.path.as_path()).collect();
        let mut parent_of: HashMap<PathBuf, PathBuf> = HashMap::new();
        for e in &self.entries {
            if let Some(s) = e.status() {
                for c in s.worktrees.iter().chain(&s.submodules) {
                    // (listed as it's named: canonical already)
                    let c = match listed.contains(c.as_path()) {
                        true => c.clone(),
                        false => std::fs::canonicalize(c).unwrap_or_else(|_| c.clone()),
                    };
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
            let (id, path) = (e.id.as_ref(), &e.path);
            let (mut on, mut off) = (vec![], vec![]);
            e.stash_ignored.clear();
            for &issue in &e.all_issues {
                match (issue, IssueList::of(issue)) {
                    // stash by stash: the counted ones and the ignored ones
                    (Issue::Stashes(n), Some(l)) => {
                        let values = l.values(s);
                        e.stash_ignored = values
                            .iter()
                            .map(|v| {
                                self.rules
                                    .ignores_issue(l, id, path, std::slice::from_ref(v))
                            })
                            .collect();
                        let ignored = e.stash_ignored.iter().filter(|&&i| i).count();
                        if n > ignored {
                            on.push(Issue::Stashes(n - ignored));
                        }
                        if ignored > 0 {
                            off.push(Issue::Stashes(ignored));
                        }
                    }
                    (_, Some(l)) if self.rules.ignores_issue(l, id, path, &l.values(s)) => {
                        off.push(issue)
                    }
                    _ => on.push(issue),
                }
            }
            let (snoozed, on): (Vec<Issue>, Vec<Issue>) = on
                .into_iter()
                .partition(|&i| self.state.is_issue_snoozed(&e.key, i, now));
            e.issues = on;
            e.ignored_issues = off;
            e.snoozed_issues = snoozed;
        }
    }

    /// Indices into `entries` in display order: top-level repos sorted by
    /// tags' sort points, severity, amount of change and name; each followed by its
    /// children (worktrees, submodules).
    pub fn ordered(&self, filter: ViewFilter) -> Vec<usize> {
        let visible = |e: &Entry| {
            (filter.show_hidden || !e.flags.hidden())
                && (!filter.attention_only || e.needs_attention())
        };
        // each listed entry's children, found once (not once per entry)
        let listed: HashSet<&Path> = self.entries.iter().map(|e| e.path.as_path()).collect();
        let mut children: HashMap<&Path, Vec<usize>> = HashMap::new();
        for (c, e) in self.entries.iter().enumerate().filter(|(_, e)| !e.nested) {
            if let Some(p) = &e.parent {
                children.entry(p).or_default().push(c);
            }
        }
        for kids in children.values_mut() {
            kids.sort_by_key(|&c| (self.entries[c].kind, &self.entries[c].path));
        }
        let is_top = |e: &Entry| {
            !e.nested
                && e.parent
                    .as_ref()
                    .is_none_or(|p| !listed.contains(p.as_path()))
        };
        let mut top: Vec<usize> = (0..self.entries.len())
            .filter(|&i| is_top(&self.entries[i]))
            .collect();
        top.sort_by(|&a, &b| {
            let (a, b) = (&self.entries[a], &self.entries[b]);
            b.needs_attention()
                .cmp(&a.needs_attention())
                .then(b.flags.sort.cmp(&a.flags.sort))
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
            self.collect_children(t, &children, &mut fam, &mut HashSet::from([t]));
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
    fn collect_children(
        &self,
        i: usize,
        children: &HashMap<&Path, Vec<usize>>,
        out: &mut Vec<usize>,
        seen: &mut HashSet<usize>,
    ) {
        let kids = children.get(self.entries[i].path.as_path());
        // (each once, whatever loops the parents make)
        let new: Vec<usize> = kids
            .into_iter()
            .flatten()
            .copied()
            .filter(|c| seen.insert(*c))
            .collect();
        for c in new {
            out.push(c);
            self.collect_children(c, children, out, seen);
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

    /// The folders walked for nothing, to skip with `exclude_paths`.
    pub fn barren_warning(&self) -> Option<String> {
        let names: Vec<String> = self.barren.iter().map(|(p, _)| contract_tilde(p)).collect();
        Some(match (&self.barren[..], &names[..]) {
            ([], _) => return None,
            ([(_, n)], [one]) => {
                format!("walked {n} folders in {one} and found no repo: exclude_paths skips it")
            }
            (all, names) => {
                let least = all.iter().map(|(_, n)| *n).min().unwrap_or_default();
                let shown = names.iter().take(3).cloned().collect::<Vec<_>>();
                let more = match names.len().saturating_sub(3) {
                    0 => String::new(),
                    n => format!(" and {n} more"),
                };
                // (two or more: one is said above)
                let n = shown.len();
                let last = match more.is_empty() {
                    true => format!("{} and {}", shown[..n - 1].join(", "), shown[n - 1]),
                    false => format!("{}{more}", shown.join(", ")),
                };
                format!(
                    "walked {least} folders or more in each of {last}, \
                     and found no repo: exclude_paths skips them"
                )
            }
        })
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

/// The tags to set for `m` `on`: ignore excludes pull and the tags with
/// sort points (which would raise a repo you hid); pure labels go with it.
fn exclusive(c: &Config, m: Tag, on: bool) -> Vec<(Tag, bool)> {
    match (m, on) {
        (Tag::Ignore, true) => std::iter::once((m, on))
            .chain(
                Tag::all(c)
                    .into_iter()
                    .filter(|&o| o == Tag::Pull || o.points(c) != 0)
                    .map(|o| (o, false)),
            )
            .collect(),
        (Tag::Label(_), true) if m.points(c) == 0 => vec![(m, on)],
        (_, true) => vec![(m, on), (Tag::Ignore, false)],
        _ => vec![(m, on)],
    }
}

/// Run `f` on a pool of `n` threads, so its parallel iterators use at most
/// that many (on the current thread if the pool can't be made).
pub fn in_pool<R: Send>(n: usize, f: impl FnOnce() -> R + Send) -> R {
    // (on the pool's thread, still in the caller's step)
    let here = crate::trace::within();
    match rayon::ThreadPoolBuilder::new().num_threads(n).build() {
        Ok(pool) => pool.install(|| here.run(f)),
        Err(_) => f(),
    }
}

impl Workspace {
    /// The status of each of `paths`, in order, [`Config::disk_jobs`] at a time.
    fn statuses<'a>(
        &self,
        paths: impl Iterator<Item = &'a Path>,
    ) -> Vec<Result<RepoStatus, String>> {
        let paths: Vec<&Path> = paths.collect();
        let reader = self.status_reader().together(paths.len());
        in_pool(self.config.disk_jobs(), || {
            let here = crate::trace::within();
            paths
                .par_iter()
                .map(|p| here.run(|| reader.read(p)))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Statuses read side by side take a thread each for their files; one
    /// read alone takes what gix likes (every core, for a big repo).
    #[test]
    fn statuses_read_together_take_a_thread_each() {
        let ws = Workspace::new(Config::default(), State::default()).unwrap();
        assert_eq!(ws.status_reader().opts.threads, None);
        assert_eq!(ws.status_reader().together(1).opts.threads, None);
        assert_eq!(ws.status_reader().together(2).opts.threads, Some(1));
    }
}
