//! argus-tui: the interactive front-end, as a library so other binaries can
//! embed it. [`app::App`] holds all logic and asks for effects; [`Runtime`]
//! carries them out (files, background work, opening URLs), and [`run`]
//! wires it to the terminal: drawing, keys, and suspending for a shell or
//! editor.

pub mod app;
pub mod details;
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
use argus_core::runner::{self, Event as RunEvent};
use argus_core::status;
use argus_core::sync::{self, ForgeFactory};
use argus_core::{configfile, fix};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::event::{self, Event, KeyEventKind};

use app::{App, Effect, Msg};

pub struct Options {
    pub state_path: PathBuf,
    /// Where marks (favourite, sync, ignore) are saved.
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
    let (mut theme, warnings) = theme::Theme::build(&ws.config.ui.theme, &ws.config.ui.colors);
    let mut theme_name = ws.config.ui.theme.clone();
    let (mut icons, icon_warnings) = ws.config.ui.icons();
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
    let result = (|| -> Result<(), Error> {
        while !app.quit {
            tick = tick.wrapping_add(1);
            // The View menu can switch theme and icons for the session.
            if app.ws.config.ui.theme != theme_name {
                theme_name = app.ws.config.ui.theme.clone();
                theme = theme::Theme::build(&theme_name, &app.ws.config.ui.colors).0;
            }
            if app.ws.config.ui.icons != icons_name {
                icons_name = app.ws.config.ui.icons.clone();
                icons = app.ws.config.ui.icons().0;
            }
            let view = ui::View {
                theme: &theme,
                icons: icons.clone(),
                tick,
            };
            terminal.draw(|f| ui::draw(f, &mut app, &view))?;
            let busy = app.scanning || !app.running.is_empty();
            let mut left = vec![];
            if event::poll(Duration::from_millis(if busy { 80 } else { 250 }))?
                && let Event::Key(k) = event::read()?
                && k.kind != KeyEventKind::Release
            {
                for e in app.on_key(k) {
                    left.extend(rt.exec(&mut app, e));
                }
            }
            left.extend(rt.drain(&mut app));
            // only suspending is left: it needs the terminal
            while let Some(e) = left.pop() {
                if let Effect::Suspend { program, args, cwd } = e {
                    ratatui::restore();
                    println!(
                        "argus: {} in {} — exit to return",
                        program,
                        argus_core::paths::contract_tilde(&cwd)
                    );
                    let r = run_suspended(&program, &args, &cwd);
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
/// waiting for it. A shell (no `args`) exits with its last command's code,
/// which isn't an error.
pub fn run_suspended(program: &str, args: &[String], cwd: &Path) -> Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| format!("{program}: {e}"))
        .and_then(|s| {
            if s.success() || args.is_empty() {
                Ok(())
            } else {
                Err(format!("{program} exited with {s}"))
            }
        })
}

/// Carries out the app's effects, except suspending the terminal: saving
/// marks and state, opening URLs, and background work (scans, refreshes,
/// jobs, fixes, planning a sync) whose results come back as messages.
pub struct Runtime {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    opts: Options,
    cancel: Arc<AtomicBool>,
    /// Background work started and not yet finished (its messages sent).
    pending: Arc<AtomicUsize>,
}

impl Runtime {
    pub fn new(opts: Options) -> Self {
        let (tx, rx) = unbounded::<Msg>();
        Self {
            tx,
            rx,
            opts,
            cancel: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Handle messages that have arrived (a bounded number, so a flood
    /// can't starve input handling). Returns effects needing the terminal.
    pub fn drain(&mut self, app: &mut App) -> Vec<Effect> {
        let mut left = vec![];
        for _ in 0..512 {
            let Ok(m) = self.rx.try_recv() else { break };
            for e in app.on_msg(m) {
                left.extend(self.exec(app, e));
            }
        }
        left
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
            if std::time::Instant::now() > until {
                return false;
            }
        }
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
                if let Err(err) = app.ws.state.save(&self.opts.state_path) {
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
            Effect::Describe(path) => self.spawn(move |tx| {
                status::describe_fully(&path);
                let _ = tx.send(Msg::Described(path));
            }),
            Effect::PlanSync => {
                let mut ws = app.ws.clone();
                let factory = self.opts.forge_factory;
                self.spawn(move |tx| {
                    let listings = sync::list_tracks(&mut ws, factory);
                    let plan = sync::plan_tracks(&ws, &listings.repos, listings.protocol);
                    let _ = tx.send(Msg::SyncPlanned {
                        tracks: ws.state,
                        plan,
                        errors: listings.errors,
                    });
                });
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
