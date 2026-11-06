//! TUI application state machine. Pure: keys and worker messages go in,
//! [`Effect`]s come out. No terminal or threads here, so it's unit-testable.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use argus_core::config::StashConfig;
use argus_core::fix::{self, Applied, FixKind};
use argus_core::ops::{Op, OpOutcome};
use argus_core::rules::Mark;
use argus_core::status::RepoStatus;
use argus_core::sync::{self, Job, SyncPlan};
use argus_core::{Entry, ViewFilter, Workspace, paths};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::details::{DetailLine, details};
use crate::input::{Edit, LineEditor};
use crate::menu::{Menu, MenuId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Marked repos, or the repo under the cursor if none are marked.
    Selected,
    /// Every visible repo (for pull: every syncable one).
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
    Stash(StashKind),
    /// Flip a menu switch for this session.
    Toggle(Switch),
    Commit,
    SwitchDefault,
    FastForward,
    AutoFix(Scope),
    Sync,
    ToggleFavourite,
    ToggleSync,
    ToggleExclude,
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
    /// List forges and plan clones/pulls; answer with [`Msg::SyncPlanned`].
    PlanSync,
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
    SyncPlanned {
        tracks: argus_core::State,
        plan: SyncPlan,
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
    pub marked: BTreeSet<PathBuf>,
    /// Files marked in the details view, by repo path.
    pub marked_files: BTreeMap<PathBuf, BTreeSet<String>>,
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
            marked: BTreeSet::new(),
            marked_files: BTreeMap::new(),
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

    /// Recompute rows, keeping the cursor on the same repo where possible.
    pub fn rebuild(&mut self) {
        let keep = self.current_path();
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
                rows.extend(
                    details(e, &self.ws.config.fix.auto, &self.ws.config.stash, &icons)
                        .into_iter()
                        .map(|line| Row::Detail { repo: i, line }),
                );
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
        self.cursor = keep
            .and_then(|p| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, Row::Repo(i) if self.ws.entries[*i].path == p))
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

    fn info(&mut self, s: impl Into<String>) {
        self.message = Some((s.into(), false));
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
                }
            }
            ExpandAll => {
                self.expanded = self
                    .visible_repos()
                    .into_iter()
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                self.rebuild();
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
                    "hiding excluded/snoozed repos"
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
            Fetch(scope) => return self.jobs_for(scope, |_| Some(Op::Fetch), false),
            Pull(Scope::All) => {
                let jobs = sync::plan_updates(&self.ws);
                return self.start_jobs(jobs);
            }
            Pull(Scope::Selected) => {
                return self.jobs_for(
                    Scope::Selected,
                    |e| {
                        let s = e.status()?;
                        Some(if s.upstream.as_ref().is_some_and(|t| !t.gone) {
                            Op::Pull
                        } else {
                            Op::Fetch
                        })
                    },
                    false,
                );
            }
            Push => return self.fix_ops(FixKind::Push),
            Publish => return self.fix_ops(FixKind::Publish),
            SwitchDefault => return self.fix_ops(FixKind::SwitchDefault),
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
                    self.visible_repos()
                } else {
                    self.selected()
                };
                let kinds = self.ws.config.fix.auto.clone();
                let paths: Vec<PathBuf> = idx
                    .into_iter()
                    .filter(|&i| {
                        self.ws.entries[i].status().is_some_and(|s| {
                            fix::choose(s, &kinds, &self.ws.config.stash).is_some()
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
            Sync => {
                self.info("listing forges…");
                return vec![Effect::PlanSync];
            }
            ToggleFavourite => return self.toggle_mark(Mark::Favourite),
            ToggleSync => return self.toggle_mark(Mark::Sync),
            ToggleExclude => return self.toggle_mark(Mark::Exclude),
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

    /// Carry out a confirmed (or harmless enough) destructive action.
    fn confirmed(&mut self, c: Confirmed) -> Vec<Effect> {
        match c {
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

    /// Run the op of a specific fix kind on every selected repo where it applies.
    fn fix_ops(&mut self, kind: FixKind) -> Vec<Effect> {
        let stash = self.ws.config.stash;
        self.jobs_for(
            Scope::Selected,
            |e| {
                fix::plan(e.status()?, &stash)
                    .into_iter()
                    .find(|f| f.kind == kind)
                    .map(|f| f.op)
            },
            true,
        )
    }

    fn jobs_for(
        &mut self,
        scope: Scope,
        op: impl Fn(&Entry) -> Option<Op>,
        explain: bool,
    ) -> Vec<Effect> {
        let idx = if scope == Scope::All {
            self.visible_repos()
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
        if jobs.is_empty() && explain {
            self.info("not applicable to the selection (see details with Tab)");
        }
        self.start_jobs(jobs)
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
            match argus_core::ops::stash_problem(s, self.stash.untracked, staged, &paths) {
                Some(why) if why == argus_core::ops::NOTHING_TO_STASH => continue,
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
                self.rebuild();
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
                        sync::record_clones(&mut self.ws, &jobs, &[Some(outcome)]);
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
            Msg::SyncPlanned {
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
                jobs.extend(sync::plan_updates(&self.ws));
                self.ws.state.last_sync = Some(jiff::Timestamp::now());
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
    k.iter()
        .map(|k| serde_json_like(k))
        .collect::<Vec<_>>()
        .join(", ")
}

fn serde_json_like(k: &FixKind) -> &'static str {
    match k {
        FixKind::SwitchDefault => "switch-default",
        FixKind::Push => "push",
        FixKind::Publish => "publish",
        FixKind::FastForward => "fast-forward",
        FixKind::Pull => "pull",
        FixKind::Stash => "stash",
        FixKind::Commit => "commit",
    }
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
