//! TUI application state machine. Pure: keys and worker messages go in,
//! [`Effect`]s come out. No terminal or threads here, so it's unit-testable.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use argus_core::badges::Role;
use argus_core::config::StashConfig;
use argus_core::diff::{self, FileDiff, LineKind, Side};
use argus_core::fix::{self, Applied, FixKind};
use argus_core::ops::{self, Op, OpOutcome};
use argus_core::pull::{self, Job, PullPlan};
use argus_core::rules::{IssueList, Mark};
use argus_core::status::RepoStatus;
use argus_core::{Entry, ViewFilter, Workspace, paths};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::details::{DetailLine, DiffRef, details};
use crate::input::{Edit, LineEditor};
use crate::menu::{Menu, MenuId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Marked repos, or the repo under the cursor if none are marked.
    Selected,
    /// Every visible repo (for pull: every one marked for pull).
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    NextRepo,
    PrevRepo,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
    ToggleMark,
    UnmarkAll,
    OpenMenu(MenuId),
    StartFilter,
    ClearFilter,
    Help,
    Quit,
    ToggleLog,
    Fetch(Scope),
    Pull(Scope),
    Push,
    Publish,
    PushBranches,
    Stash(StashKind),
    /// Flip a menu switch for this session.
    Toggle(Switch),
    /// Stage, unstage or discard the marked files, else the one under the
    /// cursor in the details.
    StageFiles,
    UnstageFiles,
    DiscardFiles,
    Commit,
    SwitchDefault,
    Reattach,
    FastForward,
    AutoFix(Scope),
    PullAndClone,
    ToggleFavourite,
    TogglePull,
    ToggleIgnore,
    /// Ignore (or stop ignoring) the issue on the details line under the
    /// cursor, for that repo, pinned as the list pins by default.
    ToggleIgnoreIssue,
    /// The same, pinning the other way: stashes to their messages, where
    /// HEAD is not at all.
    ToggleIgnoreIssueOtherPin,
    Snooze,
    Unsnooze,
    Ack,
    ClearMarks,
    Shell,
    GitUi,
    Editor,
    Browse,
    ToggleAttention,
    ToggleHidden,
    Refresh,
    Rescan,
    /// Next built-in theme, for this session only.
    NextTheme,
    /// Next icon set, for this session only.
    NextIcons,
}

/// A row's identity across rebuilds: repo path, file, and diff position
/// (side, hunk, whether a line).
type RowKey = (PathBuf, Option<String>, Option<(Side, Option<usize>, bool)>);

/// The diff rows under an expanded file's line: each side with changes (unstaged first), its hunks
/// (unless folded) and their lines.
fn diff_rows(
    cache: &mut HashMap<(PathBuf, String, Side), Result<FileDiff, String>>,
    folded: &BTreeSet<(PathBuf, String, Side, usize)>,
    repo: &std::path::Path,
    s: &RepoStatus,
    file: &str,
) -> Vec<DetailLine> {
    let Some(c) = s.changes.files.iter().find(|c| c.path == file) else {
        return vec![];
    };
    let sides = [
        (
            Side::Unstaged,
            c.worktree != ' ' || c.index == '?',
            "unstaged changes",
        ),
        (
            Side::Staged,
            c.index != ' ' && c.index != '?',
            "staged changes",
        ),
    ];
    let indent = || (None, "      ".to_string());
    let row = |parts: Vec<(Option<Role>, String)>, side, hunk, line| DetailLine {
        parts,
        file: Some(file.to_string()),
        diff: Some(DiffRef { side, hunk, line }),
        issue: None,
        ignored: false,
    };
    let mut v = vec![];
    for (side, _, title) in sides.into_iter().filter(|s| s.1) {
        v.push(row(
            vec![indent(), (Some(Role::Dim), title.into())],
            side,
            None,
            None,
        ));
        let d = cache
            .entry((repo.to_path_buf(), file.to_string(), side))
            .or_insert_with(|| diff::file_diff(repo, file, side).map_err(|e| e.to_string()));
        let note = |text: &str| {
            row(
                vec![indent(), (Some(Role::Dim), text.into())],
                side,
                None,
                None,
            )
        };
        match d {
            Err(e) => v.push(row(
                vec![indent(), (Some(Role::Error), e.clone())],
                side,
                None,
                None,
            )),
            Ok(FileDiff::Binary) => v.push(note("binary file")),
            Ok(FileDiff::External) => v.push(note("external diff configured")),
            Ok(FileDiff::Text(hunks)) if hunks.is_empty() => v.push(note("no change in content")),
            Ok(FileDiff::Text(hunks)) => {
                for (h, hunk) in hunks.iter().enumerate() {
                    let header = vec![indent(), (Some(Role::DiffHunk), hunk.header())];
                    v.push(row(header, side, Some(h), None));
                    if folded.contains(&(repo.to_path_buf(), file.to_string(), side, h)) {
                        continue;
                    }
                    for l in &hunk.lines {
                        let (role, mark) = match l.kind {
                            LineKind::Add => (Some(Role::DiffAdd), '+'),
                            LineKind::Remove => (Some(Role::DiffRemove), '-'),
                            LineKind::Context => (None, ' '),
                        };
                        let mut parts = vec![indent(), (role, format!("{mark}{}", l.text))];
                        if l.no_newline {
                            parts.push((Some(Role::Dim), "  (no newline at end of file)".into()));
                        }
                        v.push(row(parts, side, Some(h), Some(l.clone())));
                    }
                }
            }
        }
    }
    v
}

/// Where file operations apply, for when there's no file to act on.
const FILE_HINT: &str = "move to a file in the details (Tab), or mark files with space";

/// What a stash action takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashKind {
    /// Everything (untracked files as the switch says).
    All,
    /// Only staged changes.
    Staged,
    /// Everything, after asking for a message.
    WithMessage,
    /// The files marked in the details view.
    MarkedFiles,
}

/// Menu switches, kept for the session (starting from config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Stash untracked files too.
    Untracked,
    /// Leave staged changes in the index after stashing.
    KeepIndex,
}

/// Something for the runtime to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Jobs(Vec<Job>),
    Fix {
        paths: Vec<PathBuf>,
        kinds: Vec<FixKind>,
    },
    /// Discover + status everything.
    Scan,
    /// Re-status these repos.
    Refresh(Vec<PathBuf>),
    /// List forges and plan clones/pulls; answer with [`Msg::PullPlanned`].
    PlanPullAndClone,
    /// Describe a detached HEAD however far back its tag is; answer with
    /// [`Msg::Described`].
    Describe(PathBuf),
    /// Leave the TUI, run a command in a directory, come back.
    Suspend {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
    },
    Open(String),
    SaveState,
    /// Write the mark lists' changes since this config to the config file.
    SaveMarks(Box<argus_core::Config>),
    Quit,
}

/// Results coming back from the runtime.
#[derive(Debug)]
pub enum Msg {
    /// A full describe finished (its answer is remembered by status).
    Described(PathBuf),
    Scanned {
        entries: Vec<Entry>,
        /// Directories skipped because of `max_depth`.
        truncated: Vec<PathBuf>,
    },
    Status(PathBuf, Box<Result<RepoStatus, String>>),
    JobStarted {
        path: PathBuf,
        label: String,
    },
    JobFinished {
        path: PathBuf,
        label: String,
        op: Op,
        outcome: OpOutcome,
    },
    FixDone {
        path: PathBuf,
        applied: Result<Option<Applied>, String>,
    },
    PullPlanned {
        tracks: argus_core::State,
        plan: PullPlan,
        errors: Vec<(String, String)>,
    },
    Suspended(Result<(), String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Filter,
    CommitMessage(Vec<PathBuf>),
    StashMessage(Vec<PathBuf>),
    SnoozeDuration(Vec<String>),
}

pub enum Mode {
    Normal,
    Menu {
        menu: Menu,
        cursor: usize,
    },
    Input {
        prompt: Prompt,
        editor: LineEditor,
    },
    Help {
        scroll: u16,
    },
    /// A permanently destructive action waiting for `y`; any other key
    /// cancels it.
    Confirm {
        question: String,
        then: Confirmed,
    },
}

/// Actions that need confirmation, with what they apply to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    ClearMarks(Vec<String>),
    /// Files to discard, by repo path.
    Discard(Vec<(PathBuf, Vec<String>)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Repo(usize),
    Detail { repo: usize, line: DetailLine },
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: jiff::Timestamp,
    pub repo: String,
    pub label: String,
    pub ok: bool,
    pub output: String,
}

pub struct App {
    pub ws: Workspace,
    pub mode: Mode,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// Top row of the viewport, maintained by the renderer.
    pub scroll: usize,
    pub page: usize,
    pub filter: String,
    pub view: ViewFilter,
    pub expanded: BTreeSet<PathBuf>,
    /// Repos whose full describe is running.
    pub describing: BTreeSet<PathBuf>,
    pub marked: BTreeSet<PathBuf>,
    /// Files marked in the details view, by repo path.
    pub marked_files: BTreeMap<PathBuf, BTreeSet<String>>,
    /// Files whose diff is shown, by repo path and file.
    pub expanded_files: BTreeSet<(PathBuf, String)>,
    /// Folded hunks: repo, file, side, hunk index.
    pub folded_hunks: BTreeSet<(PathBuf, String, Side, usize)>,
    /// Diffs read so far; a repo's go when its status changes.
    diffs: HashMap<(PathBuf, String, Side), Result<FileDiff, String>>,
    /// The stash switches for this session.
    pub stash: StashConfig,
    pub running: HashMap<PathBuf, String>,
    pub log: Vec<LogEntry>,
    pub show_log: bool,
    pub message: Option<(String, bool)>,
    pub scanning: bool,
    /// Shown with each scan result, e.g. "no roots configured…".
    pub notices: Vec<String>,
    pub quit: bool,
    /// Pending "g" for vi-style `gg`.
    pending_g: bool,
}

impl App {
    pub fn new(ws: Workspace) -> Self {
        let view = ViewFilter {
            attention_only: ws.config.ui.attention_only,
            show_hidden: false,
        };
        let stash = ws.config.stash;
        let mut a = Self {
            ws,
            mode: Mode::Normal,
            rows: vec![],
            cursor: 0,
            scroll: 0,
            page: 20,
            filter: String::new(),
            view,
            expanded: BTreeSet::new(),
            describing: BTreeSet::new(),
            marked: BTreeSet::new(),
            marked_files: BTreeMap::new(),
            expanded_files: BTreeSet::new(),
            folded_hunks: BTreeSet::new(),
            diffs: HashMap::new(),
            stash,
            running: HashMap::new(),
            log: vec![],
            show_log: false,
            message: None,
            scanning: false,
            notices: vec![],
            quit: false,
            pending_g: false,
        };
        a.rebuild();
        a
    }

    // ---------- rows ----------

    /// Recompute rows, keeping the cursor on the same row where possible
    /// (else on its repo).
    pub fn rebuild(&mut self) {
        let target = self.row_key(self.cursor);
        self.rebuild_at(target);
    }

    /// What identifies row `n` across rebuilds: its repo, and for detail
    /// rows its file and diff position.
    fn row_key(&self, n: usize) -> Option<RowKey> {
        match self.rows.get(n)? {
            Row::Repo(i) => Some((self.ws.entries[*i].path.clone(), None, None)),
            Row::Detail { repo, line } => Some((
                self.ws.entries[*repo].path.clone(),
                line.file.clone(),
                line.diff
                    .as_ref()
                    .map(|d| (d.side, d.hunk, d.line.is_some())),
            )),
        }
    }

    /// [`Self::rebuild`], putting the cursor on the row `target` names: the
    /// exact row if it's still there (a diff line's position within its
    /// hunk isn't kept), else its file's line, else its repo.
    fn rebuild_at(&mut self, target: Option<RowKey>) {
        let keep = target.as_ref().map(|t| t.0.clone());
        let needle = self.filter.to_lowercase();
        let (icons, _) = self.ws.config.ui.icons();
        let mut rows = vec![];
        for i in self.ws.ordered(self.view) {
            let e = &self.ws.entries[i];
            if !needle.is_empty()
                && !e.display_name().to_lowercase().contains(&needle)
                && !e.key.to_lowercase().contains(&needle)
            {
                continue;
            }
            rows.push(Row::Repo(i));
            if self.expanded.contains(&e.path) {
                for line in details(e, &self.ws.config.fix.auto, &self.ws.config.stash, &icons) {
                    let expanded = line.file.as_ref().filter(|f| {
                        self.expanded_files
                            .contains(&(e.path.clone(), f.to_string()))
                    });
                    let diff = match (expanded, e.status()) {
                        (Some(f), Some(s)) => {
                            diff_rows(&mut self.diffs, &self.folded_hunks, &e.path, s, f)
                        }
                        _ => vec![],
                    };
                    rows.push(Row::Detail { repo: i, line });
                    rows.extend(diff.into_iter().map(|line| Row::Detail { repo: i, line }));
                }
            }
        }
        self.rows = rows;
        // marks on files that are no longer changed go
        let ws = &self.ws;
        self.marked_files.retain(|repo, files| {
            let changed = ws
                .get(repo)
                .and_then(|e| e.status())
                .map(|s| &s.changes.files);
            files.retain(|f| changed.is_some_and(|c| c.iter().any(|x| x.path == *f)));
            !files.is_empty()
        });
        let exact = target.as_ref().and_then(|t| {
            let file_line = (t.0.clone(), t.1.clone(), None);
            (0..self.rows.len())
                .find(|&n| self.row_key(n).as_ref() == Some(t))
                .or_else(|| {
                    (0..self.rows.len()).find(|&n| self.row_key(n) == Some(file_line.clone()))
                })
        });
        self.cursor = exact
            .or_else(|| {
                keep.and_then(|p| {
                    self.rows
                        .iter()
                        .position(|r| matches!(r, Row::Repo(i) if self.ws.entries[*i].path == p))
                })
            })
            .unwrap_or(self.cursor)
            .min(self.rows.len().saturating_sub(1));
    }

    pub fn current_repo(&self) -> Option<usize> {
        match self.rows.get(self.cursor)? {
            Row::Repo(i) | Row::Detail { repo: i, .. } => Some(*i),
        }
    }

    pub fn current(&self) -> Option<&Entry> {
        self.current_repo().map(|i| &self.ws.entries[i])
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|e| e.path.clone())
    }

    /// Marked repos that are still listed, else the current one.
    pub fn selected(&self) -> Vec<usize> {
        let marked: Vec<usize> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Repo(i) if self.marked.contains(&self.ws.entries[*i].path) => Some(*i),
                _ => None,
            })
            .collect();
        if marked.is_empty() {
            self.current_repo().into_iter().collect()
        } else {
            marked
        }
    }

    fn visible_repos(&self) -> Vec<usize> {
        self.rows
            .iter()
            .filter_map(|r| if let Row::Repo(i) = r { Some(*i) } else { None })
            .collect()
    }

    /// What a bulk action ("all") acts on: the visible repos, less hidden
    /// ones even while they're shown. Choosing one explicitly still works.
    fn bulk_repos(&self) -> Vec<usize> {
        self.visible_repos()
            .into_iter()
            .filter(|&i| !self.ws.entries[i].flags.hidden())
            .collect()
    }

    fn info(&mut self, s: impl Into<String>) {
        self.message = Some((s.into(), false));
    }

    /// Report a failure in the status line (e.g. a save that failed).
    pub fn report_error(&mut self, s: impl Into<String>) {
        self.error(s);
    }

    fn error(&mut self, s: impl Into<String>) {
        self.message = Some((s.into(), true));
    }

    // ---------- keys ----------

    pub fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        if matches!(self.mode, Mode::Normal) {
            self.message = None;
        }
        match &mut self.mode {
            Mode::Normal => match self.normal_key(k) {
                Some(a) => self.act(a),
                None => vec![],
            },
            Mode::Help { scroll } => {
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::Char('n') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        *scroll = scroll.saturating_add(1)
                    }
                    KeyCode::Char('p') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        *scroll = scroll.saturating_sub(1)
                    }
                    _ => self.mode = Mode::Normal,
                }
                vec![]
            }
            Mode::Menu { menu, cursor } => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let n = menu.items.len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    KeyCode::Char('g') if ctrl => self.mode = Mode::Normal,
                    KeyCode::Down | KeyCode::Tab => *cursor = (*cursor + 1) % n,
                    KeyCode::Char('n') if ctrl => *cursor = (*cursor + 1) % n,
                    KeyCode::Up | KeyCode::BackTab => *cursor = (*cursor + n - 1) % n,
                    KeyCode::Char('p') if ctrl => *cursor = (*cursor + n - 1) % n,
                    KeyCode::Enter => {
                        let it = &menu.items[*cursor];
                        let (a, stays) = (it.action, it.switch.is_some());
                        if !stays {
                            self.mode = Mode::Normal;
                        }
                        return self.act(a);
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(it) = menu.items.iter().find(|i| i.key == c) {
                            // switches keep the menu open, as in magit
                            let (a, stays) = (it.action, it.switch.is_some());
                            if !stays {
                                self.mode = Mode::Normal;
                            }
                            return self.act(a);
                        }
                    }
                    _ => {}
                }
                vec![]
            }
            Mode::Confirm { then, .. } => {
                let then = then.clone();
                self.mode = Mode::Normal;
                if k.code == KeyCode::Char('y') && k.modifiers.is_empty() {
                    return self.confirmed(then);
                }
                self.info("cancelled");
                vec![]
            }
            Mode::Input { prompt, editor } => {
                let r = editor.handle(k);
                let text = editor.text();
                let prompt = prompt.clone();
                match r {
                    Edit::Cancel => {
                        if prompt == Prompt::Filter {
                            self.filter.clear();
                            self.rebuild();
                        }
                        self.mode = Mode::Normal;
                        vec![]
                    }
                    Edit::Submit => {
                        self.mode = Mode::Normal;
                        self.submit(prompt, text)
                    }
                    Edit::Changed if prompt == Prompt::Filter => {
                        self.filter = text;
                        self.rebuild();
                        vec![]
                    }
                    _ => vec![],
                }
            }
        }
    }

    fn normal_key(&mut self, k: KeyEvent) -> Option<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let g = std::mem::take(&mut self.pending_g);
        use Action::*;
        Some(match k.code {
            KeyCode::Up => Up,
            KeyCode::Down => Down,
            KeyCode::PageUp => PageUp,
            KeyCode::PageDown => PageDown,
            KeyCode::Home => Top,
            KeyCode::End => Bottom,
            KeyCode::Tab | KeyCode::Enter => ToggleExpand,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    ClearFilter
                } else {
                    UnmarkAll
                }
            }
            KeyCode::Char(c) if ctrl => match c {
                'n' => Down,
                'p' => Up,
                'v' => PageDown,
                'd' => PageDown,
                'u' => PageUp,
                's' => StartFilter,
                'g' => UnmarkAll,
                'c' => Quit,
                'l' => Refresh,
                _ => return None,
            },
            KeyCode::Char(c) if alt => match c {
                'v' => PageUp,
                '<' => Top,
                '>' => Bottom,
                'n' => NextRepo,
                'p' => PrevRepo,
                _ => return None,
            },
            KeyCode::Char(c) => match c {
                'j' => Down,
                'k' => Up,
                'n' => NextRepo,
                'p' => PrevRepo,
                'g' if g => Top,
                'g' => {
                    // `g` alone refreshes (magit); `gg` goes to top (vi). The
                    // refresh is cheap, so do it immediately and let a second
                    // `g` also jump.
                    self.pending_g = true;
                    Refresh
                }
                'G' => Bottom,
                '<' => Top,
                '>' => Bottom,
                ' ' => ToggleMark,
                's' => StageFiles,
                'u' => UnstageFiles,
                'K' => DiscardFiles,
                'U' => UnmarkAll,
                '/' => StartFilter,
                '?' | 'h' => Help,
                'q' => Quit,
                '$' | 'L' => ToggleLog,
                '!' => Shell,
                'a' => ToggleAttention,
                'H' => ToggleHidden,
                'R' => Rescan,
                c => {
                    let m = MenuId::from_key(c)?;
                    OpenMenu(m)
                }
            },
            _ => return None,
        })
    }

    // ---------- actions ----------

    pub fn act(&mut self, a: Action) -> Vec<Effect> {
        use Action::*;
        let last = self.rows.len().saturating_sub(1);
        match a {
            Up => self.cursor = self.cursor.saturating_sub(1),
            Down => self.cursor = (self.cursor + 1).min(last),
            PageUp => self.cursor = self.cursor.saturating_sub(self.page.max(1)),
            PageDown => self.cursor = (self.cursor + self.page.max(1)).min(last),
            Top => self.cursor = 0,
            Bottom => self.cursor = last,
            NextRepo => {
                if let Some(i) = (self.cursor + 1..self.rows.len())
                    .find(|&i| matches!(self.rows[i], Row::Repo(_)))
                {
                    self.cursor = i;
                }
            }
            PrevRepo => {
                // from a detail line, first go to its own repo
                if let Some(i) = (0..self.cursor)
                    .rev()
                    .find(|&i| matches!(self.rows[i], Row::Repo(_)))
                {
                    self.cursor = i;
                }
            }
            ToggleExpand if self.on_file_or_diff() => return self.toggle_file_or_hunk(),
            ToggleExpand => {
                if let Some(p) = self.current_path() {
                    if !self.expanded.remove(&p) {
                        self.expanded.insert(p.clone());
                    }
                    self.rebuild();
                    // stay on the repo row when collapsing from a detail line
                    if let Some(i) = self
                        .rows
                        .iter()
                        .position(|r| matches!(r, Row::Repo(i) if self.ws.entries[*i].path == p))
                    {
                        self.cursor = i;
                    }
                    return self.describe_opened();
                }
            }
            ExpandAll => {
                self.expanded = self
                    .visible_repos()
                    .into_iter()
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                self.rebuild();
                return self.describe_opened();
            }
            CollapseAll => {
                self.expanded.clear();
                self.rebuild();
            }
            ToggleMark => {
                if let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor)
                    && let Some(f) = &line.file
                {
                    let repo = self.ws.entries[*repo].path.clone();
                    let files = self.marked_files.entry(repo.clone()).or_default();
                    if !files.remove(f) {
                        files.insert(f.clone());
                    }
                    if files.is_empty() {
                        self.marked_files.remove(&repo);
                    }
                    self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
                } else if let Some(p) = self.current_path() {
                    if !self.marked.remove(&p) {
                        self.marked.insert(p);
                    }
                    self.act(NextRepo);
                }
            }
            UnmarkAll => {
                self.marked.clear();
                self.marked_files.clear();
            }
            Toggle(Switch::Untracked) => self.stash.untracked = !self.stash.untracked,
            Toggle(Switch::KeepIndex) => self.stash.keep_index = !self.stash.keep_index,
            Stash(kind) => return self.stash_action(kind),
            StageFiles => return self.file_op(ops::stage_ops, "stage"),
            UnstageFiles => return self.file_op(ops::unstage_ops, "unstage"),
            DiscardFiles => {
                let targets = self.file_targets();
                if targets.is_empty() {
                    self.info(FILE_HINT);
                    return vec![];
                }
                let files: Vec<&String> = targets.iter().flat_map(|(_, f)| f).collect();
                let question = match files.as_slice() {
                    [one] => format!("Discard changes to {one}?"),
                    many => format!("Discard changes to {} files?", many.len()),
                };
                let then = targets
                    .iter()
                    .map(|(i, f)| (self.ws.entries[*i].path.clone(), f.clone()))
                    .collect();
                self.mode = Mode::Confirm {
                    question,
                    then: Confirmed::Discard(then),
                };
            }
            OpenMenu(m) => {
                self.mode = Mode::Menu {
                    menu: m.menu(),
                    cursor: 0,
                }
            }
            StartFilter => {
                self.mode = Mode::Input {
                    prompt: Prompt::Filter,
                    editor: LineEditor::new(&self.filter),
                }
            }
            ClearFilter => {
                self.filter.clear();
                self.rebuild();
            }
            Help => self.mode = Mode::Help { scroll: 0 },
            Quit => {
                self.quit = true;
                return vec![Effect::SaveState, Effect::Quit];
            }
            ToggleLog => self.show_log = !self.show_log,
            ToggleAttention => {
                self.view.attention_only = !self.view.attention_only;
                self.rebuild();
                self.info(if self.view.attention_only {
                    "showing repos needing attention"
                } else {
                    "showing all repos"
                });
            }
            NextTheme => {
                let ui = &mut self.ws.config.ui;
                ui.theme = next(crate::theme::BUILTIN, &ui.theme).to_string();
                let msg = format!("theme {} (this session; set [ui] theme to keep)", ui.theme);
                self.info(msg);
            }
            NextIcons => {
                let ui = &mut self.ws.config.ui;
                ui.icons = next(argus_core::icons::SETS, &ui.icons).to_string();
                let msg = format!("icons {} (this session; set [ui] icons to keep)", ui.icons);
                self.rebuild();
                self.info(msg);
            }
            ToggleHidden => {
                self.view.show_hidden = !self.view.show_hidden;
                self.rebuild();
                self.info(if self.view.show_hidden {
                    "showing hidden repos"
                } else {
                    "hiding ignored/snoozed repos"
                });
            }
            Refresh => {
                let paths: Vec<PathBuf> = self.ws.entries.iter().map(|e| e.path.clone()).collect();
                return vec![Effect::Refresh(paths)];
            }
            Rescan => {
                self.scanning = true;
                return vec![Effect::Scan];
            }
            Fetch(scope) => return self.jobs_for(scope, |_| Some(Op::Fetch)),
            Pull(Scope::All) => {
                let jobs = pull::plan_updates(&self.ws);
                return self.start_jobs(jobs);
            }
            Pull(Scope::Selected) => {
                return self.jobs_for(Scope::Selected, |e| {
                    let s = e.status()?;
                    Some(if s.upstream.as_ref().is_some_and(|t| !t.gone) {
                        Op::Pull
                    } else {
                        Op::Fetch
                    })
                });
            }
            Push => return self.fix_ops(FixKind::Push),
            Publish => return self.fix_ops(FixKind::Publish),
            PushBranches => return self.fix_ops(FixKind::PushBranches),
            SwitchDefault => return self.fix_ops(FixKind::SwitchDefault),
            Reattach => return self.fix_ops(FixKind::Reattach),
            FastForward => return self.fix_ops(FixKind::FastForward),
            Commit => {
                let targets: Vec<PathBuf> = self
                    .selected()
                    .into_iter()
                    .map(|i| &self.ws.entries[i])
                    .filter(|e| e.status().is_some_and(|s| !s.changes.is_clean()))
                    .map(|e| e.path.clone())
                    .collect();
                if targets.is_empty() {
                    self.info("nothing to commit");
                } else {
                    self.mode = Mode::Input {
                        prompt: Prompt::CommitMessage(targets),
                        editor: LineEditor::default(),
                    };
                }
            }
            AutoFix(scope) => {
                let idx = if scope == Scope::All {
                    self.bulk_repos()
                } else {
                    self.selected()
                };
                let kinds = self.ws.config.fix.auto.clone();
                let paths: Vec<PathBuf> = idx
                    .into_iter()
                    .filter(|&i| {
                        let e = &self.ws.entries[i];
                        e.status().is_some_and(|s| {
                            fix::choose(s, &e.fix_kinds(&kinds), &self.ws.config.stash).is_some()
                        })
                    })
                    .map(|i| self.ws.entries[i].path.clone())
                    .filter(|p| !self.running.contains_key(p))
                    .collect();
                if paths.is_empty() {
                    self.info(format!("no auto-fixes ({}) apply", kinds_label(&kinds)));
                    return vec![];
                }
                for p in &paths {
                    self.running.insert(p.clone(), "fix".into());
                }
                return vec![Effect::Fix { paths, kinds }];
            }
            PullAndClone => {
                self.info("listing forges…");
                return vec![Effect::PlanPullAndClone];
            }
            ToggleFavourite => return self.toggle_mark(Mark::Favourite),
            TogglePull => return self.toggle_mark(Mark::Pull),
            ToggleIgnore => return self.toggle_mark(Mark::Ignore),
            ToggleIgnoreIssue => return self.toggle_ignore_issue(false),
            ToggleIgnoreIssueOtherPin => return self.toggle_ignore_issue(true),
            ClearMarks => {
                let keys = self.selected_keys();
                let n = self.ws.marked_count(&self.selected());
                if n > 1 {
                    self.mode = Mode::Confirm {
                        question: format!("Clear marks of {n} repos?"),
                        then: Confirmed::ClearMarks(keys),
                    };
                    return vec![];
                }
                return self.confirmed(Confirmed::ClearMarks(keys));
            }
            Snooze => {
                let keys = self.selected_keys();
                if !keys.is_empty() {
                    self.mode = Mode::Input {
                        prompt: Prompt::SnoozeDuration(keys),
                        editor: LineEditor::new("3d"),
                    };
                }
            }
            Unsnooze => {
                for k in self.selected_keys() {
                    self.ws.state.snoozed.remove(&k);
                }
                return self.flags_changed();
            }
            Ack => {
                for k in self.selected_keys() {
                    self.ws.state.new.remove(&k);
                }
                return self.flags_changed();
            }
            Shell => {
                if let Some(e) = self.current() {
                    let shell = self
                        .ws
                        .config
                        .tools
                        .shell
                        .clone()
                        .or_else(|| std::env::var("SHELL").ok())
                        .unwrap_or_else(|| "/bin/sh".into());
                    return vec![Effect::Suspend {
                        program: shell,
                        args: vec![],
                        cwd: e.path.clone(),
                    }];
                }
            }
            GitUi => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                match self.ws.config.tools.git_ui.clone() {
                    Some(cmd) => return vec![shell_command(&cmd, e.path.clone())],
                    None => self.error("set tools.git_ui in config, e.g. \"lazygit\""),
                }
            }
            Editor => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                let ed = self
                    .ws
                    .config
                    .tools
                    .editor
                    .clone()
                    .or_else(|| std::env::var("VISUAL").ok())
                    .or_else(|| std::env::var("EDITOR").ok())
                    .unwrap_or_else(|| "vi".into());
                return vec![shell_command(&format!("{ed} ."), e.path.clone())];
            }
            Browse => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                match &e.id {
                    Some(id) => {
                        return vec![Effect::Open(format!(
                            "https://{}/{}/{}",
                            id.host, id.owner, id.name
                        ))];
                    }
                    None => self.error("no forge identity for this repo"),
                }
            }
        }
        vec![]
    }

    fn selected_keys(&self) -> Vec<String> {
        self.selected()
            .into_iter()
            .map(|i| self.ws.entries[i].key.clone())
            .collect()
    }

    fn toggle_mark(&mut self, m: Mark) -> Vec<Effect> {
        // Toggle relative to the first selected repo so a mixed selection converges.
        let sel = self.selected();
        let Some(&first) = sel.first() else {
            return vec![];
        };
        let on = !m.of(&self.ws.entries[first].flags);
        let before = self.ws.config.clone();
        if let Err(e) = self.ws.set_mark(&sel, m, on) {
            self.error(e.to_string());
            return vec![];
        }
        self.rebuild();
        vec![Effect::SaveMarks(Box::new(before))]
    }

    /// Full describes for repos whose details are open and whose walk for
    /// a tag gave up at `describe_limit`, unless one is already running.
    fn describe_opened(&mut self) -> Vec<Effect> {
        let wanted: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|p| !self.describing.contains(*p))
            .filter(|p| {
                self.ws
                    .get(p)
                    .and_then(|e| e.status())
                    .is_some_and(|s| s.describe_gave_up)
            })
            .cloned()
            .collect();
        wanted
            .into_iter()
            .map(|p| {
                self.describing.insert(p.clone());
                Effect::Describe(p)
            })
            .collect()
    }

    /// `other_pin`: pin the other way from the list's default.
    fn toggle_ignore_issue(&mut self, other_pin: bool) -> Vec<Effect> {
        let Some(Row::Detail {
            repo,
            line:
                DetailLine {
                    issue: Some(issue),
                    ignored,
                    ..
                },
        }) = self.rows.get(self.cursor)
        else {
            self.info("no issue under the cursor");
            return vec![];
        };
        let (i, issue, on) = (*repo, *issue, !*ignored);
        let Some(list) = IssueList::of(issue) else {
            self.info("this issue can't be ignored: it risks losing work");
            return vec![];
        };
        let pin = list.pins_by_default() != other_pin;
        let before = self.ws.config.clone();
        if let Err(e) = self.ws.set_issue_ignored(&[i], list, on, Some(pin)) {
            self.error(e.to_string());
            return vec![];
        }
        self.rebuild();
        self.info(format!(
            "{} [ignore] {}{}",
            if on { "added to" } else { "removed from" },
            list.name(),
            if on && pin && list.pins() {
                ", pinned"
            } else {
                ""
            }
        ));
        vec![Effect::SaveMarks(Box::new(before))]
    }

    /// Carry out a confirmed (or harmless enough) destructive action.
    fn confirmed(&mut self, c: Confirmed) -> Vec<Effect> {
        match c {
            Confirmed::Discard(by_repo) => {
                let targets = by_repo
                    .into_iter()
                    .filter_map(|(p, f)| Some((self.ws.index_of(&p)?, f)))
                    .collect();
                let jobs = self.file_jobs(targets, ops::discard_ops);
                self.start_jobs(jobs)
            }
            Confirmed::ClearMarks(keys) => {
                // by key: a scan may have arrived while asking
                let indices: Vec<usize> = (0..self.ws.entries.len())
                    .filter(|&i| keys.contains(&self.ws.entries[i].key))
                    .collect();
                let before = self.ws.config.clone();
                if let Err(e) = self.ws.clear_marks(&indices) {
                    self.error(e.to_string());
                    return vec![];
                }
                for k in &keys {
                    self.ws.state.snoozed.remove(k);
                }
                let mut fx = vec![Effect::SaveMarks(Box::new(before))];
                fx.extend(self.flags_changed());
                fx
            }
        }
    }

    fn flags_changed(&mut self) -> Vec<Effect> {
        self.ws.resolve_flags();
        self.rebuild();
        vec![Effect::SaveState]
    }

    /// Run every fix of a kind (one per branch, for kinds that have
    /// several) on the selected repos where it applies and isn't blocked.
    fn fix_ops(&mut self, kind: FixKind) -> Vec<Effect> {
        let stash = self.ws.config.stash;
        let (mut jobs, mut blocked) = (vec![], None);
        for i in self.selected() {
            let e = &self.ws.entries[i];
            let Some(s) = e.status() else { continue };
            for f in fix::plan(s, &stash).into_iter().filter(|f| f.kind == kind) {
                if let Some(why) = f.blocked {
                    blocked.get_or_insert(format!("{}: {why}", e.display_name()));
                    continue;
                }
                jobs.push(Job {
                    label: format!("{} {}", f.op.label(), e.display_name()),
                    family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                    path: e.path.clone(),
                    op: f.op,
                });
            }
        }
        if jobs.is_empty() {
            match blocked {
                Some(why) => self.error(why),
                None => self.info("not applicable to the selection (see details with Tab)"),
            }
            return vec![];
        }
        self.start_jobs(jobs)
    }

    fn jobs_for(&mut self, scope: Scope, op: impl Fn(&Entry) -> Option<Op>) -> Vec<Effect> {
        let idx = if scope == Scope::All {
            self.bulk_repos()
        } else {
            self.selected()
        };
        let mut jobs = vec![];
        for i in idx {
            let e = &self.ws.entries[i];
            if let Some(op) = op(e) {
                jobs.push(Job {
                    label: format!("{} {}", op.label(), e.display_name()),
                    family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                    path: e.path.clone(),
                    op,
                });
            }
        }
        self.start_jobs(jobs)
    }

    /// The marked files, else the file under the cursor (its line or its
    /// diff), by repo index.
    fn file_targets(&self) -> Vec<(usize, Vec<String>)> {
        if !self.marked_files.is_empty() {
            return self
                .marked_files
                .iter()
                .filter_map(|(repo, files)| {
                    Some((self.ws.index_of(repo)?, files.iter().cloned().collect()))
                })
                .collect();
        }
        match self.rows.get(self.cursor) {
            Some(Row::Detail { repo, line }) => {
                line.file.iter().map(|f| (*repo, vec![f.clone()])).collect()
            }
            _ => vec![],
        }
    }

    /// Jobs running `plan`'s ops for each target repo's files, in order.
    fn file_jobs(
        &self,
        targets: Vec<(usize, Vec<String>)>,
        plan: fn(&RepoStatus, &[String]) -> Vec<Op>,
    ) -> Vec<Job> {
        let mut jobs = vec![];
        for (i, files) in targets {
            let e = &self.ws.entries[i];
            let Some(s) = e.status() else { continue };
            for op in plan(s, &files) {
                jobs.push(Job {
                    label: format!("{} in {}", op.label(), e.display_name()),
                    family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                    path: e.path.clone(),
                    op,
                });
            }
        }
        jobs
    }

    fn file_op(&mut self, plan: fn(&RepoStatus, &[String]) -> Vec<Op>, verb: &str) -> Vec<Effect> {
        let targets = self.file_targets();
        if targets.is_empty() {
            self.info(FILE_HINT);
            return vec![];
        }
        let jobs = self.file_jobs(targets, plan);
        if jobs.is_empty() {
            self.info(format!("nothing to {verb}"));
            return vec![];
        }
        self.start_jobs(jobs)
    }

    fn on_file_or_diff(&self) -> bool {
        matches!(self.rows.get(self.cursor), Some(Row::Detail { line, .. }) if line.file.is_some())
    }

    /// Tab on a file's line (or a side's heading) shows or hides its diff;
    /// on a hunk's header or lines, folds or unfolds the hunk, leaving the
    /// cursor on its header.
    fn toggle_file_or_hunk(&mut self) -> Vec<Effect> {
        let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor) else {
            return vec![];
        };
        let (path, file) = (
            self.ws.entries[*repo].path.clone(),
            line.file.clone().unwrap(),
        );
        match line.diff.as_ref().and_then(|d| d.hunk.map(|h| (d.side, h))) {
            Some((side, h)) => {
                let key = (path.clone(), file.clone(), side, h);
                if !self.folded_hunks.remove(&key) {
                    self.folded_hunks.insert(key);
                }
                self.rebuild_at(Some((path, Some(file), Some((side, Some(h), false)))));
            }
            None => {
                let key = (path, file);
                if !self.expanded_files.remove(&key) {
                    self.expanded_files.insert(key);
                }
                // the cursor stays on the file's line (from a heading too)
                self.rebuild();
            }
        }
        vec![]
    }

    /// Whether a switch is on this session.
    pub fn switch_on(&self, s: Switch) -> bool {
        match s {
            Switch::Untracked => self.stash.untracked,
            Switch::KeepIndex => self.stash.keep_index,
        }
    }

    fn stash_action(&mut self, kind: StashKind) -> Vec<Effect> {
        match kind {
            StashKind::All => {
                let targets = self.selected().into_iter().map(|i| (i, vec![])).collect();
                self.stash_jobs(targets, false, None)
            }
            StashKind::Staged => {
                let targets = self.selected().into_iter().map(|i| (i, vec![])).collect();
                self.stash_jobs(targets, true, None)
            }
            StashKind::WithMessage => {
                let paths = self
                    .selected()
                    .into_iter()
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                self.mode = Mode::Input {
                    prompt: Prompt::StashMessage(paths),
                    editor: LineEditor::new(""),
                };
                vec![]
            }
            StashKind::MarkedFiles => {
                let targets: Vec<(usize, Vec<String>)> = self
                    .marked_files
                    .iter()
                    .filter_map(|(repo, files)| {
                        Some((self.ws.index_of(repo)?, files.iter().cloned().collect()))
                    })
                    .collect();
                if targets.is_empty() {
                    self.info("mark files in the details view first (Tab, then space on a file)");
                    return vec![];
                }
                self.stash_jobs(targets, false, None)
            }
        }
    }

    /// Stash each target repo (with paths, if any) using this session's
    /// switches. A repo git can't stash cleanly refuses the lot.
    fn stash_jobs(
        &mut self,
        targets: Vec<(usize, Vec<String>)>,
        staged: bool,
        message: Option<String>,
    ) -> Vec<Effect> {
        let Op::Stash {
            message: default_message,
            ..
        } = Op::stash(&self.stash)
        else {
            unreachable!()
        };
        let mut jobs = vec![];
        let mut refused = vec![];
        for (i, paths) in targets {
            let e = &self.ws.entries[i];
            let Some(s) = e.status() else { continue };
            match ops::stash_problem(&e.path, s, self.stash.untracked, staged, &paths) {
                Some(why) if why == ops::NOTHING_TO_STASH => continue,
                Some(why) => refused.push(format!("{}: {why}", e.display_name())),
                None => {
                    let op = Op::Stash {
                        message: message.clone().unwrap_or_else(|| default_message.clone()),
                        untracked: self.stash.untracked,
                        keep_index: self.stash.keep_index,
                        staged,
                        paths,
                    };
                    jobs.push(Job {
                        label: format!("{} {}", op.label(), e.display_name()),
                        family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                        path: e.path.clone(),
                        op,
                    });
                }
            }
        }
        if !refused.is_empty() {
            self.error(format!("not stashing: {}", refused.join("; ")));
            return vec![];
        }
        if jobs.is_empty() {
            self.info("nothing to stash");
            return vec![];
        }
        self.start_jobs(jobs)
    }

    fn start_jobs(&mut self, mut jobs: Vec<Job>) -> Vec<Effect> {
        jobs.retain(|j| !self.running.contains_key(&j.path));
        if jobs.is_empty() {
            return vec![];
        }
        for j in &jobs {
            self.running.insert(j.path.clone(), j.op.label());
        }
        self.info(format!("running {} job(s)…", jobs.len()));
        vec![Effect::Jobs(jobs)]
    }

    fn submit(&mut self, prompt: Prompt, text: String) -> Vec<Effect> {
        match prompt {
            Prompt::Filter => {
                self.filter = text;
                self.rebuild();
                vec![]
            }
            Prompt::CommitMessage(paths) => {
                if text.trim().is_empty() {
                    self.error("empty commit message: aborted");
                    return vec![];
                }
                let jobs = paths
                    .into_iter()
                    .filter_map(|p| {
                        self.ws
                            .get(&p)
                            .map(|e| (p, e.display_name(), e.parent.clone()))
                    })
                    .map(|(path, name, parent)| Job {
                        label: format!("commit {name}"),
                        family: parent.unwrap_or_else(|| path.clone()),
                        path,
                        op: Op::CommitAll {
                            message: text.clone(),
                        },
                    })
                    .collect();
                self.start_jobs(jobs)
            }
            Prompt::StashMessage(paths) => {
                let targets = paths
                    .iter()
                    .filter_map(|p| self.ws.index_of(p))
                    .map(|i| (i, vec![]))
                    .collect();
                let message = (!text.trim().is_empty()).then_some(text);
                self.stash_jobs(targets, false, message)
            }
            Prompt::SnoozeDuration(keys) => match argus_core::state::parse_duration(&text) {
                Some(secs) => {
                    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(secs);
                    for k in keys {
                        self.ws.state.snoozed.insert(k, until);
                    }
                    self.info(format!("snoozed for {text}"));
                    self.flags_changed()
                }
                None => {
                    self.error(format!("bad duration {text:?} (try 90m, 12h, 3d, 2w)"));
                    vec![]
                }
            },
        }
    }

    // ---------- messages ----------

    pub fn on_msg(&mut self, m: Msg) -> Vec<Effect> {
        match m {
            Msg::Scanned { entries, truncated } => {
                self.scanning = false;
                self.diffs.clear();
                self.ws.entries = entries;
                self.ws.truncated = truncated;
                self.ws.resolve_flags();
                let mut parts = vec![format!("{} repos", self.ws.entries.len())];
                parts.extend(self.notices.iter().cloned());
                parts.extend(self.ws.truncation_warning());
                self.info(parts.join(" · "));
                self.rebuild();
            }
            Msg::Status(path, st) => {
                self.ws.update_status(&path, *st);
                self.diffs.retain(|k, _| k.0 != path);
                self.rebuild();
                return self.describe_opened();
            }
            Msg::Described(path) => {
                self.describing.remove(&path);
                // status now has the remembered answer
                return vec![Effect::Refresh(vec![path])];
            }
            Msg::JobStarted { path, label } => {
                self.running.insert(path, label);
            }
            Msg::JobFinished {
                path,
                label,
                op,
                outcome,
            } => {
                self.running.remove(&path);
                self.push_log(
                    &path,
                    label.clone(),
                    outcome.success,
                    outcome.output.clone(),
                );
                if !outcome.success {
                    self.error(format!("{label} failed ($ for log)"));
                }
                if let Op::Clone { .. } = op {
                    if outcome.success {
                        let jobs = [Job {
                            path: path.clone(),
                            op: op.clone(),
                            family: path.clone(),
                            label,
                        }];
                        pull::record_clones(&mut self.ws, &jobs, &[Some(outcome)]);
                        self.rebuild();
                        return vec![Effect::SaveState];
                    }
                    return vec![];
                }
                if self.running.is_empty() && self.message.as_ref().is_none_or(|m| !m.1) {
                    self.info("done");
                }
                return vec![Effect::Refresh(vec![path])];
            }
            Msg::FixDone { path, applied } => {
                self.running.remove(&path);
                let name = self
                    .ws
                    .get(&path)
                    .map(|e| e.display_name())
                    .unwrap_or_default();
                match applied {
                    Ok(list) => {
                        if let Some(a) = list {
                            let ok = a.outcome.success;
                            self.push_log(&path, a.fix.op.label(), ok, a.outcome.output);
                            if !ok {
                                self.error(format!(
                                    "{name}: {} failed ($ for log)",
                                    a.fix.op.label()
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        self.push_log(&path, "fix".into(), false, e.clone());
                        self.error(format!("{name}: {e}"));
                    }
                }
                return vec![Effect::Refresh(vec![path])];
            }
            Msg::PullPlanned {
                tracks,
                plan,
                errors,
            } => {
                self.ws.state.tracks = tracks.tracks;
                self.ws.resolve_flags();
                for (k, e) in &errors {
                    self.push_log(&PathBuf::from(k), "list forge".into(), false, e.clone());
                }
                for r in &plan.missing {
                    self.push_log(
                        &PathBuf::new(),
                        format!("new on forge: {}", r.id.slug()),
                        true,
                        String::new(),
                    );
                }
                for o in &plan.orphans {
                    self.push_log(o, "no longer listed by forge".into(), false, String::new());
                }
                let mut jobs = plan.jobs;
                jobs.extend(pull::plan_updates(&self.ws));
                self.ws.state.last_pull = Some(jiff::Timestamp::now());
                let mut fx = self.start_jobs(jobs);
                if !errors.is_empty() {
                    self.error(format!(
                        "{} forge listing(s) failed ($ for log)",
                        errors.len()
                    ));
                }
                fx.push(Effect::SaveState);
                self.rebuild();
                return fx;
            }
            Msg::Suspended(r) => {
                if let Err(e) = r {
                    self.error(e);
                }
                if let Some(p) = self.current_path() {
                    return vec![Effect::Refresh(vec![p])];
                }
            }
        }
        vec![]
    }

    fn push_log(&mut self, path: &std::path::Path, label: String, ok: bool, output: String) {
        let repo = self
            .ws
            .get(path)
            .map(|e| e.display_name())
            .unwrap_or_else(|| paths::contract_tilde(path));
        self.log.push(LogEntry {
            at: jiff::Timestamp::now(),
            repo,
            label,
            ok,
            output,
        });
        if self.log.len() > 1000 {
            self.log.drain(..100);
        }
    }
}

fn kinds_label(k: &[FixKind]) -> String {
    k.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
}

/// Run a user-configured command line through `sh -c` so quoting works.
fn shell_command(cmd: &str, cwd: PathBuf) -> Effect {
    Effect::Suspend {
        program: "sh".into(),
        args: vec!["-c".into(), cmd.into()],
        cwd,
    }
}

/// The name after `current` in `names`, wrapping; the first if unknown.
fn next<'a>(names: &[&'a str], current: &str) -> &'a str {
    let i = names.iter().position(|n| *n == current);
    names[i.map_or(0, |i| (i + 1) % names.len())]
}
