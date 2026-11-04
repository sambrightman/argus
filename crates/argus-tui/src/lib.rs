//! argus-tui: the interactive front-end, as a library so other binaries can
//! embed it. [`app::App`] holds all logic; this module only wires the
//! terminal, the event loop and background workers together.

pub mod app;
pub mod details;
pub mod input;
pub mod menu;
pub mod theme;
pub mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use argus_core::Workspace;
use argus_core::fix;
use argus_core::runner::{self, Event as RunEvent};
use argus_core::status::{self, StatusOptions};
use argus_core::sync::{self, ForgeFactory};
use crossbeam_channel::{Receiver, Sender, unbounded};
use crossterm::event::{self, Event, KeyEventKind};

use app::{App, Effect, Msg};

pub struct Options {
    pub state_path: PathBuf,
    pub forge_factory: ForgeFactory<'static>,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn run(ws: Workspace, opts: Options) -> Result<(), Error> {
    let (theme, warnings) = theme::Theme::build(&ws.config.ui.theme, &ws.config.ui.colors);
    let icons = argus_core::badges::by_name(&ws.config.ui.icons);
    let mut app = App::new(ws);
    if let Some(w) = warnings.first() {
        app.on_msg(Msg::Suspended(Err(format!("config: {w}"))));
    }
    let (tx, rx) = unbounded::<Msg>();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut rt = Runtime {
        tx,
        opts,
        cancel: cancel.clone(),
    };

    let mut terminal = ratatui::init();
    app.scanning = true;
    rt.exec(&mut app, &mut terminal, Effect::Scan);
    let mut tick = 0usize;
    let result = (|| -> Result<(), Error> {
        while !app.quit {
            tick = tick.wrapping_add(1);
            let view = ui::View {
                theme: &theme,
                icons,
                tick,
            };
            terminal.draw(|f| ui::draw(f, &mut app, &view))?;
            let busy = app.scanning || !app.running.is_empty();
            if event::poll(Duration::from_millis(if busy { 80 } else { 250 }))?
                && let Event::Key(k) = event::read()?
                && k.kind != KeyEventKind::Release
            {
                for e in app.on_key(k) {
                    rt.exec(&mut app, &mut terminal, e);
                }
            }
            drain(&rx, &mut app, &mut rt, &mut terminal);
        }
        Ok(())
    })();
    cancel.store(true, Ordering::Relaxed);
    ratatui::restore();
    result
}

fn drain(rx: &Receiver<Msg>, app: &mut App, rt: &mut Runtime, term: &mut ratatui::DefaultTerminal) {
    // Bounded so a flood of messages can't starve input handling.
    for _ in 0..512 {
        let Ok(m) = rx.try_recv() else { break };
        for e in app.on_msg(m) {
            rt.exec(app, term, e);
        }
    }
}

struct Runtime {
    tx: Sender<Msg>,
    opts: Options,
    cancel: Arc<AtomicBool>,
}

impl Runtime {
    fn exec(&mut self, app: &mut App, term: &mut ratatui::DefaultTerminal, e: Effect) {
        let tx = self.tx.clone();
        match e {
            Effect::Quit => app.quit = true,
            Effect::SaveState => {
                if let Err(err) = app.ws.state.save(&self.opts.state_path) {
                    app.on_msg(Msg::Suspended(Err(err.to_string())));
                }
            }
            Effect::Scan => {
                let mut ws = app.ws.clone();
                std::thread::spawn(move || {
                    ws.entries.clear();
                    ws.scan();
                    let _ = tx.send(Msg::Scanned(ws.entries));
                });
            }
            Effect::Refresh(paths) => {
                std::thread::spawn(move || {
                    use rayon::prelude::*;
                    paths.into_par_iter().for_each(|p| {
                        let r =
                            status::status(&p, StatusOptions::default()).map_err(|e| e.to_string());
                        let _ = tx.send(Msg::Status(p, r));
                    });
                });
            }
            Effect::Jobs(jobs) => {
                let n = app.ws.config.concurrency;
                let cancel = self.cancel.clone();
                std::thread::spawn(move || {
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
                let n = app.ws.config.concurrency;
                std::thread::spawn(move || {
                    let pool = rayon::ThreadPoolBuilder::new().num_threads(n).build();
                    let work = || {
                        use rayon::prelude::*;
                        paths.into_par_iter().for_each(|p| {
                            let applied =
                                fix::apply_fixes(&p, &kinds, 8).map_err(|e| e.to_string());
                            let _ = tx.send(Msg::FixDone { path: p, applied });
                        });
                    };
                    match pool {
                        Ok(pool) => pool.install(work),
                        Err(_) => work(),
                    }
                });
            }
            Effect::PlanSync => {
                let mut ws = app.ws.clone();
                let factory = self.opts.forge_factory;
                std::thread::spawn(move || {
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
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                let r = std::process::Command::new(opener)
                    .arg(&url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                if let Err(e) = r {
                    app.on_msg(Msg::Suspended(Err(format!("{opener} {url}: {e}"))));
                }
            }
            Effect::Suspend { program, args, cwd } => {
                ratatui::restore();
                println!(
                    "argus: {} in {} — exit to return",
                    program,
                    argus_core::paths::contract_tilde(&cwd)
                );
                let r = std::process::Command::new(&program)
                    .args(&args)
                    .current_dir(&cwd)
                    .status()
                    .map_err(|e| format!("{program}: {e}"))
                    .and_then(|s| {
                        // A shell's exit code reflects the last command run in it: not an error.
                        if s.success() || args.is_empty() {
                            Ok(())
                        } else {
                            Err(format!("{program} exited with {s}"))
                        }
                    });
                *term = ratatui::init();
                let _ = term.clear();
                for e in app.on_msg(Msg::Suspended(r)) {
                    self.exec(app, term, e);
                }
            }
        }
    }
}
