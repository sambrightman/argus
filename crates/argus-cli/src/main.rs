//! `argus`: the command-line front-end. With no subcommand it starts the TUI.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use argus_core::badges::{self, Badge, Icons, Role};
use argus_core::fix::{self, FixKind};
use argus_core::runner::{self, Event};
use argus_core::sync;
use argus_core::{Config, State, ViewFilter, Workspace, configfile, paths};
use clap::{Args, Parser, Subcommand, ValueEnum};
use unicode_width::UnicodeWidthStr;

#[derive(Parser)]
#[command(
    name = "argus",
    version,
    about = "Overview, clean up and sync many git clones"
)]
struct Cli {
    /// Config file [default: $XDG_CONFIG_HOME/argus/config.toml]
    #[arg(long, global = true, env = "ARGUS_CONFIG")]
    config: Option<PathBuf>,
    /// State file [default: $XDG_STATE_HOME/argus/state.toml]
    #[arg(long, global = true, env = "ARGUS_STATE")]
    state: Option<PathBuf>,
    /// Scan this root instead of the configured ones (repeatable).
    #[arg(long = "root", global = true, value_name = "DIR")]
    roots: Vec<PathBuf>,
    /// Scan this many directory levels below each root instead of
    /// `max_depth` (0: no limit).
    #[arg(long, global = true, value_name = "N")]
    max_depth: Option<usize>,
    /// Follow symlinks to directories (cycles are cut; a repo reached by
    /// several paths is listed once).
    #[arg(long, short = 'L', global = true)]
    follow_symlinks: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive TUI (default).
    Tui,
    /// Print repositories needing attention (`--all`: clean ones too).
    Status {
        #[command(flatten)]
        view: View,
        /// Print JSON instead of text (same filter).
        #[arg(long)]
        json: bool,
    },
    /// Pull syncable repos; list tracked owners and clone new repos.
    Sync {
        /// Show what would happen.
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Don't query forges or clone; only pull.
        #[arg(long)]
        no_forge: bool,
        /// Only these repos (glob on name/key/path).
        patterns: Vec<String>,
    },
    /// Apply safe fixes (default: those in `[fix] auto`).
    Fix {
        /// Fix kinds to apply instead of `[fix] auto`; comma-separated,
        /// repeatable.
        #[arg(long = "kind", short = 'k', value_enum, value_delimiter = ',')]
        kinds: Vec<FixArg>,
        /// Show what would be fixed.
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Fetch the selected repos first, so "behind" and conflict
        /// predictions are current (with --dry-run too).
        #[arg(long, short = 'F')]
        fetch: bool,
        /// Repos to fix (glob on name/key/path); default all visible.
        patterns: Vec<String>,
    },
    /// Set favourite / sync / exclude marks, or snooze repos.
    Mark(MarkArgs),
    /// Acknowledge "new" repos (all, or matching patterns).
    Ack {
        /// Repos (glob on name/key/path).
        patterns: Vec<String>,
    },
    /// Forge queries.
    Forge {
        #[command(subcommand)]
        cmd: ForgeCmd,
    },
    /// Manage the directories scanned for repositories.
    Root {
        #[command(subcommand)]
        cmd: RootCmd,
    },
    /// Print config and state file locations.
    Paths,
    /// Open the TUI on sample repos in every state (deleted on quit), to see
    /// the whole UI and try themes and icons (`v t`, `v i`; `?` legend).
    Demo {
        /// Theme to start with [default: `[ui] theme`].
        #[arg(long, value_name = "NAME")]
        theme: Option<String>,
        /// Icon set to start with: unicode, nerd or ascii [default: `[ui] icons`].
        #[arg(long, value_name = "SET")]
        icons: Option<String>,
    },
}

#[derive(Args, Clone, Copy)]
struct View {
    /// Include clean repos.
    #[arg(long, short = 'a')]
    all: bool,
    /// Include excluded and snoozed repos.
    #[arg(long)]
    hidden: bool,
}

impl From<View> for ViewFilter {
    fn from(v: View) -> Self {
        ViewFilter {
            attention_only: !v.all,
            show_hidden: v.hidden,
        }
    }
}

#[derive(Args)]
struct MarkArgs {
    /// Repos (glob on name/key/path).
    #[arg(required = true)]
    patterns: Vec<String>,
    /// Sort above other repos in the same group.
    #[arg(long, overrides_with = "no_favourite")]
    favourite: bool,
    /// Not a favourite, whatever the config rules say.
    #[arg(long)]
    no_favourite: bool,
    /// Pull on sync.
    #[arg(long, overrides_with = "no_sync")]
    sync: bool,
    /// Don't pull on sync, whatever the config rules say.
    #[arg(long)]
    no_sync: bool,
    /// Hide and skip in bulk operations.
    #[arg(long, overrides_with = "include")]
    exclude: bool,
    /// Not excluded, whatever the config rules say.
    #[arg(long)]
    include: bool,
    /// Hide temporarily, e.g. `3d`, `12h`, `2w`.
    #[arg(long, value_name = "DURATION")]
    snooze: Option<String>,
    /// End a snooze early.
    #[arg(long)]
    unsnooze: bool,
    /// Remove all marks (fall back to config rules).
    #[arg(long)]
    clear: bool,
}

#[derive(Subcommand)]
enum RootCmd {
    /// Add directories (default: the current directory).
    Add {
        /// Existing directories; stored as `~/…` where possible.
        dirs: Vec<PathBuf>,
    },
    /// Remove directories.
    Remove {
        /// Configured roots (they need not exist any more).
        #[arg(required = true)]
        dirs: Vec<PathBuf>,
    },
    /// List configured roots, flagging any that no longer exist.
    List,
}

#[derive(Subcommand)]
enum ForgeCmd {
    /// List an org's or user's repositories.
    List {
        /// Org or user.
        owner: String,
        /// Only this org team's repositories (team slug).
        #[arg(long)]
        team: Option<String>,
        /// Forge host (GitHub Enterprise: your host).
        #[arg(long, default_value = "github.com")]
        host: String,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FixArg {
    SwitchDefault,
    Push,
    Publish,
    FastForward,
    Pull,
    Stash,
}

impl From<FixArg> for FixKind {
    fn from(f: FixArg) -> Self {
        match f {
            FixArg::SwitchDefault => FixKind::SwitchDefault,
            FixArg::Push => FixKind::Push,
            FixArg::Publish => FixKind::Publish,
            FixArg::FastForward => FixKind::FastForward,
            FixArg::Pull => FixKind::Pull,
            FixArg::Stash => FixKind::Stash,
        }
    }
}

struct Ctx {
    config_path: PathBuf,
    state_path: PathBuf,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("argus: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx {
        config_path: cli.config.clone().unwrap_or_else(paths::config_file),
        state_path: cli.state.clone().unwrap_or_else(paths::state_file),
    };
    if !matches!(cli.cmd, Some(Cmd::Paths)) && configfile::ensure_exists(&ctx.config_path)? {
        eprintln!(
            "argus: created {} (every setting listed with its default, commented out)",
            paths::contract_tilde(&ctx.config_path)
        );
    }
    match cli.cmd {
        Some(Cmd::Root { cmd }) => return root_cmd(&ctx, cmd),
        Some(Cmd::Paths) => {
            println!("config: {}", ctx.config_path.display());
            println!("state:  {}", ctx.state_path.display());
            return Ok(());
        }
        Some(Cmd::Forge { cmd }) => return forge_cmd(cmd),
        Some(Cmd::Demo { theme, icons }) => {
            argus_tui::check_terminal().map_err(|e| anyhow::anyhow!(e))?;
            return demo(&ctx, theme, icons);
        }
        None | Some(Cmd::Tui) => argus_tui::check_terminal().map_err(|e| anyhow::anyhow!(e))?,
        _ => {}
    }
    let (mut ws, notices) = load(&ctx, &cli.roots, cli.max_depth)?;
    if cli.follow_symlinks {
        ws.config.follow_symlinks = true;
    }
    let tui = matches!(cli.cmd, None | Some(Cmd::Tui));
    if !tui {
        for n in &notices {
            eprintln!("argus: {n}");
        }
    }
    match cli.cmd.unwrap_or(Cmd::Tui) {
        Cmd::Tui => {
            let opts = argus_tui::Options {
                state_path: ctx.state_path.clone(),
                forge_factory: forge_factory(),
                notices,
            };
            argus_tui::run(ws, opts).map_err(|e| anyhow::anyhow!(e))?;
        }
        Cmd::Status { view, json } => {
            scan(&mut ws);
            status(&ws, view.into(), json)?;
        }
        Cmd::Sync {
            dry_run,
            no_forge,
            patterns,
        } => {
            scan(&mut ws);
            sync_cmd(&mut ws, &ctx, dry_run, no_forge, &patterns)?;
        }
        Cmd::Fix {
            kinds,
            dry_run,
            fetch,
            patterns,
        } => {
            scan(&mut ws);
            let kinds: Vec<FixKind> = if kinds.is_empty() {
                ws.config.fix.auto.clone()
            } else {
                kinds.into_iter().map(Into::into).collect()
            };
            if fetch {
                fetch_selected(&mut ws, &patterns);
            }
            fix_cmd(&mut ws, &kinds, dry_run, &patterns)?;
        }
        Cmd::Mark(m) => {
            scan(&mut ws);
            mark(&mut ws, &m)?;
            save_state(&ws, &ctx)?;
        }
        Cmd::Ack { patterns } => {
            scan(&mut ws);
            let sel = ws.select(
                &patterns,
                ViewFilter {
                    show_hidden: true,
                    ..Default::default()
                },
            );
            let keys: Vec<String> = sel.iter().map(|&i| ws.entries[i].key.clone()).collect();
            let before = ws.state.new.len();
            ws.state.new.retain(|k| !keys.contains(k));
            println!("acknowledged {}", before - ws.state.new.len());
            save_state(&ws, &ctx)?;
        }
        Cmd::Root { .. } | Cmd::Paths | Cmd::Forge { .. } | Cmd::Demo { .. } => unreachable!(),
    }
    Ok(())
}

/// Load config and state. With no roots configured (and none given), the
/// current directory is scanned; the returned notices say so.
fn load(
    ctx: &Ctx,
    roots: &[PathBuf],
    max_depth: Option<usize>,
) -> Result<(Workspace, Vec<String>)> {
    let mut config = Config::load(&ctx.config_path)?;
    let mut notices = vec![];
    if let Some(d) = max_depth {
        config.max_depth = d;
    }
    if !roots.is_empty() {
        config.roots = roots.iter().map(|r| paths::expand_tilde(r)).collect();
    }
    if config.roots.is_empty() {
        let cwd = std::env::current_dir().context("current directory")?;
        notices.push(
            "no roots configured; scanning the current directory (save it with `argus root add`)"
                .to_string(),
        );
        config.roots = vec![cwd];
    }
    let mut state = State::load(&ctx.state_path)?;
    state.expire_snoozes(jiff::Timestamp::now());
    Ok((Workspace::new(config, state)?, notices))
}

/// Scan, warning about directories skipped because of `max_depth`.
fn scan(ws: &mut Workspace) {
    ws.scan();
    if let Some(w) = ws.truncation_warning() {
        eprintln!("argus: {w}");
    }
}

fn save_state(ws: &Workspace, ctx: &Ctx) -> Result<()> {
    ws.state.save(&ctx.state_path).context("saving state")
}

fn root_cmd(ctx: &Ctx, cmd: RootCmd) -> Result<()> {
    let path = &ctx.config_path;
    match cmd {
        RootCmd::Add { dirs } => {
            let dirs = if dirs.is_empty() {
                vec![std::env::current_dir()?]
            } else {
                dirs
            };
            let added = configfile::add_roots(path, &dirs)?;
            if added.is_empty() {
                println!("already configured");
            }
            for a in added {
                println!("added {a}");
            }
        }
        RootCmd::Remove { dirs } => {
            for r in configfile::remove_roots(path, &dirs)? {
                println!("removed {r}");
            }
        }
        RootCmd::List => {
            let config = Config::load(path)?;
            if config.roots.is_empty() {
                println!("no roots configured (the current directory is scanned)");
            }
            for r in &config.roots {
                let missing = if r.is_dir() { "" } else { "  (missing)" };
                println!("{}{missing}", paths::contract_tilde(r));
            }
        }
    }
    Ok(())
}

fn forge_factory() -> sync::ForgeFactory<'static> {
    &|kind, host| argus_forge::for_track(kind, host)
}

// ---------- output ----------

struct Paint {
    color: bool,
}

impl Paint {
    fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn role(&self, role: Role, s: &str) -> String {
        if !self.color {
            return s.to_string();
        }
        let code = match role {
            Role::Favourite => "33",
            Role::New => "1;36",
            Role::Hidden | Role::Dim => "2",
            Role::Branch => "35",
            Role::DefaultBranch => "2;35",
            Role::Detached => "1;35",
            Role::Ahead => "32",
            Role::Behind => "36",
            Role::Staged => "32",
            Role::Unstaged => "33",
            Role::Untracked => "2",
            Role::Conflict | Role::Error => "1;31",
            Role::Stash => "34",
            Role::Warning => "31",
        };
        format!("\x1b[{code}m{s}\x1b[0m")
    }

    fn badges(&self, b: &[Badge]) -> String {
        b.iter()
            .map(|b| self.role(b.role, &b.text))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn pad(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

fn icons(ws: &Workspace) -> Icons {
    badges::by_name(&ws.config.ui.icons)
}

fn status(ws: &Workspace, filter: ViewFilter, json: bool) -> Result<()> {
    let order = ws.ordered(filter);
    if json {
        let v: Vec<_> = order.iter().map(|&i| &ws.entries[i]).collect();
        serde_json::to_writer_pretty(std::io::stdout().lock(), &v)?;
        println!();
        return Ok(());
    }
    let p = Paint::detect();
    let ic = icons(ws);
    let rows: Vec<(String, String, String, String)> = order
        .iter()
        .map(|&i| {
            let e = &ws.entries[i];
            let flags = badges::flags(e, &ic);
            let flag_w: usize = flags
                .iter()
                .map(|b| UnicodeWidthStr::width(b.text.as_str()))
                .sum();
            let indent = "  ".repeat(ws.depth(i));
            let name = format!("{indent}{}", e.display_name());
            let br = badges::branch(e, &ic)
                .map(|b| p.role(b.role, &b.text))
                .unwrap_or_default();
            let br_plain = badges::branch(e, &ic).map(|b| b.text).unwrap_or_default();
            (
                format!(
                    "{}{}",
                    " ".repeat(2usize.saturating_sub(flag_w)),
                    p.badges(&flags).replace(' ', "")
                ),
                name,
                format!("{br}\u{0}{br_plain}"),
                p.badges(&badges::state(e, &ic)),
            )
        })
        .collect();
    let name_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.1.as_str()))
        .max()
        .unwrap_or(0);
    let br_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.2.split('\0').nth(1).unwrap_or("")))
        .max()
        .unwrap_or(0);
    let mut out = std::io::stdout().lock();
    for (flags, name, br, state) in rows {
        let (br_col, br_plain) = br.split_once('\0').unwrap();
        let br_pad = " ".repeat(br_w.saturating_sub(UnicodeWidthStr::width(br_plain)));
        writeln!(
            out,
            "{flags} {}  {br_col}{br_pad}  {state}",
            pad(&name, name_w)
        )?;
    }
    writeln!(out, "{}", p.role(Role::Dim, &status_summary(ws, filter)))?;
    Ok(())
}

/// Footer counts, naming the flag that shows whatever the filter left out.
fn status_summary(ws: &Workspace, filter: ViewFilter) -> String {
    let shown = |e: &&argus_core::Entry| filter.show_hidden || !e.flags.hidden();
    let total = ws.entries.iter().filter(shown).count();
    let attention = ws
        .entries
        .iter()
        .filter(shown)
        .filter(|e| e.needs_attention())
        .count();
    let clean = total - attention;
    let mut s = if filter.attention_only && clean > 0 {
        format!("{attention} of {total} repos need attention; {clean} clean not shown (--all)")
    } else {
        format!("{total} repos, {attention} need attention")
    };
    let hidden = ws.entries.len() - total;
    if hidden > 0 {
        s += &format!("; {hidden} hidden (--hidden)");
    }
    s
}

// ---------- sync ----------

fn sync_cmd(
    ws: &mut Workspace,
    ctx: &Ctx,
    dry_run: bool,
    no_forge: bool,
    patterns: &[String],
) -> Result<()> {
    let p = Paint::detect();
    let mut jobs = vec![];
    let mut tracks = sync::SyncPlan::default();
    if !no_forge && !ws.config.tracks.is_empty() {
        let listings = sync::list_tracks(ws, forge_factory());
        for (k, e) in &listings.errors {
            eprintln!("{} {k}: {e}", p.role(Role::Error, "forge error"));
        }
        tracks = sync::plan_tracks(ws, &listings.repos, listings.protocol);
        jobs.extend(tracks.jobs.iter().cloned());
    }
    let mut updates = sync::plan_updates(ws);
    if !patterns.is_empty() {
        let sel: Vec<PathBuf> = ws
            .select(patterns, ViewFilter::default())
            .into_iter()
            .map(|i| ws.entries[i].path.clone())
            .collect();
        updates.retain(|j| sel.contains(&j.path));
    }
    jobs.extend(updates);

    for r in &tracks.missing {
        println!(
            "{} {} (not cloned)",
            p.role(Role::New, "new on forge:"),
            r.id.slug()
        );
    }
    for o in &tracks.orphans {
        println!(
            "{} {} (no longer listed by the forge)",
            p.role(Role::Warning, "orphan:"),
            paths::contract_tilde(o)
        );
    }
    if dry_run {
        for j in &jobs {
            println!("would {}", j.label);
        }
        return Ok(());
    }
    let total = jobs.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let tty = std::io::stderr().is_terminal();
    let results = runner::run_jobs(
        &jobs,
        ws.config.network_jobs(),
        &AtomicBool::new(false),
        &|ev| {
            if let Event::Finished { index, outcome } = ev {
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if !outcome.success {
                    eprintln!(
                        "{} {}\n{}",
                        p.role(Role::Error, "failed:"),
                        jobs[index].label,
                        indent(&outcome.output)
                    );
                } else if tty {
                    eprint!("\r\x1b[K[{n}/{total}] {}", jobs[index].label);
                }
            }
        },
    );
    if tty {
        eprint!("\r\x1b[K");
    }
    let added = sync::record_clones(ws, &jobs, &results);
    for a in &added {
        println!(
            "{} {}",
            p.role(Role::New, "cloned:"),
            paths::contract_tilde(a)
        );
    }
    let failed = results
        .iter()
        .filter(|r| r.as_ref().is_some_and(|o| !o.success))
        .count();
    let skipped = results.iter().filter(|r| r.is_none()).count();
    println!(
        "{total} jobs: {} ok, {failed} failed, {skipped} skipped",
        total - failed - skipped
    );
    ws.state.last_sync = Some(jiff::Timestamp::now());
    save_state(ws, ctx)?;
    if failed > 0 {
        std::process::exit(2);
    }
    Ok(())
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------- demo ----------

fn demo(ctx: &Ctx, theme: Option<String>, icons: Option<String>) -> Result<()> {
    eprintln!("argus: building sample repos…");
    let d = argus_core::demo::build();
    // Only the user's look carries over; everything else is the demo's own.
    let mut config = Config {
        roots: vec![d.root.clone()],
        ..Config::default()
    };
    if let Ok(c) = Config::load(&ctx.config_path) {
        config.ui = c.ui;
    }
    config.ui.attention_only = false;
    if let Some(t) = theme {
        config.ui.theme = t;
    }
    if let Some(i) = icons {
        config.ui.icons = i;
    }
    let mut ws = Workspace::new(config, State::default())?;
    ws.scan();
    d.apply_marks(&mut ws);
    let state_path = d.sandbox.path("state.toml");
    ws.state.save(&state_path)?;
    let opts = argus_tui::Options {
        state_path,
        forge_factory: forge_factory(),
        notices: vec![format!(
            "demo repos in {} (deleted on quit) · v t: theme · v i: icons · ?: legend",
            d.root.display()
        )],
    };
    argus_tui::run(ws, opts).map_err(|e| anyhow::anyhow!(e))
}

// ---------- fix ----------

/// Fetch the repos `patterns` select and refresh their status. Failures are
/// reported; planning continues with what is known.
fn fetch_selected(ws: &mut Workspace, patterns: &[String]) {
    let p = Paint::detect();
    let sel = ws.select(patterns, ViewFilter::default());
    let jobs = sync::plan_fetches(ws, &sel);
    let tty = std::io::stderr().is_terminal();
    let done = std::sync::atomic::AtomicUsize::new(0);
    runner::run_jobs(
        &jobs,
        ws.config.network_jobs(),
        &AtomicBool::new(false),
        &|ev| {
            if let Event::Finished { index, outcome } = ev {
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if !outcome.success {
                    eprintln!(
                        "{} {}\n{}",
                        p.role(Role::Error, "fetch failed:"),
                        jobs[index].label.trim_start_matches("fetch "),
                        indent(&outcome.output)
                    );
                } else if tty {
                    eprint!("\r\x1b[K[{n}/{}] {}", jobs.len(), jobs[index].label);
                }
            }
        },
    );
    if tty {
        eprint!("\r\x1b[K");
    }
    for i in sel {
        let path = ws.entries[i].path.clone();
        ws.refresh(&path);
    }
}

fn fix_cmd(
    ws: &mut Workspace,
    kinds: &[FixKind],
    dry_run: bool,
    patterns: &[String],
) -> Result<()> {
    let p = Paint::detect();
    let sel = ws.select(patterns, ViewFilter::default());
    let mut any = false;
    for i in sel {
        let e = &ws.entries[i];
        let Some(s) = e.status() else { continue };
        let name = e.display_name();
        let path = e.path.clone();
        if dry_run {
            // The fix this run would apply, the chain later runs would
            // continue with, then other fixes available now.
            let fix::Preview {
                chain,
                alternatives,
            } = fix::preview(s, kinds);
            if chain.is_empty() && alternatives.is_empty() {
                continue;
            }
            any = true;
            match chain.first() {
                Some(f) => println!("{name}: would {} ({})", f.op.label(), f.reason),
                None => println!("{name}: nothing automatic"),
            }
            for f in chain.iter().skip(1) {
                println!("  then: {} ({})", f.op.label(), f.reason);
            }
            for f in alternatives {
                let hint = if let Some(why) = &f.blocked {
                    format!("; not automatic: {why}")
                } else if kinds.contains(&f.kind) {
                    String::new()
                } else {
                    format!(", with -k {}", f.kind.name())
                };
                println!("  alternative: {} ({}){hint}", f.op.label(), f.reason);
            }
            continue;
        }
        if let Some(a) = fix::apply_fix(&path, kinds)? {
            any = true;
            if a.outcome.success {
                println!("{name}: {} {}", p.role(Role::Ahead, "✓"), a.fix.op.label());
            } else {
                println!(
                    "{name}: {} {}\n{}",
                    p.role(Role::Error, "✗"),
                    a.fix.op.label(),
                    indent(&a.outcome.output)
                );
            }
        }
    }
    if !any {
        println!("nothing to fix");
    }
    Ok(())
}

// ---------- mark ----------

fn mark(ws: &mut Workspace, m: &MarkArgs) -> Result<()> {
    let sel = ws.select(
        &m.patterns,
        ViewFilter {
            show_hidden: true,
            ..Default::default()
        },
    );
    if sel.is_empty() {
        bail!("no repos match {:?}", m.patterns);
    }
    let snooze_until = match &m.snooze {
        Some(d) => {
            let secs = argus_core::state::parse_duration(d)
                .with_context(|| format!("bad duration {d:?}"))?;
            Some(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(secs))
        }
        None => None,
    };
    let tri = |yes: bool, no: bool| {
        if yes {
            Some(Some(true))
        } else if no {
            Some(Some(false))
        } else {
            None
        }
    };
    for i in sel {
        let key = ws.entries[i].key.clone();
        ws.state.mark(&key, |mk| {
            if m.clear {
                *mk = Default::default();
            }
            if let Some(v) = tri(m.favourite, m.no_favourite) {
                mk.favourite = v;
            }
            if let Some(v) = tri(m.sync, m.no_sync) {
                mk.sync = v;
            }
            if let Some(v) = tri(m.exclude, m.include) {
                mk.exclude = v;
            }
        });
        if let Some(t) = snooze_until {
            ws.state.snoozed.insert(key.clone(), t);
        }
        if m.unsnooze || m.clear {
            ws.state.snoozed.remove(&key);
        }
        println!("{}", ws.entries[i].display_name());
    }
    Ok(())
}

// ---------- forge ----------

fn forge_cmd(cmd: ForgeCmd) -> Result<()> {
    match cmd {
        ForgeCmd::List {
            owner,
            team,
            host,
            json,
        } => {
            let f = argus_forge::for_track("github", &host)?;
            let repos = f.list(&owner, team.as_deref())?;
            if json {
                serde_json::to_writer_pretty(std::io::stdout().lock(), &repos)?;
                println!();
            } else {
                for r in repos {
                    let tags = [(r.archived, " [archived]"), (r.fork, " [fork]")]
                        .iter()
                        .filter(|t| t.0)
                        .map(|t| t.1)
                        .collect::<String>();
                    println!("{}{tags}", r.id.slug());
                }
            }
        }
    }
    Ok(())
}
