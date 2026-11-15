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
                    println!("{}", suspend_banner(&program, &cwd));
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
    /// The forge clients made so far, by kind and host.
    forges: pull::Clients,
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
            forges: Default::default(),
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
            Effect::SaveMarks(before) => {
                let path = &self.opts.config_path;
                if let Err(err) = configfile::update_mark_lists(path, &before, &app.ws.config) {
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
                let pool = self.forge_pool.get_or_insert_with(|| Pool::new(jobs));
                let (tx, pending) = (self.tx.clone(), Pending::new(&self.pending));
                pool.run(move || {
                    let _pending = pending;
                    let ask = |f: Arc<dyn argus_core::forge::Forge>| {
                        f.info(&id, &branches, &owners).map_err(|e| e.to_string())
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
    fn the_default_opener_is_the_platforms() {
        let want = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(super::default_opener(), want);
    }
}
