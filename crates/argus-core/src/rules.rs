//! Resolution of per-repo flags from tracked owners, the `favourite`, `pull`
//! and `ignore.repos` lists in config, and state (snoozes, "new"); and which
//! issues the other `[ignore]` lists turn off.

use std::path::Path;

use globset::GlobMatcher;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::attention::Issue;
use crate::config::{Config, ConfigError, Track};
use crate::identity::RepoId;
use crate::paths::expand_tilde;
use crate::state::State;
use crate::status::{Head, RepoStatus};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    /// The `[marks]` the repo has, by name, in config order.
    #[serde(default)]
    pub marks: Vec<String>,
    /// The sum of their sort points.
    #[serde(default)]
    pub sort: i32,
    pub pull: bool,
    pub ignored: bool,
    pub snoozed: bool,
    /// Recently cloned/discovered by argus and not acknowledged.
    pub new: bool,
    /// Key of the tracked owner this repo belongs to.
    pub tracked: Option<String>,
}

impl Flags {
    /// Hidden from the default view and skipped by bulk operations.
    pub fn hidden(&self) -> bool {
        self.ignored || self.snoozed
    }

    /// Whether the repo has the mark named `name`.
    pub fn has(&self, name: &str) -> bool {
        self.marks.iter().any(|m| m == name)
    }
}

/// The per-repo settings kept as lists in config: each of `[marks]`, by its
/// place there, then `pull` and `ignore.repos`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mark {
    Label(usize),
    Pull,
    Ignore,
}

impl Mark {
    /// Every mark of `c`: its `[marks]` in order, then pull and ignore.
    pub fn all(c: &Config) -> Vec<Mark> {
        (0..c.marks.len())
            .map(Mark::Label)
            .chain([Mark::Pull, Mark::Ignore])
            .collect()
    }

    /// The mark of `c`'s `[marks]` named `name`.
    pub fn label(c: &Config, name: &str) -> Option<Mark> {
        c.marks.get_index_of(name).map(Mark::Label)
    }

    fn def(c: &Config, i: usize) -> (&String, &crate::config::MarkDef) {
        c.marks.get_index(i).expect("a mark of this config")
    }

    /// The config key, dotted.
    pub fn path(self, c: &Config) -> String {
        match self {
            Mark::Label(i) => format!("marks.{}.repos", Self::def(c, i).0),
            Mark::Pull => "pull".into(),
            Mark::Ignore => "ignore.repos".into(),
        }
    }

    pub fn list(self, c: &Config) -> &Vec<String> {
        match self {
            Mark::Label(i) => &Self::def(c, i).1.repos,
            Mark::Pull => &c.pull,
            Mark::Ignore => &c.ignore.repos,
        }
    }

    pub fn list_mut(self, c: &mut Config) -> &mut Vec<String> {
        match self {
            Mark::Label(i) => {
                &mut c
                    .marks
                    .get_index_mut(i)
                    .expect("a mark of this config")
                    .1
                    .repos
            }
            Mark::Pull => &mut c.pull,
            Mark::Ignore => &mut c.ignore.repos,
        }
    }

    pub fn of(self, f: &Flags, c: &Config) -> bool {
        match self {
            Mark::Label(i) => f.has(Self::def(c, i).0),
            Mark::Pull => f.pull,
            Mark::Ignore => f.ignored,
        }
    }

    /// How a repo with the mark is described: its name, `pull`, `ignored`.
    pub fn label_text(self, c: &Config) -> String {
        match self {
            Mark::Label(i) => Self::def(c, i).0.clone(),
            Mark::Pull => "pull".into(),
            Mark::Ignore => "ignored".into(),
        }
    }

    /// Its sort points: a mark with some gives way to ignoring.
    pub fn points(self, c: &Config) -> i32 {
        match self {
            Mark::Label(i) => Self::def(c, i).1.sort,
            _ => 0,
        }
    }
}

/// The mark terms of a filter (fzf's extended syntax, which `/` follows):
/// `#work` has the mark, `!#work` hasn't; space is AND, `|` OR. Groups
/// of mark terms joined by `|` each need one of theirs; every group must
/// hold. (OR is between mark terms only: fuzzy matching has none.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MarkFilter {
    /// Each group: (wanted, mark name) terms, one of which must hold.
    groups: Vec<Vec<(bool, String)>>,
}

impl MarkFilter {
    /// Split `query` into its mark terms and the rest, the text to match
    /// fuzzily (a group mixing marks and text is text).
    pub fn parse(query: &str) -> (Self, String) {
        let mut groups: Vec<Vec<&str>> = vec![];
        let mut join = false;
        for tok in query.split_whitespace() {
            if tok == "|" {
                join = !groups.is_empty();
                continue;
            }
            match groups.last_mut() {
                Some(g) if join => g.push(tok),
                _ => groups.push(vec![tok]),
            }
            join = false;
        }
        let term = |t: &str| -> Option<(bool, String)> {
            let (wanted, name) = match t.strip_prefix('!') {
                Some(n) => (false, n),
                None => (true, t),
            };
            let name = name.strip_prefix('#').filter(|n| !n.is_empty())?;
            Some((wanted, name.to_string()))
        };
        let mut f = Self::default();
        let mut rest = vec![];
        for g in groups {
            match g.iter().map(|t| term(t)).collect::<Option<Vec<_>>>() {
                Some(terms) => f.groups.push(terms),
                None => rest.extend(g),
            }
        }
        (f, rest.join(" "))
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether a repo with these flags passes.
    pub fn matches(&self, f: &Flags) -> bool {
        self.groups
            .iter()
            .all(|g| g.iter().any(|(wanted, name)| f.has(name) == *wanted))
    }
}

/// Issues that can be ignored for some repos: the `[ignore]` lists other
/// than `repos`. Conflicts, operations in progress and commits only a
/// detached HEAD holds can't be: they risk losing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IssueList {
    NonDefaultBranch,
    Detached,
    NoRemote,
    NoUpstream,
    Stashes,
    UnpushedBranches,
    Behind,
}

impl IssueList {
    pub const ALL: [IssueList; 7] = [
        IssueList::NonDefaultBranch,
        IssueList::Detached,
        IssueList::NoRemote,
        IssueList::NoUpstream,
        IssueList::Stashes,
        IssueList::UnpushedBranches,
        IssueList::Behind,
    ];

    /// The key in `[ignore]`.
    pub fn name(self) -> &'static str {
        match self {
            IssueList::NonDefaultBranch => "non_default_branch",
            IssueList::Detached => "detached",
            IssueList::NoRemote => "no_remote",
            IssueList::NoUpstream => "no_upstream",
            IssueList::Stashes => "stashes",
            IssueList::UnpushedBranches => "unpushed_branches",
            IssueList::Behind => "behind",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.name() == name || l.name().replace('_', "-") == name)
    }

    /// The list that can ignore `issue`, if any.
    pub fn of(issue: Issue) -> Option<Self> {
        Some(match issue {
            Issue::NonDefaultBranch => IssueList::NonDefaultBranch,
            Issue::Detached => IssueList::Detached,
            Issue::NoRemote => IssueList::NoRemote,
            Issue::NoUpstream => IssueList::NoUpstream,
            Issue::Stashes(_) => IssueList::Stashes,
            Issue::UnpushedBranches(_) => IssueList::UnpushedBranches,
            Issue::Behind(_) => IssueList::Behind,
            _ => return None,
        })
    }

    /// Whether entries can pin a value (`repo@value`).
    pub fn pins(self) -> bool {
        matches!(
            self,
            IssueList::NonDefaultBranch
                | IssueList::Detached
                | IssueList::NoUpstream
                | IssueList::Stashes
        )
    }

    /// Whether ignoring from the TUI or `argus mark` pins by default: where
    /// HEAD is, yes (a repo that moves is flagged again); stashes, no.
    pub fn pins_by_default(self) -> bool {
        self.pins() && !self.separate_values()
    }

    /// Whether each value is a thing of its own, ignored or not by itself
    /// (stashes), rather than another name for one thing (a detached HEAD's
    /// tags, describe and commit).
    pub fn separate_values(self) -> bool {
        self == IssueList::Stashes
    }

    /// What a pin is matched against in this state; pinning writes the
    /// first, or each of [`separate_values`](Self::separate_values). Empty
    /// for lists that don't pin.
    pub fn values(self, s: &RepoStatus) -> Vec<String> {
        match self {
            IssueList::Stashes => s.stashes.clone(),
            IssueList::NonDefaultBranch | IssueList::NoUpstream => {
                s.head.branch().map(String::from).into_iter().collect()
            }
            IssueList::Detached => {
                let Head::Detached(_) = &s.head else {
                    return vec![];
                };
                let mut v = s.tags.clone();
                v.extend(s.describe.clone());
                if let Some(id) = &s.head_commit {
                    v.push(id.chars().take(7).collect());
                    v.push(id.clone());
                }
                v
            }
            _ => vec![],
        }
    }

    pub fn list(self, c: &Config) -> &Vec<String> {
        let i = &c.ignore;
        match self {
            IssueList::NonDefaultBranch => &i.non_default_branch,
            IssueList::Detached => &i.detached,
            IssueList::NoRemote => &i.no_remote,
            IssueList::NoUpstream => &i.no_upstream,
            IssueList::Stashes => &i.stashes,
            IssueList::UnpushedBranches => &i.unpushed_branches,
            IssueList::Behind => &i.behind,
        }
    }

    pub fn list_mut(self, c: &mut Config) -> &mut Vec<String> {
        let i = &mut c.ignore;
        match self {
            IssueList::NonDefaultBranch => &mut i.non_default_branch,
            IssueList::Detached => &mut i.detached,
            IssueList::NoRemote => &mut i.no_remote,
            IssueList::NoUpstream => &mut i.no_upstream,
            IssueList::Stashes => &mut i.stashes,
            IssueList::UnpushedBranches => &mut i.unpushed_branches,
            IssueList::Behind => &mut i.behind,
        }
    }
}

/// Any list of repos kept in config: the marks and the issue lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Mark(Mark),
    Issue(IssueList),
}

impl List {
    /// Every list of `c`.
    pub fn all(c: &Config) -> impl Iterator<Item = List> {
        Mark::all(c)
            .into_iter()
            .map(List::Mark)
            .chain(IssueList::ALL.into_iter().map(List::Issue))
    }

    /// The config key, dotted (`ignore.detached`).
    pub fn path(self, c: &Config) -> String {
        match self {
            List::Mark(m) => m.path(c),
            List::Issue(l) => format!("ignore.{}", l.name()),
        }
    }

    pub fn get(self, c: &Config) -> &Vec<String> {
        match self {
            List::Mark(m) => m.list(c),
            List::Issue(l) => l.list(c),
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

/// An `[ignore]` issue list entry: the repo part, and the pinned value
/// after `@`, if any.
#[derive(Clone)]
struct IssueEntry {
    /// As written.
    text: String,
    repo: Entry,
    value: Option<GlobMatcher>,
}

impl IssueEntry {
    fn new(list: IssueList, text: &str) -> Result<Self, ConfigError> {
        let bad = |reason| ConfigError::IgnoreEntry {
            list: list.name(),
            entry: text.to_string(),
            reason,
        };
        // at the first `@`: a value (a stash message) may hold another
        let (repo, value) = match text.split_once('@') {
            Some(_) if !list.pins() => return Err(bad("this list doesn't take @value")),
            Some((repo, value)) => {
                if value.is_empty() {
                    return Err(bad("nothing after @"));
                }
                let m = globset::Glob::new(value)
                    .map_err(|source| ConfigError::Pattern {
                        pattern: text.to_string(),
                        source,
                    })?
                    .compile_matcher();
                (repo, Some(m))
            }
            None => (text, None),
        };
        Ok(Self {
            text: text.to_string(),
            repo: Entry::new(repo)?,
            value,
        })
    }

    fn matches(&self, id: Option<&RepoId>, path: &Path, values: &[String]) -> bool {
        self.repo.matches(id, path)
            && self
                .value
                .as_ref()
                .is_none_or(|m| values.iter().any(|v| m.is_match(v)))
    }
}

/// The `[marks]`, `pull` and `ignore` lists, compiled.
#[derive(Clone)]
pub struct RuleSet {
    labels: Vec<Vec<Entry>>,
    pull: Vec<Entry>,
    ignore: Vec<Entry>,
    issues: Vec<Vec<IssueEntry>>,
}

impl RuleSet {
    pub fn new(config: &Config) -> Result<Self, ConfigError> {
        let compile = |m: Mark| -> Result<Vec<Entry>, ConfigError> {
            m.list(config).iter().map(|t| Entry::new(t)).collect()
        };
        let issues = IssueList::ALL
            .into_iter()
            .map(|l| {
                l.list(config)
                    .iter()
                    .map(|t| IssueEntry::new(l, t))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            labels: (0..config.marks.len())
                .map(|i| compile(Mark::Label(i)))
                .collect::<Result<_, _>>()?,
            pull: compile(Mark::Pull)?,
            ignore: compile(Mark::Ignore)?,
            issues,
        })
    }

    fn entries(&self, m: Mark) -> &[Entry] {
        match m {
            Mark::Label(i) => self.labels.get(i).map_or(&[], Vec::as_slice),
            Mark::Pull => &self.pull,
            Mark::Ignore => &self.ignore,
        }
    }

    /// What `m`'s list says about the repo: its last matching entry, if any.
    pub fn decide(&self, m: Mark, id: Option<&RepoId>, path: &Path) -> Option<bool> {
        self.entries(m)
            .iter()
            .rev()
            .find(|e| e.matches(id, path))
            .map(|e| !e.negate)
    }

    /// The entry that turns `m` on for the repo, if one does (its last
    /// matching entry, when that isn't a `!` one).
    pub fn marking(&self, m: Mark, id: Option<&RepoId>, path: &Path) -> Option<&str> {
        self.entries(m)
            .iter()
            .rev()
            .find(|e| e.matches(id, path))
            .filter(|e| !e.negate)
            .map(|e| e.text.as_str())
    }

    /// Whether a (possibly not yet cloned) repo is ignored.
    pub fn ignores(&self, id: &RepoId, path: &Path) -> bool {
        self.decide(Mark::Ignore, Some(id), path).unwrap_or(false)
    }

    /// Whether `list` ignores its issue for the repo in a state with these
    /// [`IssueList::values`]: its last matching entry, if any.
    pub fn ignores_issue(
        &self,
        list: IssueList,
        id: Option<&RepoId>,
        path: &Path,
        values: &[String],
    ) -> bool {
        self.issues[list as usize]
            .iter()
            .rev()
            .find(|e| e.matches(id, path, values))
            .is_some_and(|e| !e.repo.negate)
    }

    /// Entries of `list` naming this repo itself (not patterns), whatever
    /// value they pin, as written.
    /// Where the repo's `list` issue stands for these values: ignored for
    /// all of them, pinned (by entries of its own) or not; else counted.
    pub fn issue_state(
        &self,
        list: IssueList,
        id: Option<&RepoId>,
        path: &Path,
        values: &[String],
    ) -> IgnoreState {
        if !self.ignores_all(list, id, path, values) {
            return IgnoreState::Counted;
        }
        let own = self.own_issue_entries(list, id, path);
        let positive = |e: &&String| !e.starts_with('!');
        let pinned = own.iter().filter(positive).any(|e| e.contains('@'));
        let whole = own.iter().filter(positive).any(|e| !e.contains('@'));
        if list.pins() && pinned && !whole {
            IgnoreState::Pinned
        } else {
            IgnoreState::Ignored
        }
    }

    fn own_issue_entries(&self, list: IssueList, id: Option<&RepoId>, path: &Path) -> Vec<String> {
        self.issues[list as usize]
            .iter()
            .filter(|e| e.repo.literal && e.repo.matches(id, path))
            .map(|e| e.text.clone())
            .collect()
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

/// The pattern naming every repo of an owner: `github.com/acme/*`.
pub fn owner_pattern(id: &RepoId) -> String {
    format!("{}/{}/*", id.host, id.owner)
}

/// What `m`'s list says for `id`'s owner as a whole: as for a repo of its
/// with no entries of its own (`None` when nothing matches).
pub fn owner_decides(config: &Config, m: Mark, id: &RepoId) -> Result<Option<bool>, ConfigError> {
    // a name no repo has
    let probe = RepoId::new(&id.host, &id.owner, "\u{0}");
    Ok(RuleSet::new(config)?.decide(m, Some(&probe), Path::new("")))
}

/// Edit `m`'s list so `id`'s owner as a whole ends up `on`, as [`set_mark`]
/// does for a repo: drop the owner pattern's entries, then add it (or `!`
/// it) only if what's left (or `base`) doesn't already give `on`.
pub fn set_owner_mark(
    config: &mut Config,
    m: Mark,
    id: &RepoId,
    base: bool,
    on: bool,
) -> Result<(), ConfigError> {
    let pattern = owner_pattern(id);
    m.list_mut(config)
        .retain(|e| e.strip_prefix('!').unwrap_or(e) != pattern);
    if owner_decides(config, m, id)?.unwrap_or(base) != on {
        let entry = if on { pattern } else { format!("!{pattern}") };
        m.list_mut(config).push(entry);
    }
    Ok(())
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

impl RuleSet {
    /// Which of `values` `list` ignores for the repo, each by itself, for
    /// lists of [`IssueList::separate_values`].
    pub fn ignored_values<'v>(
        &self,
        list: IssueList,
        id: Option<&RepoId>,
        path: &Path,
        values: &'v [String],
    ) -> impl Iterator<Item = &'v String> {
        values
            .iter()
            .filter(move |v| self.ignores_issue(list, id, path, std::slice::from_ref(*v)))
    }

    /// Whether `list` ignores the issue for the repo in this state: for
    /// separate values, every one of them.
    fn ignores_all(
        &self,
        list: IssueList,
        id: Option<&RepoId>,
        path: &Path,
        values: &[String],
    ) -> bool {
        if list.separate_values() {
            self.ignored_values(list, id, path, values).count() == values.len()
        } else {
            self.ignores_issue(list, id, path, values)
        }
    }
}

/// Edit `list` in `config` so the issue is ignored (`on`) or not for the
/// repo in its current state (`values`): drop the entries naming it, then,
/// only if what's left doesn't already give `on`, add `!name`, `name`, or
/// with `pin` (for lists that pin) `name@value`: the first value, or one
/// entry for each separate value not already ignored. Values are written as literals, glob
/// characters escaped.
#[allow(clippy::too_many_arguments)]
pub fn set_issue_ignored(
    config: &mut Config,
    list: IssueList,
    id: Option<&RepoId>,
    path: &Path,
    name: &str,
    values: &[String],
    on: bool,
    pin: bool,
) -> Result<(), ConfigError> {
    let own = RuleSet::new(config)?.own_issue_entries(list, id, path);
    list.list_mut(config).retain(|e| !own.contains(e));
    let rules = RuleSet::new(config)?;
    if rules.ignores_all(list, id, path, values) == on {
        return Ok(());
    }
    // separate values: pin those what's left doesn't ignore already
    let uncovered: Vec<String> = values
        .iter()
        .filter(|v| !rules.ignores_issue(list, id, path, std::slice::from_ref(*v)))
        .cloned()
        .collect();
    let pinned = |v: &String| format!("{name}@{}", globset::escape(v));
    let entries: Vec<String> = match (on, pin && list.pins()) {
        (false, _) => vec![format!("!{name}")],
        (true, true) if list.separate_values() && !uncovered.is_empty() => {
            uncovered.iter().map(pinned).collect()
        }
        (true, true) if !values.is_empty() => vec![pinned(&values[0])],
        (true, _) => vec![name.to_string()],
    };
    let mut seen = std::collections::HashSet::new();
    let entries = entries.into_iter().filter(|e| seen.insert(e.clone())); // stashes may share a message
    list.list_mut(config).extend(entries);
    Ok(())
}

/// Where a repo's issue stands in its `[ignore]` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreState {
    /// Not ignored: it counts towards attention.
    Counted,
    /// Ignored only for its current value(s): this branch, this HEAD,
    /// these stashes.
    Pinned,
    /// Ignored whatever the value.
    Ignored,
}

impl IgnoreState {
    /// What ignoring `list`'s issue means unless asked otherwise.
    pub fn default_for(list: IssueList) -> Self {
        if list.pins() && list.pins_by_default() {
            IgnoreState::Pinned
        } else {
            IgnoreState::Ignored
        }
    }

    /// The next state round: counted, the list's default way of ignoring,
    /// the other way (for lists that pin), counted again.
    pub fn next(self, list: IssueList) -> Self {
        let first = Self::default_for(list);
        match self {
            IgnoreState::Counted => first,
            s if s == first && list.pins() => match first {
                IgnoreState::Pinned => IgnoreState::Ignored,
                _ => IgnoreState::Pinned,
            },
            _ => IgnoreState::Counted,
        }
    }
}

/// Put the repo's `list` issue in `state`: its own entries replaced by
/// what that takes given the patterns. Counted: `!name` if patterns would
/// ignore any value. Pinned: `name@value` for each (the first, for lists
/// with one), after `!name` if patterns would ignore others too. Ignored:
/// `name`, unless patterns already ignore it.
#[allow(clippy::too_many_arguments)]
pub fn set_issue_state(
    config: &mut Config,
    list: IssueList,
    id: Option<&RepoId>,
    path: &Path,
    name: &str,
    values: &[String],
    state: IgnoreState,
) -> Result<(), ConfigError> {
    let own = RuleSet::new(config)?.own_issue_entries(list, id, path);
    list.list_mut(config).retain(|e| !own.contains(e));
    let rules = RuleSet::new(config)?;
    // with values, an entry ignoring the issue ignores one of them
    let any_ignored = rules
        .ignored_values(list, id, path, values)
        .next()
        .is_some()
        || rules.ignores_issue(list, id, path, values);
    let pinned = |v: &String| format!("{name}@{}", globset::escape(v));
    let state = match state {
        IgnoreState::Pinned if !list.pins() || values.is_empty() => IgnoreState::Ignored,
        s => s,
    };
    let entries: Vec<String> = match state {
        IgnoreState::Counted if any_ignored => vec![format!("!{name}")],
        IgnoreState::Counted => vec![],
        IgnoreState::Ignored if rules.ignores_all(list, id, path, values) => vec![],
        IgnoreState::Ignored => vec![name.to_string()],
        IgnoreState::Pinned => {
            let mut v = vec![];
            if any_ignored {
                v.push(format!("!{name}"));
            }
            if list.separate_values() {
                v.extend(values.iter().map(pinned));
            } else {
                v.push(pinned(&values[0]));
            }
            v
        }
    };
    let mut seen = std::collections::HashSet::new();
    let entries = entries.into_iter().filter(|e| seen.insert(e.clone())); // stashes may share a message
    list.list_mut(config).extend(entries);
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

/// Precedence (lowest to highest): track membership (pull), then each list's
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
        f.pull = t.pull;
        f.tracked = Some(t.key());
    }
    let mut labels = vec![];
    for m in Mark::all(config) {
        if let Some(v) = rules.decide(m, id, path) {
            match m {
                Mark::Label(_) if v => labels.push(m),
                Mark::Label(_) => {}
                Mark::Pull => f.pull = v,
                Mark::Ignore => f.ignored = v,
            }
        }
    }
    // an ignored repo is neither pulled nor raised by marks with points,
    // whatever patterns say; labels stay
    if f.ignored {
        f.pull = false;
        labels.retain(|m| m.points(config) == 0);
    }
    f.sort = labels.iter().map(|m| m.points(config)).sum();
    f.marks = labels.iter().map(|m| m.label_text(config)).collect();
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
        flags(cfg, &State::default(), url, path).has("favourite")
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
        assert!(fav(
            "marks.favourite.repos = ['github.com/acme/*']",
            URL,
            "/x"
        ));
        assert!(fav("marks.favourite.repos = ['acme/widget']", URL, "/x"));
        assert!(fav("marks.favourite.repos = ['/r/**']", URL, "/r/a/widget"));
        assert!(
            fav("marks.favourite.repos = ['ACME/*']", URL, "/x"),
            "case-insensitive"
        );
        // inverse: other owners, and `*` must not cross `/`
        assert!(!fav(
            "marks.favourite.repos = ['github.com/other/*']",
            URL,
            "/x"
        ));
        assert!(!fav("marks.favourite.repos = ['github.com/*']", URL, "/x"));
        assert!(!fav("marks.favourite.repos = ['/r/*']", URL, "/r/a/widget"));
    }

    /// Whether `[ignore] <list>` in `cfg` ignores the issue for the widget
    /// repo in a state with these values.
    fn ignores(cfg: &str, list: IssueList, values: &[&str]) -> bool {
        let c = Config::parse(cfg).unwrap();
        let values: Vec<String> = values.iter().map(|v| v.to_string()).collect();
        RuleSet::new(&c).unwrap().ignores_issue(
            list,
            RepoId::from_url(URL).as_ref(),
            Path::new("/r/widget"),
            &values,
        )
    }

    #[test]
    fn issue_lists_pin_values_and_take_globs() {
        let off = IssueList::NonDefaultBranch;
        assert!(ignores(
            "[ignore]\nnon_default_branch = ['acme/widget']",
            off,
            &["any"]
        ));
        assert!(ignores(
            "[ignore]\nnon_default_branch = ['acme/widget@release-2']",
            off,
            &["release-2"]
        ));
        assert!(ignores(
            "[ignore]\nnon_default_branch = ['acme/*@release-*']",
            off,
            &["release-3"]
        ));
        // inverse: another value, another repo, or no list at all
        assert!(!ignores(
            "[ignore]\nnon_default_branch = ['acme/widget@release-2']",
            off,
            &["main"]
        ));
        assert!(!ignores(
            "[ignore]\nnon_default_branch = ['acme/other']",
            off,
            &["x"]
        ));
        assert!(!ignores("", off, &["x"]));
        // detached: any of the values matches (a tag, the describe, the id)
        let det = IssueList::Detached;
        let state = ["v1.2", "v1.2-0-g1a2b3c4", "1a2b3c4", "1a2b3c4d5e6f"];
        assert!(ignores(
            "[ignore]\ndetached = ['acme/widget@1a2b3c4']",
            det,
            &state
        ));
        assert!(ignores(
            "[ignore]\ndetached = ['acme/widget@v1.*']",
            det,
            &state
        ));
        assert!(!ignores(
            "[ignore]\ndetached = ['acme/widget@v2.0']",
            det,
            &state
        ));
        // the last match wins; ! turns it off
        assert!(!ignores(
            "[ignore]\nstashes = ['acme/*', '!acme/widget']",
            IssueList::Stashes,
            &[]
        ));
        assert!(ignores(
            "[ignore]\nstashes = ['!acme/*', 'acme/widget']",
            IssueList::Stashes,
            &[]
        ));
    }

    #[test]
    fn issue_lists_by_name_and_by_issue() {
        assert_eq!(
            IssueList::from_name("non-default-branch"),
            Some(IssueList::NonDefaultBranch)
        );
        assert_eq!(
            IssueList::from_name("non_default_branch"),
            Some(IssueList::NonDefaultBranch)
        );
        assert_eq!(
            IssueList::from_name("unpushed-branches"),
            Some(IssueList::UnpushedBranches)
        );
        assert_eq!(IssueList::from_name("dirty"), None);
        for l in IssueList::ALL {
            assert_eq!(IssueList::from_name(l.name()), Some(l));
        }
        for (issue, list) in [
            (Issue::NonDefaultBranch, IssueList::NonDefaultBranch),
            (Issue::Detached, IssueList::Detached),
            (Issue::NoRemote, IssueList::NoRemote),
            (Issue::NoUpstream, IssueList::NoUpstream),
            (Issue::Stashes(2), IssueList::Stashes),
            (Issue::UnpushedBranches(1), IssueList::UnpushedBranches),
            (Issue::Behind(4), IssueList::Behind),
        ] {
            assert_eq!(IssueList::of(issue), Some(list), "{issue:?}");
        }
        // inverse: issues that risk losing work
        for i in [
            Issue::Conflicts(1),
            Issue::InProgress,
            Issue::DetachedCommits(1),
            Issue::Dirty(3),
        ] {
            assert_eq!(IssueList::of(i), None, "{i:?}");
        }
    }

    #[test]
    fn only_some_issue_lists_take_a_value() {
        for (cfg, ok) in [
            ("[ignore]\nstashes = ['acme/widget@wip']", true),
            ("[ignore]\nstashes = ['acme/widget@']", false),
            ("[ignore]\nbehind = ['acme/widget@x']", false),
            ("[ignore]\nunpushed_branches = ['acme/widget@x']", false),
            ("[ignore]\nno_remote = ['acme/widget@x']", false),
            ("[ignore]\nnon_default_branch = ['acme/widget@']", false),
            ("[ignore]\nno_upstream = ['acme/widget@feat']", true),
            ("[ignore]\nnon_default_branch = ['acme/widget@feat']", true),
        ] {
            let c = Config::parse(cfg).unwrap();
            assert_eq!(RuleSet::new(&c).is_ok(), ok, "{cfg}");
        }
    }

    #[test]
    fn ignoring_an_issue_pins_its_value_and_unignoring_removes_it() {
        let id = RepoId::from_url(URL);
        let path = Path::new("/r/widget");
        let mut c = Config::default();
        let values = vec!["release-2".to_string()];
        let set = |c: &mut Config, on| {
            set_issue_ignored(
                c,
                IssueList::NonDefaultBranch,
                id.as_ref(),
                path,
                "acme/widget",
                &values,
                on,
                true,
            )
            .unwrap()
        };
        set(&mut c, true);
        assert_eq!(c.ignore.non_default_branch, ["acme/widget@release-2"]);
        set(&mut c, true);
        assert_eq!(
            c.ignore.non_default_branch,
            ["acme/widget@release-2"],
            "once"
        );
        set(&mut c, false);
        assert!(c.ignore.non_default_branch.is_empty());
        // a pattern ignores it: unignoring adds `!`
        c.ignore.non_default_branch = vec!["acme/*".into()];
        set(&mut c, false);
        assert_eq!(c.ignore.non_default_branch, ["acme/*", "!acme/widget"]);
        // lists without values, or without one now, aren't pinned
        for list in [IssueList::Behind, IssueList::NonDefaultBranch] {
            let mut c = Config::default();
            set_issue_ignored(
                &mut c,
                list,
                id.as_ref(),
                path,
                "acme/widget",
                &[],
                true,
                true,
            )
            .unwrap();
            assert_eq!(list.list(&c), &["acme/widget"], "{list:?}");
        }
    }

    #[test]
    fn issue_states_round_trip_through_entries() {
        use IgnoreState::*;
        let id = RepoId::from_url(URL);
        let path = Path::new("/r/widget");
        let go = |c: &mut Config, list: IssueList, values: &[String], to| {
            set_issue_state(c, list, id.as_ref(), path, "acme/widget", values, to).unwrap();
            RuleSet::new(c)
                .unwrap()
                .issue_state(list, id.as_ref(), path, values)
        };
        // a branch issue: pinned to the branch, or any branch
        let branch = vec!["feat".to_string()];
        let mut c = Config::default();
        let nd = IssueList::NonDefaultBranch;
        assert_eq!(go(&mut c, nd, &branch, Pinned), Pinned);
        assert_eq!(c.ignore.non_default_branch, ["acme/widget@feat"]);
        assert_eq!(go(&mut c, nd, &branch, Ignored), Ignored);
        assert_eq!(c.ignore.non_default_branch, ["acme/widget"]);
        assert_eq!(go(&mut c, nd, &branch, Counted), Counted);
        assert!(c.ignore.non_default_branch.is_empty());
        // a list that doesn't pin ignores, whatever values it's given
        let mut c = Config::default();
        assert_eq!(go(&mut c, IssueList::Behind, &branch, Pinned), Ignored);
        assert_eq!(c.ignore.behind, ["acme/widget"]);
        // stashes, under the default patterns (all but argus's own)
        let stashes = vec!["wip".to_string(), "argus 2026-01-01 10:00".to_string()];
        let mut c = Config::default();
        let st = IssueList::Stashes;
        let defaults = c.ignore.stashes.clone();
        assert_eq!(
            RuleSet::new(&c)
                .unwrap()
                .issue_state(st, id.as_ref(), path, &stashes),
            Counted,
            "argus's stash still counts"
        );
        assert_eq!(go(&mut c, st, &stashes, Ignored), Ignored);
        assert_eq!(c.ignore.stashes[defaults.len()..], ["acme/widget"]);
        // pinned: these two only, so the patterns are overruled first
        assert_eq!(go(&mut c, st, &stashes, Pinned), Pinned);
        assert_eq!(
            c.ignore.stashes[defaults.len()..],
            [
                "!acme/widget",
                "acme/widget@wip",
                "acme/widget@argus 2026-01-01 10:00"
            ]
        );
        let later = ["wip".to_string(), "newer".to_string()];
        assert!(
            !RuleSet::new(&c)
                .unwrap()
                .ignores_issue(st, id.as_ref(), path, &later[1..])
        );
        // counted: overrules the patterns
        assert_eq!(go(&mut c, st, &stashes, Counted), Counted);
        assert_eq!(c.ignore.stashes[defaults.len()..], ["!acme/widget"]);
        // a list that doesn't pin goes straight to ignored
        let mut c = Config::default();
        assert_eq!(go(&mut c, IssueList::Behind, &[], Pinned), Ignored);
        assert_eq!(c.ignore.behind, ["acme/widget"]);
        // ignored by a pattern alone: ignored, not pinned, and asking for
        // ignored writes nothing more
        let mut c = Config::default();
        c.ignore.non_default_branch = vec!["acme/*".into()];
        let rules = RuleSet::new(&c).unwrap();
        assert_eq!(rules.issue_state(nd, id.as_ref(), path, &branch), Ignored);
        assert_eq!(go(&mut c, nd, &branch, Ignored), Ignored);
        assert_eq!(c.ignore.non_default_branch, ["acme/*"]);
    }

    #[test]
    fn the_state_cycle_starts_with_each_lists_default() {
        use IgnoreState::*;
        let nd = IssueList::NonDefaultBranch;
        assert_eq!(Counted.next(nd), Pinned);
        assert_eq!(Pinned.next(nd), Ignored);
        assert_eq!(Ignored.next(nd), Counted);
        let st = IssueList::Stashes;
        assert_eq!(Counted.next(st), Ignored);
        assert_eq!(Ignored.next(st), Pinned);
        assert_eq!(Pinned.next(st), Counted);
        // two states for a list that doesn't pin
        let b = IssueList::Behind;
        assert_eq!(Counted.next(b), Ignored);
        assert_eq!(Ignored.next(b), Counted);
    }

    #[test]
    fn later_entries_win_and_bang_turns_off() {
        let cfg = "pull = ['acme/*', '!acme/widget']";
        assert!(!flags(cfg, &State::default(), URL, "/x").pull);
        assert!(
            flags(
                cfg,
                &State::default(),
                "git@github.com:acme/other.git",
                "/x"
            )
            .pull
        );
        // order matters: a later pattern turns it back on
        assert!(fav(
            "marks.favourite.repos = ['!acme/widget', 'acme/*']",
            URL,
            "/x"
        ));
        // the lists are independent
        let f = flags(
            "marks.favourite.repos = ['acme/*']\nignore.repos = ['!acme/*']",
            &State::default(),
            URL,
            "/x",
        );
        assert!(f.has("favourite") && !f.ignored && !f.pull);
    }

    #[test]
    fn tracked_owner_implies_pull_unless_overridden() {
        let s = State::default();
        let f = flags("[[track]]\nowner='acme'", &s, URL, "/x");
        assert!(f.pull);
        assert_eq!(f.tracked.as_deref(), Some("github.com/acme"));
        assert!(!flags("[[track]]\nowner='acme'\npull=false", &s, URL, "/x").pull);
        assert!(
            !flags(
                "pull = ['!acme/widget']\n[[track]]\nowner='acme'",
                &s,
                URL,
                "/x"
            )
            .pull
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
        assert!(flags(cfg, &s, URL, "/x").pull);
        assert!(!flags(cfg, &s, "git@github.com:acme/notinteam.git", "/x").pull);
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
            "ignore.repos = ['/local/**']",
            &State::default(),
            "",
            "/local/thing",
        );
        assert!(f.ignored);
    }

    /// `#work` has it, `!#work` hasn't, space is AND, `|` OR between marks;
    /// the rest is text.
    #[test]
    fn mark_filters_parse_and_match() {
        let f = |marks: &[&str]| Flags {
            marks: marks.iter().map(|s| s.to_string()).collect(),
            ..Flags::default()
        };
        let (work, home, both, none) = (f(&["work"]), f(&["home"]), f(&["work", "home"]), f(&[]));
        let pass = |q: &str, fl: &Flags| MarkFilter::parse(q).0.matches(fl);
        assert!(pass("#work", &work) && !pass("#work", &home));
        assert!(pass("!#work", &home) && !pass("!#work", &work));
        assert!(
            pass("#work #home", &both) && !pass("#work #home", &work),
            "AND"
        );
        assert!(
            pass("#work | #home", &work) && pass("#work | #home", &home),
            "OR"
        );
        assert!(!pass("#work | #home", &none));
        assert!(pass("#work | #home !#home", &work) && !pass("#work | #home !#home", &both));
        // the rest is text; a group mixing both is text
        let (m, rest) = MarkFilter::parse("api #work | #home acme");
        assert_eq!(rest, "api acme");
        assert!(m.matches(&work) && !m.matches(&none));
        let (m, rest) = MarkFilter::parse("#work | api");
        assert!(m.is_empty() && rest == "#work api", "{rest}");
        let (m, rest) = MarkFilter::parse("  ");
        assert!(m.is_empty() && rest.is_empty() && m.matches(&none));
        // not marks: a bare `#`, a leading `|`, `!` alone
        let (m, rest) = MarkFilter::parse("| # ! !x");
        assert!(m.is_empty(), "{m:?}");
        assert_eq!(rest, "# ! !x");
    }

    #[test]
    fn each_mark_reads_its_own_flag() {
        let c = Config::parse("[marks.work]\n").unwrap();
        let (fav, work) = (Mark::Label(0), Mark::Label(1));
        let f = |marks: &[&str], pull, ignored| Flags {
            marks: marks.iter().map(|s| s.to_string()).collect(),
            pull,
            ignored,
            ..Flags::default()
        };
        assert!(fav.of(&f(&["favourite"], false, false), &c));
        assert!(work.of(&f(&["work"], false, false), &c));
        assert!(Mark::Pull.of(&f(&[], true, false), &c));
        assert!(Mark::Ignore.of(&f(&[], false, true), &c));
        // inverse: not the others'
        for m in Mark::all(&c) {
            assert!(!m.of(&f(&[], false, false), &c), "{m:?}");
        }
        assert!(!fav.of(&f(&["work"], true, true), &c));
        assert!(!Mark::Pull.of(&f(&["favourite"], false, true), &c));
        assert!(!Mark::Ignore.of(&f(&["favourite"], true, false), &c));
        assert_eq!(Mark::all(&c), [fav, work, Mark::Pull, Mark::Ignore]);
        assert_eq!(
            (work.path(&c), Mark::Pull.path(&c), Mark::Ignore.path(&c)),
            (
                "marks.work.repos".into(),
                "pull".into(),
                "ignore.repos".into()
            )
        );
        assert_eq!(
            (fav.points(&c), work.points(&c), Mark::Pull.points(&c)),
            (1, 0, 0)
        );
    }

    #[test]
    fn bad_patterns_are_config_errors() {
        let c = Config::parse("marks.favourite.repos = ['acme/[oops']").unwrap();
        let Err(e) = RuleSet::new(&c) else {
            panic!("accepted")
        };
        assert!(e.to_string().contains("[oops"), "{e}");
    }

    /// `set_mark` on the widget, returning the favourite list after.
    fn set(list: &[&str], on: bool) -> Vec<String> {
        let mut c = Config::default();
        c.marks["favourite"].repos = list.iter().map(|s| s.to_string()).collect();
        let id = RepoId::from_url(URL).unwrap();
        set_mark(
            &mut c,
            Mark::Label(0),
            Some(&id),
            Path::new("/r/widget"),
            "github.com/acme/widget",
            false,
            on,
        )
        .unwrap();
        c.marks["favourite"].repos.clone()
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
            Mark::Pull,
            Some(&id),
            p,
            "github.com/acme/widget",
            true,
            false,
        )
        .unwrap();
        assert_eq!(c.pull, ["!github.com/acme/widget"]);
        set_mark(
            &mut c,
            Mark::Pull,
            Some(&id),
            p,
            "github.com/acme/widget",
            true,
            true,
        )
        .unwrap();
        assert!(c.pull.is_empty());
        // clearing drops its own entries only
        c.pull = vec!["acme/*".into(), "!acme/widget".into()];
        clear_mark(&mut c, Mark::Pull, Some(&id), p).unwrap();
        assert_eq!(c.pull, ["acme/*"]);
    }
}
