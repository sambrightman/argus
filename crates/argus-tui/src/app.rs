//! TUI application state machine. Pure: keys and worker messages go in,
//! [`Effect`]s come out. No terminal or threads here, so it's unit-testable.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use argus_core::attention::Issue;
use argus_core::badges::Role;
use argus_core::config::StashConfig;
use argus_core::diff::{self, FileDiff, LineKind, Side};
use argus_core::fix::{self, Applied, FixKind};
use argus_core::gitignore::IgnoreFile;
use argus_core::ops::{self, Op, OpOutcome};
use argus_core::pull::{self, Job, PullPlan};
use argus_core::rules::{IgnoreState, IssueList, Mark};
use argus_core::status::RepoStatus;
use argus_core::{Entry, ViewFilter, Workspace, paths};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::details::{DetailLine, DiffRef, Expand, ago, describe, details};
use crate::input::{Edit, LineEditor};
use crate::menu::{self, Item, Menu, MenuId};

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
    /// Ask for a name, then create a branch at each selected detached HEAD
    /// that has commits on no branch.
    CreateBranch,
    /// Ask for a URL, then add it as `origin` to the current repo, if it
    /// has no remote.
    AddRemote,
    /// Abort the operation in progress in the selected repos, once confirmed.
    AbortOperation,
    /// Delete the selected repos' merged branches, once confirmed.
    DeleteMergedBranches,
    PopStash,
    /// Drop the stash under the cursor in the details, once confirmed.
    DropStash,
    FastForward,
    AutoFix(Scope),
    PullAndClone,
    ToggleFavourite,
    TogglePull,
    ToggleIgnore,
    /// Open the ignore menu for the repo under the cursor: its issues that
    /// can be ignored, each with its state.
    IgnoreIssues,
    /// In the ignore menu: move that issue (by its place) to its next
    /// state, or (`None`) ignore every counted one its default way.
    IgnoreStep(Option<usize>),
    /// Open the gitignore menu for the untracked files marked, or under
    /// the cursor.
    GitignoreMenu,
    /// In the gitignore menu: add the rules to that file.
    Gitignore(IgnoreFile),
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

/// Most commits a list unfolds into.
const MAX_COMMITS: usize = 50;
/// Most files an untracked directory unfolds into.
const MAX_DIR_FILES: usize = 200;

/// The rows directly below `line` if it's unfolded, indented two more
/// than it (six at least).
fn unfolded_rows(
    cache: &mut HashMap<(PathBuf, Expand, usize), Vec<DetailLine>>,
    unfolded: &BTreeSet<(PathBuf, Expand)>,
    repo: &std::path::Path,
    s: &RepoStatus,
    line: &DetailLine,
) -> Vec<DetailLine> {
    let Some(x) = &line.expand else {
        return vec![];
    };
    let text: String = line.parts.iter().map(|(_, t)| t.as_str()).collect();
    let indent = (text.len() - text.trim_start().len() + 2).max(6);
    if !unfolded.contains(&(repo.to_path_buf(), x.clone())) {
        return vec![];
    }
    cache
        .entry((repo.to_path_buf(), x.clone(), indent))
        .or_insert_with(|| unfold(repo, s, x, indent))
        .clone()
}

/// What `x` unfolds into, read now, indented `indent` spaces.
fn unfold(repo: &std::path::Path, s: &RepoStatus, x: &Expand, indent: usize) -> Vec<DetailLine> {
    use argus_core::history;
    let pad = " ".repeat(indent);
    let line = |parts: Vec<(Option<Role>, String)>| {
        let mut p = vec![(None, pad.clone())];
        p.extend(parts);
        DetailLine {
            parts: p,
            file: None,
            diff: None,
            issue: None,
            ignored: false,
            stash: None,
            expand: None,
        }
    };
    let dim = |t: String| (Some(Role::Dim), t);
    let failed = |e: String| vec![line(vec![(Some(Role::Error), e)])];
    match x {
        Expand::Commits(range) => match history::commits(repo, range, MAX_COMMITS) {
            Err(e) => failed(e),
            Ok((v, _)) if v.is_empty() => vec![line(vec![dim("no commits".into())])],
            Ok((v, more)) => {
                let mut rows: Vec<DetailLine> = v
                    .into_iter()
                    .map(|c| {
                        let when = c.time.map(|t| format!(", {}", ago(t))).unwrap_or_default();
                        DetailLine {
                            expand: Some(Expand::Commit(c.id.clone())),
                            ..line(vec![
                                dim(c.id.chars().take(7).collect()),
                                (None, format!(" {}", c.summary)),
                                dim(format!("  {}{when}", c.author)),
                            ])
                        }
                    })
                    .collect();
                if more {
                    rows.push(line(vec![dim(format!(
                        "… more (the newest {MAX_COMMITS} shown)"
                    ))]));
                }
                rows
            }
        },
        Expand::UnpushedBranches => s
            .branches
            .iter()
            .filter(|b| !b.is_head && b.is_unpushed())
            .map(|b| DetailLine {
                expand: Some(Expand::Commits(history::Range::NotOnRemote {
                    tip: format!("refs/heads/{}", b.name),
                })),
                ..line(vec![
                    (Some(Role::Branch), b.name.clone()),
                    dim(match b.unpushed {
                        Some(n) => format!("  {n} on no remote"),
                        None => "  on no remote".into(),
                    }),
                ])
            })
            .collect(),
        Expand::Commit(id) => match history::commit_detail(repo, id) {
            Err(e) => failed(e),
            Ok(d) => {
                let when = d.time.map(|t| format!(", {}", ago(t))).unwrap_or_default();
                let mut rows = vec![
                    line(vec![dim("commit ".into()), (None, d.id.clone())]),
                    line(vec![
                        dim("author ".into()),
                        (None, format!("{} <{}>{when}", d.author, d.email)),
                    ]),
                ];
                rows.extend(
                    d.message
                        .lines()
                        .map(|l| line(vec![(None, format!("  {l}"))])),
                );
                rows.push(line(vec![dim(format!("files ({})", d.files.len()))]));
                rows.extend(d.files.iter().map(|(code, path)| {
                    let role = match code {
                        'A' => Role::Ahead,
                        'D' => Role::Behind,
                        _ => Role::Unstaged,
                    };
                    DetailLine {
                        expand: Some(Expand::CommitFile(d.id.clone(), path.clone())),
                        ..line(vec![
                            (Some(role), format!("  {code}")),
                            (None, format!(" {path}")),
                        ])
                    }
                }));
                rows
            }
        },
        Expand::CommitFile(id, path) => match diff::commit_file_diff(repo, id, path) {
            Err(e) => failed(e.to_string()),
            Ok(FileDiff::Binary) => vec![line(vec![dim("binary file".into())])],
            Ok(FileDiff::External) => vec![line(vec![dim("external diff configured".into())])],
            Ok(FileDiff::Text(hunks)) if hunks.is_empty() => {
                vec![line(vec![dim("no change in content".into())])]
            }
            Ok(FileDiff::Text(hunks)) => {
                let mut rows = vec![];
                for h in hunks {
                    rows.push(line(vec![(Some(Role::DiffHunk), h.header())]));
                    for l in &h.lines {
                        let (role, mark) = match l.kind {
                            LineKind::Add => (Some(Role::DiffAdd), '+'),
                            LineKind::Remove => (Some(Role::DiffRemove), '-'),
                            LineKind::Context => (None, ' '),
                        };
                        let mut parts = vec![(role, format!("{mark}{}", l.text))];
                        if l.no_newline {
                            parts.push(dim("  (no newline at end of file)".into()));
                        }
                        rows.push(line(parts));
                    }
                }
                rows
            }
        },
        Expand::Dir(dir) => match argus_core::status::untracked_in(repo, dir, MAX_DIR_FILES) {
            Err(e) => failed(e.to_string()),
            Ok((files, more)) => {
                let mut rows: Vec<DetailLine> = files
                    .into_iter()
                    .map(|f| DetailLine {
                        file: Some(f.clone()),
                        ..line(vec![
                            (Some(Role::Untracked), "??".into()),
                            (None, format!(" {f}")),
                        ])
                    })
                    .collect();
                if more {
                    rows.push(line(vec![dim(format!(
                        "… more (the first {MAX_DIR_FILES} shown)"
                    ))]));
                }
                rows
            }
        },
    }
}

/// A row's identity across rebuilds: repo path, file, diff position
/// (side, hunk, whether a line), and what it unfolds into.
type RowKey = (
    PathBuf,
    Option<String>,
    Option<(Side, Option<usize>, bool)>,
    Option<Expand>,
);

/// The diff rows under an expanded file's line: each side with changes (unstaged first), its hunks
/// (unless folded) and their lines.
fn diff_rows(
    cache: &mut HashMap<(PathBuf, String, Side), Result<FileDiff, String>>,
    folded: &BTreeSet<(PathBuf, String, Side, usize)>,
    repo: &std::path::Path,
    s: &RepoStatus,
    file: &str,
) -> Vec<DetailLine> {
    let Some(c) = s.changes.entry(file) else {
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
        stash: None,
        expand: None,
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
    /// In the gitignore menu: rules match only their path (`/build/`), not
    /// the name anywhere (`build/`).
    Anchored,
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
        /// An interactive shell, started as a login shell (see
        /// [`crate::suspended_command`]).
        login_shell: bool,
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
    BranchName(Vec<PathBuf>),
    RemoteUrl(PathBuf),
    SnoozeDuration(Vec<String>),
}

pub enum Mode {
    Normal,
    Menu {
        menu: Menu,
        cursor: usize,
        /// What its actions would do, shown above them.
        notes: Vec<String>,
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
    /// Operations to run, by repo path.
    Ops(Vec<(PathBuf, Op)>),
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
    /// Details lines unfolded (Tab), by repo.
    pub unfolded: BTreeSet<(PathBuf, Expand)>,
    /// What they unfolded into, by indent, read once until the repo's
    /// status changes.
    unfold_cache: HashMap<(PathBuf, Expand, usize), Vec<DetailLine>>,
    /// The stash switches for this session.
    pub stash: StashConfig,
    pub running: HashMap<PathBuf, String>,
    /// Auto-fixes still running from the last auto-fix action, and what
    /// the first did, to say what's done.
    auto_fixes: (usize, String),
    /// The repo the open ignore menu is for.
    ignore_menu: Option<PathBuf>,
    /// The untracked files the open gitignore menu is for, by repo, and
    /// whether rules are anchored.
    gitignore: Option<GitignoreTargets>,
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
            unfolded: BTreeSet::new(),
            unfold_cache: HashMap::new(),
            stash,
            running: HashMap::new(),
            auto_fixes: (0, String::new()),
            ignore_menu: None,
            gitignore: None,
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
            Row::Repo(i) => Some((self.ws.entries[*i].path.clone(), None, None, None)),
            Row::Detail { repo, line } => Some((
                self.ws.entries[*repo].path.clone(),
                line.file.clone(),
                line.diff
                    .as_ref()
                    .map(|d| (d.side, d.hunk, d.line.is_some())),
                line.expand.clone(),
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
                // depth first: each line, its diff, then what it unfolds into
                let mut stack: Vec<DetailLine> =
                    details(e, &self.ws.config.fix.auto, &self.ws.config.stash, &icons)
                        .into_iter()
                        .rev()
                        .collect();
                while let Some(line) = stack.pop() {
                    let expanded = line.file.as_ref().filter(|f| {
                        self.expanded_files
                            .contains(&(e.path.clone(), f.to_string()))
                    });
                    let (diff, below) = match e.status() {
                        Some(s) => (
                            match expanded {
                                Some(f) => {
                                    diff_rows(&mut self.diffs, &self.folded_hunks, &e.path, s, f)
                                }
                                None => vec![],
                            },
                            unfolded_rows(
                                &mut self.unfold_cache,
                                &self.unfolded,
                                &e.path,
                                s,
                                &line,
                            ),
                        ),
                        None => (vec![], vec![]),
                    };
                    rows.push(Row::Detail { repo: i, line });
                    rows.extend(diff.into_iter().map(|line| Row::Detail { repo: i, line }));
                    stack.extend(below.into_iter().rev());
                }
            }
        }
        self.rows = rows;
        // marks on files that are no longer changed go
        let ws = &self.ws;
        self.marked_files.retain(|repo, files| {
            let changed = ws.get(repo).and_then(|e| e.status()).map(|s| &s.changes);
            files.retain(|f| changed.is_some_and(|c| c.entry(f).is_some()));
            !files.is_empty()
        });
        let exact = target.as_ref().and_then(|t| {
            let file_line = (t.0.clone(), t.1.clone(), None, None);
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
            Mode::Menu { menu, cursor, .. } => {
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
                        let (a, stays) = (it.action, it.stays);
                        if !stays {
                            self.mode = Mode::Normal;
                        }
                        return self.act(a);
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if let Some(it) = menu.items.iter().find(|i| i.key == c) {
                            // switches keep the menu open, as in magit
                            let (a, stays) = (it.action, it.stays);
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
                'i' => GitignoreMenu,
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
            ToggleExpand if self.on_unfoldable() => return self.toggle_unfold(),
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
            IgnoreStep(which) => return self.ignore_step(which),
            GitignoreMenu => return self.open_gitignore_menu(),
            Toggle(Switch::Anchored) => {
                if let Some(g) = &mut self.gitignore {
                    g.anchored = !g.anchored;
                }
                // the items show the rules
                if let (Some(menu), Mode::Menu { menu: open, .. }) =
                    (self.gitignore_menu_items(), &mut self.mode)
                {
                    *open = menu;
                }
            }
            Gitignore(file) => {
                let Some(GitignoreTargets { targets, anchored }) = self.gitignore.take() else {
                    return vec![];
                };
                let jobs = targets
                    .into_iter()
                    .filter_map(|(p, paths)| {
                        self.job(
                            &p,
                            Op::Ignore {
                                file,
                                paths,
                                anchored,
                            },
                        )
                    })
                    .collect();
                return self.start_jobs(jobs);
            }
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
                let notes = self.menu_notes(m);
                let mut menu = m.menu();
                self.state_labels(&mut menu);
                self.mode = Mode::Menu {
                    menu,
                    cursor: 0,
                    notes,
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
            CreateBranch => {
                let targets = self.applicable(FixKind::Branch);
                if targets.is_empty() {
                    self.info("no detached HEAD with commits on no branch selected");
                } else {
                    self.mode = Mode::Input {
                        prompt: Prompt::BranchName(targets),
                        editor: LineEditor::default(),
                    };
                }
            }
            AbortOperation => {
                let p = self.planned(FixKind::Abort);
                let ops = p.ops.clone();
                let question = match ops.as_slice() {
                    [] => return self.not_applicable("nothing in progress"),
                    [(p, op)] => format!(
                        "{} in {}? What it did so far (resolved conflicts, commits) is lost.",
                        capitalised(&op.label()),
                        self.name_of(p)
                    ),
                    _ => format!(
                        "Abort what's in progress in {} repos? What each did so far is lost.",
                        ops.len()
                    ),
                } + &p.note();
                self.mode = Mode::Confirm {
                    question,
                    then: Confirmed::Ops(ops),
                };
            }
            DeleteMergedBranches => {
                let p = self.planned(FixKind::DeleteBranches);
                let ops = p.ops.clone();
                if ops.is_empty() {
                    return self.not_applicable("no merged branches to delete");
                }
                let names: Vec<String> = ops
                    .iter()
                    .filter_map(|(_, op)| match op {
                        Op::DeleteBranch { branch } => Some(branch.clone()),
                        _ => None,
                    })
                    .collect();
                self.mode = Mode::Confirm {
                    question: format!(
                        "Delete {} merged branch(es): {}?{}",
                        names.len(),
                        fix::list(&names, 3),
                        p.note()
                    ),
                    then: Confirmed::Ops(ops),
                };
            }
            PopStash => return self.fix_ops(FixKind::StashPop),
            DropStash => {
                let Some(Row::Detail {
                    repo,
                    line: DetailLine { stash: Some(n), .. },
                }) = self.rows.get(self.cursor)
                else {
                    self.info("put the cursor on a stash in the details (Tab)");
                    return vec![];
                };
                let (i, n) = (*repo, *n);
                let e = &self.ws.entries[i];
                let Some(s) = e.status() else { return vec![] };
                let (Some(message), Some(id)) = (s.stashes.get(n), s.stash_ids.get(n)) else {
                    return vec![];
                };
                self.mode = Mode::Confirm {
                    question: format!(
                        "Drop stash@{{{n}}} ({message}) of {}? It can't be recovered.",
                        e.display_name()
                    ),
                    then: Confirmed::Ops(vec![(
                        e.path.clone(),
                        Op::StashDrop {
                            index: n,
                            id: id.clone(),
                        },
                    )]),
                };
            }
            AddRemote => match self.applicable(FixKind::AddRemote).as_slice() {
                [] => self.info("the selection has remotes"),
                [one] => {
                    self.mode = Mode::Input {
                        prompt: Prompt::RemoteUrl(one.clone()),
                        editor: LineEditor::default(),
                    }
                }
                _ => self.error("add a remote to one repo at a time"),
            },
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
                let total = idx.len();
                let fixable: Vec<PathBuf> = idx
                    .into_iter()
                    .filter(|&i| {
                        let e = &self.ws.entries[i];
                        e.status().is_some_and(|s| {
                            fix::choose(s, &e.fix_kinds(&kinds), &self.ws.config.stash).is_some()
                        })
                    })
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                let none = skipped(total - fixable.len(), "with nothing to apply");
                let (paths, busy): (Vec<PathBuf>, Vec<PathBuf>) = fixable
                    .into_iter()
                    .partition(|p| !self.running.contains_key(p));
                let busy = skipped(busy.len(), "busy");
                if paths.is_empty() {
                    match busy.is_empty() {
                        true => self.info(format!("no auto-fixes ({}) apply", kinds_label(&kinds))),
                        false => self.info(format!("nothing started{busy}")),
                    }
                    return vec![];
                }
                let what = self.auto_fix_summary(&paths).unwrap_or_default();
                for p in &paths {
                    self.running.insert(p.clone(), "fix".into());
                }
                self.auto_fixes = (paths.len(), what.clone());
                self.info(format!("{what}…{none}{busy}"));
                return vec![Effect::Fix { paths, kinds }];
            }
            PullAndClone => {
                self.info("listing forges…");
                return vec![Effect::PlanPullAndClone];
            }
            ToggleFavourite => return self.toggle_mark(Mark::Favourite),
            TogglePull => return self.toggle_mark(Mark::Pull),
            ToggleIgnore => return self.toggle_mark(Mark::Ignore),
            IgnoreIssues => return self.open_ignore_menu(),
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
                        login_shell: true,
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
    /// The ignore menu for the repo under the cursor, on the issue under
    /// it if that's one.
    fn open_ignore_menu(&mut self) -> Vec<Effect> {
        let (i, at) = match self.rows.get(self.cursor) {
            Some(Row::Repo(i)) => (*i, None),
            Some(Row::Detail { repo, line }) => (*repo, line.issue.and_then(IssueList::of)),
            None => return vec![],
        };
        let path = self.ws.entries[i].path.clone();
        self.ignore_menu = Some(path);
        let Some(menu) = self.ignore_menu_items() else {
            self.ignore_menu = None;
            return self.not_applicable("no issue of this repo can be ignored");
        };
        let lists = self.ignorable(i);
        let cursor = at
            .and_then(|l| lists.iter().position(|(m, _)| *m == l))
            .unwrap_or(0);
        self.mode = Mode::Menu {
            menu,
            cursor,
            notes: vec![],
        };
        vec![]
    }

    /// The gitignore menu for the untracked files marked, or under the
    /// cursor (tracked ones left out: rules don't apply to them).
    fn open_gitignore_menu(&mut self) -> Vec<Effect> {
        let targets: Vec<(PathBuf, Vec<String>)> = self
            .file_targets()
            .into_iter()
            .filter_map(|(i, files)| {
                let e = &self.ws.entries[i];
                let s = e.status()?;
                let untracked: Vec<String> = files
                    .into_iter()
                    .filter(|f| s.changes.entry(f).is_some_and(|c| c.index == '?'))
                    .collect();
                (!untracked.is_empty()).then(|| (e.path.clone(), untracked))
            })
            .collect();
        if targets.is_empty() {
            return self.not_applicable(
                "no untracked file selected: put the cursor on one in the details, or mark some",
            );
        }
        self.gitignore = Some(GitignoreTargets {
            targets,
            anchored: true,
        });
        if let Some(menu) = self.gitignore_menu_items() {
            self.mode = Mode::Menu {
                menu,
                cursor: 0,
                notes: vec![],
            };
        }
        vec![]
    }

    /// The gitignore menu: each place, with the rules it would get.
    fn gitignore_menu_items(&self) -> Option<Menu> {
        use argus_core::gitignore::{dir_of, rule};
        let GitignoreTargets { targets, anchored } = self.gitignore.as_ref()?;
        let paths: Vec<&String> = targets.iter().flat_map(|(_, p)| p).collect();
        let rules = |base: &dyn Fn(&str) -> String| -> String {
            let r: Vec<String> = paths.iter().map(|p| rule(p, &base(p), *anchored)).collect();
            fix::list(&r, 2)
        };
        let top = rules(&|_| String::new());
        let mut dirs: Vec<&str> = paths.iter().map(|p| dir_of(p)).collect();
        dirs.dedup();
        let own = rules(&|p| dir_of(p).to_string());
        let dir_label = match dirs.as_slice() {
            [d] if targets.len() == 1 => {
                let at = if d.is_empty() {
                    String::new()
                } else {
                    format!("{d}/")
                };
                format!("add {own} to {at}.gitignore")
            }
            _ => format!("add {own} to each file's directory's .gitignore"),
        };
        let n = paths.len();
        Some(Menu {
            id: MenuId::Gitignore,
            title: format!("Gitignore · {n} untracked file(s)"),
            items: vec![
                menu::item(
                    't',
                    format!("add {top} to .gitignore"),
                    Action::Gitignore(IgnoreFile::TopLevel),
                ),
                menu::item('d', dir_label, Action::Gitignore(IgnoreFile::Directory)),
                menu::item(
                    'p',
                    format!("add {top} to .git/info/exclude (this clone only)"),
                    Action::Gitignore(IgnoreFile::Private),
                ),
                menu::item(
                    'g',
                    format!("add {top} to the global excludes file (every repo)"),
                    Action::Gitignore(IgnoreFile::Global),
                ),
                menu::switch(
                    'a',
                    "anchored: only this path, not the name anywhere",
                    Switch::Anchored,
                ),
            ],
        })
    }

    /// Repo `i`'s issues that can be ignored, counted or not, one per
    /// list (stashes may be both), in the order the details show them.
    fn ignorable(&self, i: usize) -> Vec<(IssueList, Issue)> {
        let e = &self.ws.entries[i];
        let mut v: Vec<(IssueList, Issue)> = vec![];
        for &issue in e.issues.iter().chain(&e.ignored_issues) {
            let Some(list) = IssueList::of(issue) else {
                continue;
            };
            if v.iter().all(|(l, _)| *l != list) {
                v.push((list, issue));
            }
        }
        // all of them, ignored or not
        if let Some(s) = e.status() {
            for (list, issue) in &mut v {
                if *list == IssueList::Stashes {
                    *issue = Issue::Stashes(s.stashes.len());
                }
            }
        }
        v
    }

    /// The ignore menu for [`Self::ignore_menu`]: each issue with a symbol
    /// and words for its state, and what `a` does. `None` if it has none.
    fn ignore_menu_items(&self) -> Option<Menu> {
        let i = self.ws.index_of(self.ignore_menu.as_ref()?)?;
        let e = &self.ws.entries[i];
        let s = e.status()?;
        let lists = self.ignorable(i);
        if lists.is_empty() {
            return None;
        }
        let (icons, _) = self.ws.config.ui.icons();
        let mut counted = 0;
        let mut items: Vec<Item> = vec![];
        for (n, (list, issue)) in lists.iter().enumerate() {
            let state = self.ws.issue_state(i, *list).ok()?;
            if state == IgnoreState::Counted {
                counted += 1;
            }
            let key = char::from_digit(n as u32 + 1, 10).unwrap_or('?');
            let partly = match list {
                IssueList::Stashes if state == IgnoreState::Counted => {
                    e.stash_ignored.iter().filter(|&&b| b).count()
                }
                _ => 0,
            };
            let label = state_label(*issue, *list, state, partly, s, &icons);
            items.push(menu::stepper(key, label, Action::IgnoreStep(Some(n))));
        }
        items.push(menu::stepper(
            'a',
            match counted {
                0 => "all: no counted issue to ignore".to_string(),
                n => format!("all: ignore {n} counted issue(s), each its default way"),
            },
            Action::IgnoreStep(None),
        ));
        Some(Menu {
            id: MenuId::Ignore,
            title: format!("Ignore issues · {}", e.display_name()),
            items,
        })
    }

    /// Move the ignore menu's issue `which` to its next state, or every
    /// counted one to its default; the menu stays open, showing the result.
    fn ignore_step(&mut self, which: Option<usize>) -> Vec<Effect> {
        let Some(i) = self.ignore_menu.as_ref().and_then(|p| self.ws.index_of(p)) else {
            return vec![];
        };
        let lists = self.ignorable(i);
        let mut changes = vec![];
        for (n, (list, _)) in lists.iter().enumerate() {
            let Ok(now) = self.ws.issue_state(i, *list) else {
                continue;
            };
            match which {
                Some(w) if w == n => changes.push((*list, now.next(*list))),
                None if now == IgnoreState::Counted => {
                    changes.push((*list, IgnoreState::default_for(*list)))
                }
                _ => {}
            }
        }
        if changes.is_empty() {
            return vec![];
        }
        let before = self.ws.config.clone();
        for (list, to) in &changes {
            if let Err(e) = self.ws.set_issue_state(&[i], *list, *to) {
                self.error(e.to_string());
                return vec![];
            }
        }
        self.rebuild();
        if let (Some(menu), Mode::Menu { menu: open, .. }) =
            (self.ignore_menu_items(), &mut self.mode)
        {
            *open = menu;
        }
        vec![Effect::SaveMarks(Box::new(before))]
    }

    /// What auto-fixing the repos at `paths` would do: `auto-fix acme/w:
    /// push`, or how many of each kind for several. `None` for nothing.
    fn auto_fix_summary(&self, paths: &[PathBuf]) -> Option<String> {
        let kinds = &self.ws.config.fix.auto;
        let fixes: Vec<(String, fix::Fix)> = paths
            .iter()
            .filter_map(|p| {
                let e = self.ws.get(p)?;
                let f = fix::choose(e.status()?, &e.fix_kinds(kinds), &self.ws.config.stash)?;
                Some((e.display_name(), f))
            })
            .collect();
        match fixes.as_slice() {
            [] => None,
            [(name, f)] => Some(format!("auto-fix {name}: {}", f.op.label())),
            _ => {
                // how many of each kind, in priority order
                let mut counts: Vec<(FixKind, usize)> = vec![];
                for (_, f) in &fixes {
                    match counts.iter_mut().find(|c| c.0 == f.kind) {
                        Some(c) => c.1 += 1,
                        None => counts.push((f.kind, 1)),
                    }
                }
                let tally: Vec<String> = counts
                    .iter()
                    .map(|(k, n)| format!("{} ({n})", k.name()))
                    .collect();
                Some(format!(
                    "auto-fixing {} repo(s): {}",
                    fixes.len(),
                    tally.join(", ")
                ))
            }
        }
    }

    /// Labels for toggles saying what choosing them does now: mark or
    /// unmark (by the first selected repo, as toggling goes), show or hide.
    fn state_labels(&self, menu: &mut Menu) {
        let first = self.selected().first().map(|&i| &self.ws.entries[i].flags);
        let label = |key: char| -> Option<&'static str> {
            Some(match (menu.id, key) {
                (MenuId::Mark, 'f') if first?.favourite => "unfavourite",
                (MenuId::Mark, 'f') => "favourite",
                (MenuId::Mark, 'p') if first?.pull => "stop pulling",
                (MenuId::Mark, 'p') => "pull (with F a and argus pull)",
                (MenuId::Mark, 'I') if first?.ignored => "unignore repo",
                (MenuId::Mark, 'I') => "ignore repo: hidden, no pulls or bulk actions",
                (MenuId::View, 'a') if self.view.attention_only => "show all repos",
                (MenuId::View, 'a') => "show only repos needing attention",
                (MenuId::View, 'h') if self.view.show_hidden => "hide ignored and snoozed repos",
                (MenuId::View, 'h') => "show ignored and snoozed repos",
                (MenuId::View, 'l') if self.show_log => "hide process log",
                (MenuId::View, 'l') => "show process log",
                _ => return None,
            })
        };
        let labels: Vec<Option<&str>> = menu.items.iter().map(|it| label(it.key)).collect();
        for (it, l) in menu.items.iter_mut().zip(labels) {
            if let Some(l) = l {
                it.label = l.to_string();
            }
        }
    }

    /// Lines for the top of menu `m`: for Fix, what `x` and `a` would do.
    fn menu_notes(&self, m: MenuId) -> Vec<String> {
        if m != MenuId::Fix {
            return vec![];
        }
        let paths = |idx: Vec<usize>| -> Vec<PathBuf> {
            idx.into_iter()
                .map(|i| self.ws.entries[i].path.clone())
                .collect()
        };
        [
            ('x', paths(self.selected())),
            ('a', paths(self.bulk_repos())),
        ]
        .into_iter()
        .map(|(key, p)| {
            let what = self
                .auto_fix_summary(&p)
                .unwrap_or_else(|| "nothing to auto-fix".into());
            format!("{key}: {what}")
        })
        .collect()
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
            Confirmed::Ops(ops) => {
                let jobs = ops
                    .into_iter()
                    .filter_map(|(p, op)| self.job(&p, op))
                    .collect();
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

    /// Every fix of `kind` the selected repos have and can run, and what
    /// was left out.
    fn planned(&self, kind: FixKind) -> Planned {
        let stash = self.ws.config.stash;
        let mut p = Planned::default();
        for i in self.selected() {
            let e = &self.ws.entries[i];
            let Some(s) = e.status() else { continue };
            let fixes: Vec<_> = fix::plan(s, &stash)
                .into_iter()
                .filter(|f| f.kind == kind)
                .collect();
            if fixes.is_empty() {
                p.none += 1;
            }
            for f in fixes {
                match f.blocked {
                    Some(why) => {
                        p.blocked += 1;
                        p.why.get_or_insert(format!("{}: {why}", e.display_name()));
                    }
                    None => p.ops.push((e.path.clone(), f.op)),
                }
            }
        }
        p
    }

    fn not_applicable(&mut self, why: &str) -> Vec<Effect> {
        self.info(why.to_string());
        vec![]
    }

    fn name_of(&self, path: &std::path::Path) -> String {
        self.ws
            .get(path)
            .map_or_else(|| path.display().to_string(), |e| e.display_name())
    }

    /// The selected repos a fix of `kind` applies to.
    fn applicable(&self, kind: FixKind) -> Vec<PathBuf> {
        let stash = self.ws.config.stash;
        self.selected()
            .into_iter()
            .map(|i| &self.ws.entries[i])
            .filter(|e| {
                e.status()
                    .is_some_and(|s| fix::plan(s, &stash).iter().any(|f| f.kind == kind))
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Run every fix of a kind (one per branch, for kinds that have
    /// several) on the selected repos where it applies and isn't blocked.
    fn fix_ops(&mut self, kind: FixKind) -> Vec<Effect> {
        let p = self.planned(kind);
        if p.ops.is_empty() {
            match p.why {
                Some(why) => self.error(why),
                None => self.info("not applicable to the selection (see details with Tab)"),
            }
            return vec![];
        }
        let note = p.note();
        let jobs = p
            .ops
            .into_iter()
            .filter_map(|(path, op)| self.job(&path, op))
            .collect();
        let fx = self.start_jobs(jobs);
        self.add_note(&note);
        fx
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

    fn on_unfoldable(&self) -> bool {
        matches!(self.rows.get(self.cursor), Some(Row::Detail { line, .. }) if line.expand.is_some())
    }

    /// Tab on a line that unfolds: shows or hides what's below it.
    fn toggle_unfold(&mut self) -> Vec<Effect> {
        let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor) else {
            return vec![];
        };
        let Some(x) = line.expand.clone() else {
            return vec![];
        };
        let key = (self.ws.entries[*repo].path.clone(), x);
        if !self.unfolded.remove(&key) {
            self.unfolded.insert(key);
        }
        let at = self.cursor;
        self.rebuild();
        self.cursor = at.min(self.rows.len().saturating_sub(1));
        vec![]
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
                self.rebuild_at(Some((path, Some(file), Some((side, Some(h), false)), None)));
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
            Switch::Anchored => self.gitignore.as_ref().is_some_and(|g| g.anchored),
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

    /// Start `jobs`, except in repos already running something (said so).
    fn start_jobs(&mut self, mut jobs: Vec<Job>) -> Vec<Effect> {
        let busy: BTreeSet<PathBuf> = jobs
            .iter()
            .filter(|j| self.running.contains_key(&j.path))
            .map(|j| j.path.clone())
            .collect();
        jobs.retain(|j| !busy.contains(&j.path));
        let busy = skipped(busy.len(), "busy");
        if jobs.is_empty() {
            if !busy.is_empty() {
                self.info(format!("nothing started{busy}"));
            }
            return vec![];
        }
        for j in &jobs {
            self.running.insert(j.path.clone(), j.op.label());
        }
        self.info(format!("running {} job(s)…{busy}", jobs.len()));
        vec![Effect::Jobs(jobs)]
    }

    /// Add `note` to the status line's message.
    fn add_note(&mut self, note: &str) {
        if let Some((m, _)) = &mut self.message {
            m.push_str(note);
        }
    }

    /// A job running `op` in the repo at `path`, if it's still there.
    fn job(&self, path: &std::path::Path, op: Op) -> Option<Job> {
        let e = self.ws.get(path)?;
        Some(Job {
            label: format!("{} {}", op.label(), e.display_name()),
            family: e.parent.clone().unwrap_or_else(|| path.to_path_buf()),
            path: path.to_path_buf(),
            op,
        })
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
            Prompt::BranchName(_) | Prompt::RemoteUrl(_) if text.trim().is_empty() => {
                self.error("nothing entered: aborted");
                vec![]
            }
            Prompt::BranchName(paths) => {
                let name = text.trim().to_string();
                let jobs = paths
                    .iter()
                    .filter_map(|p| self.job(p, Op::CreateBranch { name: name.clone() }))
                    .collect();
                self.start_jobs(jobs)
            }
            Prompt::RemoteUrl(path) => {
                let op = Op::AddRemote {
                    name: "origin".into(),
                    url: text.trim().to_string(),
                };
                let jobs = self.job(&path, op).into_iter().collect();
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
                self.unfold_cache.retain(|k, _| k.0 != path);
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
                self.auto_fixes.0 = self.auto_fixes.0.saturating_sub(1);
                if self.auto_fixes.0 == 0 && self.message.as_ref().is_none_or(|m| !m.1) {
                    let what = std::mem::take(&mut self.auto_fixes.1);
                    self.info(format!("{what}: done"));
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

/// The untracked files a gitignore menu is for, by repo, and whether its
/// rules are anchored.
struct GitignoreTargets {
    targets: Vec<(PathBuf, Vec<String>)>,
    anchored: bool,
}

/// An ignore menu item: the state's symbol, then the issue and, when
/// ignored, how (`ignored while on feat`, `ignored on any branch`). For
/// counted stashes, how many config patterns ignore anyway (`partly`).
fn state_label(
    issue: Issue,
    list: IssueList,
    state: IgnoreState,
    partly: usize,
    s: &RepoStatus,
    icons: &argus_core::icons::Icons,
) -> String {
    let (_, text) = describe(&issue, s.default_branch.as_deref());
    let first = list.values(s).into_iter().next().unwrap_or_default();
    let (symbol, how) = match state {
        IgnoreState::Counted if partly > 0 => (
            &icons.issue_counted,
            format!(", {partly} ignored by patterns"),
        ),
        IgnoreState::Counted => (&icons.issue_counted, String::new()),
        IgnoreState::Pinned => (
            &icons.issue_pinned,
            match list {
                IssueList::Detached => format!(", ignored while at {first}"),
                IssueList::Stashes => ", ignored for these messages".into(),
                _ => format!(", ignored while on {first}"),
            },
        ),
        IgnoreState::Ignored => (
            &icons.issue_ignored,
            match list {
                IssueList::NonDefaultBranch | IssueList::NoUpstream => {
                    ", ignored on any branch".into()
                }
                IssueList::Detached => ", ignored wherever HEAD is".into(),
                IssueList::Stashes => ", ignored, later ones too".into(),
                _ => ", ignored".into(),
            },
        ),
    };
    format!("{} {text}{how}", symbol.trim_end())
}

/// The fixes of one kind an action runs, and what it leaves out.
#[derive(Default)]
struct Planned {
    ops: Vec<(PathBuf, Op)>,
    /// Selected repos with no fix of the kind.
    none: usize,
    /// Fixes left out as blocked, and the first one's reason.
    blocked: usize,
    why: Option<String>,
}

impl Planned {
    /// ` · 2 not applicable · 1 blocked (acme/w: …)`, or nothing.
    fn note(&self) -> String {
        let mut s = skipped(self.none, "not applicable");
        if let Some(why) = &self.why {
            s.push_str(&format!("{} ({why})", skipped(self.blocked, "blocked")));
        }
        s
    }
}

/// ` · 2 busy`, or nothing for none.
fn skipped(n: usize, what: &str) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" · {n} {what}")
    }
}

/// `abort merge` -> `Abort merge`
fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
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
        login_shell: false,
    }
}

/// The name after `current` in `names`, wrapping; the first if unknown.
fn next<'a>(names: &[&'a str], current: &str) -> &'a str {
    let i = names.iter().position(|n| *n == current);
    names[i.map_or(0, |i| (i + 1) % names.len())]
}
