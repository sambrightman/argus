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
    /// Where marks (favourite, pull, ignore) are saved.
    pub config_path: PathBuf,
    pub forge_factory: ForgeFactory<'static>,
    /// Startup notices (e.g. the current-directory fallback), shown after scans.
    pub notices: Vec<String>,
    /// The program opening URLs (see [`default_opener`]).
    pub opener: String,
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
    check_terminal()?;
    let (mut theme, mut warnings) = theme::Theme::for_config(&ws.config);
    warnings.extend(menu::mark_key_clashes(&ws.config));
    let mut theme_name = ws.config.ui.theme.clone();
    let (mut icons, icon_warnings) = ws.config.icons();
    let mut icons_name = ws.config.ui.icons.clone();
    let mut app = App::new(ws);
    app.notices = opts.notices.clone();
    if let Some(w) = warnings.iter().chain(&icon_warnings).next() {
        app.report_error(format!("config: {w}"));
    }
    let mut rt = Runtime::new(opts);

    let mut terminal = ratatui::init();
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
            let busy = app.scanning || !app.running.is_empty();
            // (the spinner turns at its own pace however often results
            // are looked for)
            if dirty || (busy && drawn.elapsed() >= SPIN) {
                tick = tick.wrapping_add(1);
                let mut links = vec![];
                terminal.draw(|f| links = ui::draw(f, &mut app, &view))?;
                ui::write_links(terminal.backend_mut(), &links)?;
                drawn = std::time::Instant::now();
            }
            dirty = false;
            let mut left = vec![];
            if event::poll(wait_for_key(rt.working(), busy))? {
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
                    println!(
                        "argus: {} in {} — exit to return",
                        program,
                        argus_core::paths::contract_tilde(&cwd)
                    );
                    let r = run_suspended(&program, &args, &cwd, login_shell);
                    dirty = true;
                    terminal = ratatui::init();
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

/// Lets so many through at once: the rest wait their turn.
struct Gate {
    free: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
}

impl Gate {
    fn new(n: usize) -> Self {
        Gate {
            free: std::sync::Mutex::new(n.max(1)),
            freed: std::sync::Condvar::new(),
        }
    }

    /// Run `work` once there's room.
    fn run<R>(&self, work: impl FnOnce() -> R) -> R {
        {
            let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
            while *free == 0 {
                free = self.freed.wait(free).unwrap_or_else(|e| e.into_inner());
            }
            *free -= 1;
        }
        let r = work();
        *self.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.freed.notify_one();
        r
    }
}

/// Carries out the app's effects, except suspending the terminal: saving
/// marks and state, opening URLs, and background work (scans, refreshes,
/// jobs, fixes, planning pulls and clones) whose results come back as messages.
pub struct Runtime {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    opts: Options,
    cancel: Arc<AtomicBool>,
    /// Background work started and not yet finished (its messages sent).
    pending: Arc<AtomicUsize>,
    /// Forge requests out at once: as many as other network work
    /// (`concurrency.network`), however many repos are opened together.
    forge_gate: Option<Arc<Gate>>,
    /// The forge clients made so far, by kind and host.
    forges: pull::Clients,
    /// The state file as last read or written here: what has changed since
    /// is what a save adds to the file (see [`State::save_merged`]).
    ///
    /// [`State::save_merged`]: argus_core::State::save_merged
    state_base: argus_core::State,
}

impl Runtime {
    pub fn new(opts: Options) -> Self {
        let (tx, rx) = unbounded::<Msg>();
        Self {
            tx,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicUsize::new(0)),
            forge_gate: None,
            forges: Default::default(),
            state_base: argus_core::State::load(&opts.state_path).unwrap_or_default(),
            opts,
        }
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
        for _ in 0..512 {
            let Ok(m) = self.rx.try_recv() else { break };
            received = true;
            let m = match m {
                Msg::Status(path, st) => {
                    statuses.push((path, *st));
                    continue;
                }
                other => other,
            };
            // (in the order they came: those before another message first)
            for e in app.on_statuses(std::mem::take(&mut statuses)) {
                left.extend(self.exec(app, e));
            }
            for e in app.on_msg(m) {
                left.extend(self.exec(app, e));
            }
        }
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
    pub fn settle(&mut self, app: &mut App, timeout: Duration) -> bool {
        let until = std::time::Instant::now() + timeout;
        loop {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(m) => {
                    for e in app.on_msg(m) {
                        self.exec(app, e);
                    }
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

    /// Run `work` on a thread, counted as pending until it returns.
    fn spawn(&self, work: impl FnOnce(Sender<Msg>) + Send + 'static) {
        let (tx, pending) = (self.tx.clone(), self.pending.clone());
        pending.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            work(tx);
            pending.fetch_sub(1, Ordering::SeqCst);
        });
    }

    /// Carry out `e` and what follows from it. Suspending needs the
    /// terminal: it's returned to the caller.
    pub fn exec(&mut self, app: &mut App, e: Effect) -> Vec<Effect> {
        match e {
            Effect::Quit => app.quit = true,
            Effect::SaveMarks(before) => {
                let path = &self.opts.config_path;
                if let Err(err) = configfile::update_mark_lists(path, &before, &app.ws.config) {
                    app.report_error(err.to_string());
                }
            }
            Effect::SaveState => {
                let saved = app
                    .ws
                    .state
                    .save_merged(&self.opts.state_path, &mut self.state_base);
                if let Err(err) = saved {
                    app.report_error(err.to_string());
                }
            }
            Effect::Scan => {
                let mut ws = app.ws.clone();
                self.spawn(move |tx| {
                    ws.entries.clear();
                    ws.scan();
                    let _ = tx.send(Msg::Scanned {
                        entries: ws.entries,
                        truncated: ws.truncated,
                    });
                });
            }
            Effect::Refresh(paths) => {
                let n = app.ws.config.disk_jobs();
                let reader = app.ws.status_reader();
                self.spawn(move |tx| {
                    argus_core::workspace::in_pool(n, || {
                        use rayon::prelude::*;
                        paths.into_par_iter().for_each(|p| {
                            let r = reader.read(&p);
                            let _ = tx.send(Msg::Status(p, Box::new(r)));
                        });
                    })
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
                let gate = self
                    .forge_gate
                    .get_or_insert_with(|| Arc::new(Gate::new(jobs)))
                    .clone();
                self.spawn(move |tx| {
                    let ask = |f: Arc<dyn argus_core::forge::Forge>| {
                        gate.run(|| f.info(&id, &branches, &owners))
                            .map_err(|e| e.to_string())
                    };
                    let info = forge.and_then(ask);
                    let _ = tx.send(Msg::Forge {
                        path,
                        branches,
                        info,
                    });
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
                    let listings = pull::list_tracks(&mut ws, factory);
                    let mut plan = pull::plan_tracks(&ws, &listings.repos, listings.protocol);
                    pull::locate_orphans(&mut plan, factory);
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
    fn a_gate_lets_only_so_many_through_at_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for size in [1, 3] {
            let gate = Arc::new(super::Gate::new(size));
            let (inside, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let threads: Vec<_> = (0..12)
                .map(|_| {
                    let (gate, inside, most) = (gate.clone(), inside.clone(), most.clone());
                    std::thread::spawn(move || {
                        gate.run(|| {
                            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                            most.fetch_max(now, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            inside.fetch_sub(1, Ordering::SeqCst);
                        })
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            // all ran, never more than the size at once, and (with room
            // for several) more than one
            assert_eq!(inside.load(Ordering::SeqCst), 0);
            let most = most.load(Ordering::SeqCst);
            assert!(most <= size && (size == 1 || most > 1), "{size}: {most}");
        }
        // none is no gate at all: one
        assert_eq!(*super::Gate::new(0).free.lock().unwrap(), 1);
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
