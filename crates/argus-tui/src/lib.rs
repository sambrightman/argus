//! argus-tui: the interactive front-end, as a library so other binaries can
//! embed it. [`app::App`] holds all logic and asks for effects; [`Runtime`]
//! carries them out (files, background work, opening URLs), and [`run`]
//! wires it to the terminal: drawing, keys, and suspending for a shell or
//! editor.

pub mod app;
pub mod details;
pub mod fuzzy;
pub mod input;
pub mod menu;
pub mod messages;
pub mod refine;
pub mod theme;
pub mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use argus_core::Workspace;
use argus_core::pull::{self, ForgeFactory};
use argus_core::runner::{self, Event as RunEvent};
use argus_core::status;
use argus_core::{configfile, fix};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::event::{self, Event, KeyEventKind};

use app::{App, Effect, Msg};

pub struct Options {
    pub state_path: PathBuf,
    /// Where tags (favourite, pull, ignore) are saved.
    pub config_path: PathBuf,
    pub forge_factory: ForgeFactory<'static>,
    /// Startup notices (e.g. the current-directory fallback), shown after scans.
    pub notices: Vec<String>,
    /// The program opening URLs (see [`default_opener`]).
    pub opener: String,
    /// Where argus was started: the cursor starts on the repo holding it.
    pub start_in: Option<PathBuf>,
    /// Steps taken go to the message history ([`argus_core::trace`]).
    pub verbose: bool,
    /// Steps taken before the TUI started, for the history.
    pub early_steps: Vec<argus_core::trace::Step>,
    /// The startup step, begun before the TUI: it ends once the first
    /// scan's rows are in, holding everything until then (the scan too).
    pub startup: Option<argus_core::trace::Begun>,
}

/// `open` on macOS, `xdg-open` elsewhere.
pub fn default_opener() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// The TUI needs a terminal on stdin and stdout (not a pipe, or a tool
/// that runs commands without one).
pub fn check_terminal() -> Result<(), Error> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err("the TUI needs a terminal; run argus from one, or use `argus status`".into())
    }
}

pub fn run(mut ws: Workspace, opts: Options) -> Result<(), Error> {
    // refreshes reuse repos rather than reopening them each time
    ws.keep_repos_open();
    // the rows first: a repo's older branches are checked after
    ws.check_branches_later();
    check_terminal()?;
    let (mut theme, mut warnings) = theme::Theme::for_config(&ws.config);
    warnings.extend(menu::tag_key_clashes(&ws.config));
    let mut theme_name = ws.config.ui.theme.clone();
    let (mut icons, icon_warnings) = ws.config.icons();
    let mut icons_name = ws.config.ui.icons.clone();
    let mut app = App::new(ws);
    app.notices = opts.notices.clone();
    app.start_in = opts.start_in.clone();
    if let Some(w) = warnings.iter().chain(&icon_warnings).next() {
        app.report_error(format!("config: {w}"));
    }
    let mut rt = Runtime::new(opts);
    rt.log_steps(&mut app);

    let mut terminal = init_terminal(&rt.panics);
    app.scanning = true;
    rt.exec(&mut app, Effect::Scan);
    let mut tick = 0usize;
    // Drawn only when something may have changed, or while the spinner
    // turns: even a frame that changes nothing writes to the terminal,
    // which terminals show as activity (iTerm's tab spinner).
    let mut dirty = true;
    let mut drawn = std::time::Instant::now();
    let result = (|| -> Result<(), Error> {
        while !app.quit {
            // The View menu can switch theme and icons for the session.
            if app.ws.config.ui.theme != theme_name {
                theme_name = app.ws.config.ui.theme.clone();
                theme = theme::Theme::for_config(&app.ws.config).0;
            }
            if app.ws.config.ui.icons != icons_name {
                icons_name = app.ws.config.ui.icons.clone();
                icons = app.ws.config.icons().0;
            }
            let view = ui::View {
                theme: &theme,
                icons: icons.clone(),
                tick,
            };
            for what in rt.take_panics() {
                app.report_error(format!("a background task failed (a bug): {what}"));
                dirty = true;
            }
            let busy = app.scanning || !app.running.is_empty();
            // (the spinner turns at its own pace however often results
            // are looked for)
            if dirty || (busy && drawn.elapsed() >= SPIN) {
                tick = tick.wrapping_add(1);
                let mut links = vec![];
                argus_core::trace::if_slow(
                    "frame",
                    || "frame drawn, slowly".into(),
                    || terminal.draw(|f| links = ui::draw(f, &mut app, &view)),
                )?;
                ui::write_links(terminal.backend_mut(), &links)?;
                drawn = std::time::Instant::now();
            }
            dirty = false;
            let mut left = vec![];
            if event::poll(wait_for_key(rt.working() || rt.has_results(), busy))? {
                // a resize too
                dirty = true;
                if let Event::Key(k) = event::read()?
                    && k.kind != KeyEventKind::Release
                {
                    for e in app.on_key(k) {
                        left.extend(rt.exec(&mut app, e));
                    }
                }
            }
            let (effects, received) = rt.drain(&mut app);
            left.extend(effects);
            dirty |= received;
            // only suspending is left: it needs the terminal
            while let Some(e) = left.pop() {
                if let Effect::Suspend {
                    program,
                    args,
                    cwd,
                    login_shell,
                } = e
                {
                    // restore() leaves the cursor hidden: only dropping the
                    // terminal shows it again
                    let _ = terminal.show_cursor();
                    ratatui::restore();
                    argus_core::trace::note(|| {
                        format!(
                            "suspended for {program} in {}",
                            argus_core::paths::contract_tilde(&cwd)
                        )
                    });
                    println!("{}", suspend_banner(&program, &cwd));
                    let r = run_suspended(&program, &args, &cwd, login_shell);
                    dirty = true;
                    terminal = init_terminal(&rt.panics);
                    let _ = terminal.clear();
                    for e in app.on_msg(Msg::Suspended(r)) {
                        left.extend(rt.exec(&mut app, e));
                    }
                }
            }
        }
        Ok(())
    })();
    rt.cancel.store(true, Ordering::Relaxed);
    ratatui::restore();
    result
}

/// Run `program` with the terminal handed over (shell, git UI, editor),
/// waiting for it. A login shell exits with its last command's code, which
/// isn't an error.
pub fn run_suspended(
    program: &str,
    args: &[String],
    cwd: &Path,
    login_shell: bool,
) -> Result<(), String> {
    suspended_command(program, args, cwd, login_shell)
        .status()
        .map_err(|e| format!("{program}: {e}"))
        .and_then(|s| {
            if s.success() || login_shell {
                Ok(())
            } else {
                Err(format!("{program} exited with {s}"))
            }
        })
}

/// What's said on leaving the TUI for `program` in `cwd` (their names
/// shown as text, whatever is in them).
pub fn suspend_banner(program: &str, cwd: &Path) -> String {
    let said = format!(
        "argus: {} in {} — exit to return",
        program,
        argus_core::paths::contract_tilde(cwd)
    );
    argus_core::text::printable(&said).into_owned()
}

/// The command [`run_suspended`] runs. A login shell is started as a
/// terminal starts one: named `-bash` (for `/opt/homebrew/bin/bash`), which
/// tells any shell to read its login files (`~/.bash_profile`,
/// `~/.zprofile`…). Nothing else changes, `$SHELL` included.
pub fn suspended_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    login_shell: bool,
) -> std::process::Command {
    let mut c = std::process::Command::new(program);
    c.args(args).current_dir(cwd);
    #[cfg(unix)]
    if login_shell {
        use std::os::unix::process::CommandExt;
        let name = Path::new(program)
            .file_name()
            .map_or(program.into(), |n| n.to_string_lossy());
        c.arg0(format!("-{name}"));
    }
    c
}

/// How often the spinner turns while something runs.
const SPIN: Duration = Duration::from_millis(80);

/// How long the loop waits for a key before looking for results again.
/// Nothing wakes it when one arrives, so while background work is out it
/// looks often (a result shows within a frame, not a quarter of a second
/// later); with only a spinner to turn, at the spinner's pace; idle,
/// rarely.
pub fn wait_for_key(working: bool, busy: bool) -> Duration {
    match (working, busy) {
        (true, _) => Duration::from_millis(10),
        (false, true) => SPIN,
        (false, false) => Duration::from_millis(250),
    }
}

/// What background threads panicked with, for the loop to report.
type Panics = Arc<std::sync::Mutex<Vec<String>>>;

/// The terminal set up for the TUI. ratatui has a panic put the terminal
/// back before it's reported, which is right for the thread that draws:
/// the program is ending. A panic in background work ends only that work,
/// so there it's kept for the loop to report (`panics`) and the terminal
/// is left alone, the TUI still being on it.
fn init_terminal(panics: &Panics) -> ratatui::DefaultTerminal {
    let terminal = ratatui::init();
    keep_background_panics(std::thread::current().id(), panics.clone());
    terminal
}

/// Have a panic on any thread but `ui` kept in `panics` instead of going
/// to the panic hook there is now (which, for `ui`, still gets it).
fn keep_background_panics(ui: std::thread::ThreadId, panics: Panics) {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui {
            return hook(info);
        }
        if let Ok(mut kept) = panics.lock() {
            kept.push(info.to_string());
        }
    }));
}

/// So many threads, each taking work as it's free: the rest waits its
/// turn, in a queue, not in a thread of its own (a question for every
/// repo opened at once is a queue of thousands, not thousands of threads).
struct Pool {
    work: Sender<Box<dyn FnOnce() + Send>>,
}

impl Pool {
    /// With `n` threads (one at least), which end when the pool goes.
    fn new(n: usize) -> Self {
        let (work, waiting) = unbounded::<Box<dyn FnOnce() + Send>>();
        for _ in 0..n.max(1) {
            let waiting = waiting.clone();
            std::thread::spawn(move || {
                for job in waiting {
                    // (work that panics takes no thread with it)
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            });
        }
        Pool { work }
    }

    /// Run `job` when a thread is free.
    fn run(&self, job: impl FnOnce() + Send + 'static) {
        let _ = self.work.send(Box::new(job));
    }
}

/// Background work that's out: counted from when it's made until it's
/// dropped, however the work ends (a panic included).
struct Pending(Arc<AtomicUsize>);

impl Pending {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Pending(count.clone())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Runtime {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    opts: Options,
    cancel: Arc<AtomicBool>,
    /// Background work started and not yet finished (its messages sent).
    pending: Arc<AtomicUsize>,
    /// Forge requests out at once: as many as other network work
    /// (`concurrency.network`), however many repos are opened together.
    forge_pool: Option<Pool>,
    /// What background work panicked with, not yet reported.
    panics: Panics,
    /// The forge clients made so far, by kind and host.
    forges: pull::Clients,
}

/// Refresh `repo`'s index ([`argus_core::ops::refresh_index`]), as a
/// step of its own, timed, saying whether it failed and why.
fn refresh_index(repo: &Path) -> Result<(), String> {
    argus_core::trace::timed(
        "refresh index",
        |r: &Result<(), String>| {
            let repo = argus_core::paths::contract_tilde(repo);
            match r {
                Ok(()) => format!("refresh index of {repo}"),
                Err(e) => format!("refresh index of {repo}, failed: {e}"),
            }
        },
        || argus_core::ops::refresh_index(repo),
    )
}

impl Runtime {
    pub fn new(opts: Options) -> Self {
        let (tx, rx) = unbounded::<Msg>();
        Self {
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicUsize::new(0)),
            forge_pool: None,
            panics: Panics::default(),
            forges: Default::default(),
            opts,
        }
    }

    /// With `verbose`, steps taken go to `app`'s history from now on, after
    /// those taken before the TUI started.
    pub fn log_steps(&mut self, app: &mut App) {
        if !self.opts.verbose {
            return;
        }
        for step in std::mem::take(&mut self.opts.early_steps) {
            app.remember(app::Happened::Step(step));
        }
        let tx = self.tx.clone();
        let sink: argus_core::trace::Sink = Arc::new(move |step| {
            let _ = tx.send(Msg::Step(step));
        });
        argus_core::trace::set_sink(Some((argus_core::trace::Form::Whole, sink)));
    }

    /// Handle messages that have arrived (a bounded number, so a flood
    /// can't starve input handling). Returns effects needing the terminal,
    /// and whether any arrived (the screen may have changed).
    pub fn drain(&mut self, app: &mut App) -> (Vec<Effect>, bool) {
        let mut left = vec![];
        let mut received = false;
        // statuses that came in together are applied together: a refresh
        // of every repo is one rebuild, not one each
        let mut statuses = vec![];
        // and so are branches checked together
        let mut branches = vec![];
        for _ in 0..512 {
            let Ok(m) = self.rx.try_recv() else { break };
            received = true;
            let m = match m {
                Msg::Status(path, st) => {
                    statuses.push((path, *st));
                    continue;
                }
                Msg::Branches(path, b) => {
                    branches.push((path, b));
                    continue;
                }
                other => other,
            };
            // (in the order they came: those before another message first)
            app.on_branches(std::mem::take(&mut branches));
            for e in app.on_statuses(std::mem::take(&mut statuses)) {
                left.extend(self.exec(app, e));
            }
            left.extend(self.handle(app, m));
        }
        app.on_branches(branches);
        if !statuses.is_empty() {
            for e in app.on_statuses(statuses) {
                left.extend(self.exec(app, e));
            }
        }
        (left, received)
    }

    /// Handle messages until no background work is left, or `timeout`
    /// passes; returns whether it settled. Effects needing the terminal
    /// are dropped (for tests and headless use).
    /// `m` handled, and what follows run: the effects needing the
    /// terminal returned. The first scan's rows in, startup's over.
    fn handle(&mut self, app: &mut App, m: Msg) -> Vec<Effect> {
        let scanned = matches!(m, Msg::Scanned { .. });
        let mut left = vec![];
        for e in app.on_msg(m) {
            left.extend(self.exec(app, e));
        }
        if scanned && let Some(startup) = self.opts.startup.take() {
            startup.end();
        }
        left
    }

    pub fn settle(&mut self, app: &mut App, timeout: Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        loop {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(m) => {
                    self.handle(app, m);
                }
                Err(_) if self.pending.load(Ordering::SeqCst) == 0 && self.rx.is_empty() => {
                    return true;
                }
                Err(_) => {}
            }
            if until
                .saturating_duration_since(std::time::Instant::now())
                .is_zero()
            {
                return false;
            }
        }
    }

    /// Whether background work is out, its results still to come.
    pub fn working(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }

    /// Have panics off this thread kept for [`Self::take_panics`], as the
    /// loop has them once the terminal is set up.
    pub fn keep_panics(&self) {
        keep_background_panics(std::thread::current().id(), self.panics.clone());
    }

    /// What background work has panicked with since last asked.
    pub fn take_panics(&mut self) -> Vec<String> {
        self.panics
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default()
    }

    /// Whether results have come and not been handled yet: the last one
    /// may arrive just after background work stops counting as out.
    pub fn has_results(&self) -> bool {
        !self.rx.is_empty()
    }

    /// Run `work` on a thread of its own, counted as pending until it ends.
    fn spawn(&self, work: impl FnOnce(Sender<Msg>) + Send + 'static) {
        let (tx, pending) = (self.tx.clone(), Pending::new(&self.pending));
        std::thread::spawn(move || {
            let _pending = pending;
            work(tx);
        });
    }

    /// Carry out `e` and what follows from it. Suspending needs the
    /// terminal: it's returned to the caller.
    pub fn exec(&mut self, app: &mut App, e: Effect) -> Vec<Effect> {
        match e {
            Effect::Quit => app.quit = true,
            Effect::SaveTags(before) => {
                let path = &self.opts.config_path;
                let saved = argus_core::trace::timed(
                    "save",
                    |_| format!("save tags in {}", argus_core::paths::contract_tilde(path)),
                    || configfile::update_tag_lists(path, &before, &app.ws.config),
                );
                if let Err(err) = saved {
                    app.report_error(err.to_string());
                }
            }
            Effect::SaveUi { key, value } => {
                let path = &self.opts.config_path;
                let saved = argus_core::trace::timed(
                    "save",
                    |_| {
                        format!(
                            "save [ui] {key} in {}",
                            argus_core::paths::contract_tilde(path)
                        )
                    },
                    || configfile::set_ui(path, key, &value),
                );
                if let Err(err) = saved {
                    app.report_error(err.to_string());
                }
            }
            Effect::SaveState => {
                let saved = app.ws.save_state(&self.opts.state_path);
                if let Err(err) = saved {
                    app.report_error(err.to_string());
                }
            }
            Effect::Scan => {
                let mut ws = app.ws.clone();
                // (the first, in the startup step)
                let here = argus_core::trace::within();
                self.spawn(move |tx| {
                    ws.entries.clear();
                    here.run(|| ws.scan());
                    let _ = tx.send(Msg::Scanned {
                        entries: ws.entries,
                        truncated: ws.truncated,
                        barren: ws.barren,
                    });
                });
            }
            Effect::Refresh(paths) => {
                let n = app.ws.config.disk_jobs();
                let reader = app.ws.status_reader();
                self.spawn(move |tx| {
                    let what = match &paths[..] {
                        [one] => format!("refresh {}", argus_core::paths::contract_tilde(one)),
                        many => format!("refresh {} repos", many.len()),
                    };
                    argus_core::trace::step(
                        "refresh",
                        || what,
                        || {
                            argus_core::workspace::in_pool(n, || {
                                use rayon::prelude::*;
                                let here = argus_core::trace::within();
                                paths.into_par_iter().for_each(|p| {
                                    let r = here.run(|| reader.read(&p));
                                    let _ = tx.send(Msg::Status(p, Box::new(r)));
                                });
                            })
                        },
                    )
                });
            }
            Effect::CheckBranches(paths) => {
                let n = app.ws.config.disk_jobs();
                let reader = app.ws.status_reader();
                self.spawn(move |tx| {
                    let what = match &paths[..] {
                        [one] => format!(
                            "check branches of {}",
                            argus_core::paths::contract_tilde(one)
                        ),
                        many => format!("check branches of {} repos", many.len()),
                    };
                    argus_core::trace::step(
                        "check branches",
                        || what,
                        || {
                            argus_core::workspace::in_pool(n, || {
                                use rayon::prelude::*;
                                let here = argus_core::trace::within();
                                paths.into_par_iter().for_each(|p| {
                                    let r = here.run(|| reader.branches(&p));
                                    let _ = tx.send(Msg::Branches(p, r));
                                });
                            })
                        },
                    )
                });
            }
            Effect::RefreshIndexes(paths) => {
                let n = app.ws.config.disk_jobs();
                self.spawn(move |_| {
                    let what = match &paths[..] {
                        [one] => format!(
                            "refresh index of {}",
                            argus_core::paths::contract_tilde(one)
                        ),
                        many => format!("refresh indexes of {} repos", many.len()),
                    };
                    argus_core::trace::step(
                        "refresh indexes",
                        || what,
                        || {
                            argus_core::workspace::in_pool(n, || {
                                use rayon::prelude::*;
                                let here = argus_core::trace::within();
                                paths.into_par_iter().for_each(|p| {
                                    // (one that fails says why, and is tried
                                    // again once a later status finds it stale)
                                    let _ = here.run(|| refresh_index(&p));
                                });
                            })
                        },
                    )
                });
            }
            Effect::Jobs(jobs) => {
                let n = app.ws.config.network_jobs();
                let cancel = self.cancel.clone();
                self.spawn(move |tx| {
                    runner::run_jobs(&jobs, n, &cancel, &|ev| {
                        let m = match ev {
                            RunEvent::Started { index } => Msg::JobStarted {
                                path: jobs[index].path.clone(),
                                label: jobs[index].op.label(),
                            },
                            RunEvent::Finished { index, outcome } => Msg::JobFinished {
                                path: jobs[index].path.clone(),
                                label: jobs[index].label.clone(),
                                op: jobs[index].op.clone(),
                                outcome,
                            },
                        };
                        let _ = tx.send(m);
                    });
                });
            }
            Effect::Fix { paths, kinds } => {
                let n = app.ws.config.network_jobs();
                let stash = app.ws.config.stash;
                // each repo's own kinds: less those undoing an ignored state
                let jobs: Vec<(PathBuf, Vec<fix::FixKind>)> = paths
                    .into_iter()
                    .map(|p| {
                        let k = app
                            .ws
                            .get(&p)
                            .map_or_else(|| kinds.clone(), |e| e.fix_kinds(&kinds));
                        (p, k)
                    })
                    .collect();
                self.spawn(move |tx| {
                    argus_core::workspace::in_pool(n, || {
                        use rayon::prelude::*;
                        jobs.into_par_iter().for_each(|(p, kinds)| {
                            let applied =
                                fix::apply_fix(&p, &kinds, &stash).map_err(|e| e.to_string());
                            let _ = tx.send(Msg::FixDone { path: p, applied });
                        });
                    })
                });
            }
            Effect::Forge {
                path,
                kind,
                id,
                branches,
                owners,
            } => {
                // one client per forge, so its token is found once
                let forge = self.forges.get(self.opts.forge_factory, &kind, &id.host);
                let jobs = app.ws.config.network_jobs();
                let pool = self.forge_pool.get_or_insert_with(|| Pool::new(jobs));
                let (tx, pending) = (self.tx.clone(), Pending::new(&self.pending));
                pool.run(move || {
                    let _pending = pending;
                    // (answered whatever happens: asking that panics is
                    // an answer too, not a question left open)
                    struct Answer(Sender<Msg>, Option<(PathBuf, Vec<String>)>);
                    impl Drop for Answer {
                        fn drop(&mut self) {
                            if let Some((path, branches)) = self.1.take() {
                                let info = Err("asking failed unexpectedly (a bug)".to_string());
                                let _ = self.0.send(Msg::Forge {
                                    path,
                                    branches,
                                    info,
                                });
                            }
                        }
                    }
                    let mut answer = Answer(tx, Some((path, branches.clone())));
                    let ask = |f: Arc<dyn argus_core::forge::Forge>| {
                        f.info(&id, &branches, &owners).map_err(|e| e.to_string())
                    };
                    let info = forge.and_then(ask);
                    if let Some((path, branches)) = answer.1.take() {
                        let _ = answer.0.send(Msg::Forge {
                            path,
                            branches,
                            info,
                        });
                    }
                });
            }
            Effect::Describe(path) => self.spawn(move |tx| {
                status::describe_fully(&path);
                let _ = tx.send(Msg::Described(path));
            }),
            Effect::PlanPullAndClone => {
                let mut ws = app.ws.clone();
                let factory = self.opts.forge_factory;
                self.spawn(move |tx| {
                    let started = std::time::Instant::now();
                    let listings = pull::list_tracks(&mut ws, factory);
                    let mut plan = pull::plan_tracks(&ws, &listings.repos, listings.protocol);
                    pull::locate_orphans(&mut plan, factory);
                    argus_core::trace::took(
                        "plan",
                        || "plan pulls and clones of tracked owners' repos".into(),
                        started.elapsed(),
                    );
                    let _ = tx.send(Msg::PullPlanned {
                        tracks: ws.state,
                        plan,
                        errors: listings.errors,
                    });
                });
            }
            // (only a web address: nothing else from a forge is handed
            // to the opener, which takes files and options too)
            Effect::Open(url) if argus_core::text::web_url(&url).is_none() => {
                app.report_error(format!(
                    "not opened: {:?} isn't a web address",
                    argus_core::text::printable(&url)
                ));
            }
            Effect::Open(url) => {
                let opener = &self.opts.opener;
                argus_core::trace::note(|| format!("{opener} {url}"));
                let r = std::process::Command::new(opener)
                    .arg(&url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                if let Err(e) = r {
                    app.report_error(format!("{opener} {url}: {e}"));
                }
            }
            suspend @ Effect::Suspend { .. } => return vec![suspend],
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn results_are_looked_for_often_only_while_work_is_out() {
        use super::wait_for_key;
        let ms = |working, busy| wait_for_key(working, busy).as_millis();
        assert_eq!(ms(true, true), 10);
        assert_eq!(ms(true, false), 10, "a refresh, with nothing spinning");
        assert_eq!(ms(false, true), 80);
        assert_eq!(ms(false, false), 250);
    }

    #[test]
    fn a_pool_runs_so_many_at_once_on_its_own_threads_whatever_is_asked() {
        use std::collections::HashSet;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        // (none asked for is one)
        for (size, threads) in [(1, 1), (3, 3), (0, 1)] {
            let pool = super::Pool::new(size);
            let (inside, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let ran = Arc::new(Mutex::new(HashSet::new()));
            let (done, results) = std::sync::mpsc::channel();
            for n in 0..40 {
                let (inside, most, ran, done) =
                    (inside.clone(), most.clone(), ran.clone(), done.clone());
                pool.run(move || {
                    let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    ran.lock().unwrap().insert(std::thread::current().id());
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    inside.fetch_sub(1, Ordering::SeqCst);
                    // one of them panics: the rest still run
                    assert_ne!(n, 7, "in the work");
                    done.send(n).unwrap();
                });
            }
            drop(done);
            let finished: Vec<i32> = results.iter().collect();
            assert_eq!(finished.len(), 39, "{size}: all but the one that panicked");
            let most = most.load(Ordering::SeqCst);
            assert!(
                most <= threads && (threads == 1 || most > 1),
                "{size}: {most}"
            );
            assert!(ran.lock().unwrap().len() <= threads, "{size}");
        }
    }

    #[test]
    fn a_panic_off_the_drawing_thread_is_kept_and_the_hook_not_run() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        // (the hook there was, standing in for ratatui's)
        let ran = Arc::new(AtomicUsize::new(0));
        let before = std::panic::take_hook();
        let count = ran.clone();
        std::panic::set_hook(Box::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        let panics = super::Panics::default();
        super::keep_background_panics(std::thread::current().id(), panics.clone());
        let off = std::thread::spawn(|| panic!("in the background")).join();
        assert!(off.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 0, "the terminal left alone");
        let kept = panics.lock().unwrap().clone();
        assert!(
            matches!(&kept[..], [one] if one.contains("in the background")),
            "{kept:?}"
        );
        // inverse: on the drawing thread, the hook has it, nothing is kept
        let on = std::panic::catch_unwind(|| panic!("drawing"));
        assert!(on.is_err());
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        assert_eq!(panics.lock().unwrap().len(), 1);
        std::panic::set_hook(before);
    }

    #[test]
    fn the_default_opener_is_the_platforms() {
        let want = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(super::default_opener(), want);
    }
}
