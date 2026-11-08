//! `argus`: the command-line front-end. With no subcommand it starts the TUI.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use argus_core::badges::{self, Badge, Role};
use argus_core::fix::{self, FixKind};
use argus_core::icons::{self, Icons};
use argus_core::pull;
use argus_core::rules::{IssueList, Mark};
use argus_core::runner::{self, Event};
use argus_core::{Config, State, ViewFilter, Workspace, configfile, paths};
use clap::{Args, Parser, Subcommand, ValueEnum};
use unicode_width::UnicodeWidthStr;

#[derive(Parser)]
#[command(
    name = "argus",
    version,
    about = "Overview, clean up and pull many git clones"
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
    /// Stay on each root's filesystem: don't enter other mounts below a
    /// root, or follow links to them.
    #[arg(long, short = 'x', global = true)]
    one_file_system: bool,
    /// Print one JSON document on stdout instead of text (messages and
    /// progress still go to stderr). Not for the TUI or completions.
    #[arg(long, global = true)]
    json: bool,
    /// Proceed with permanently destructive operations without asking
    /// (otherwise they ask on the terminal, or refuse without one).
    #[arg(long, short = 'y', global = true)]
    yes: bool,
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
    },
    /// Pull the repos marked for pull; list tracked owners and clone their
    /// new repos.
    Pull {
        /// Show what would happen.
        #[arg(long, short = 'n')]
        dry_run: bool,
        /// Don't query forges or clone; only pull.
        #[arg(long)]
        no_clone: bool,
        /// Only these repos (glob on name/key/path).
        #[arg(add = repo_completer())]
        patterns: Vec<String>,
    },
    /// Apply safe fixes (default: those in `[fix] auto`), one per repo per
    /// run. `-k abort` and `-k delete-branches` apply every one they find,
    /// after asking (or with --yes).
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
        #[arg(add = repo_completer())]
        patterns: Vec<String>,
    },
    /// Set favourite / pull / ignore (edits those lists in config), or
    /// snooze repos.
    Mark(MarkArgs),
    /// Acknowledge "new" repos (all, or matching patterns).
    Ack {
        /// Repos (glob on name/key/path).
        #[arg(add = repo_completer())]
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
    /// Print a shell completion script, e.g. `argus completions zsh >
    /// ~/.zfunc/_argus` or `argus completions fish | source`.
    Completions {
        /// bash, zsh, fish, elvish or powershell.
        shell: clap_complete::Shell,
    },
    /// Open the TUI on sample repos in every state (deleted on quit), to see
    /// the whole UI and try themes and icons (`v t`, `v i`; `?` legend).
    Demo {
        /// Theme to start with [default: `[ui] theme`].
        #[arg(long, value_name = "NAME")]
        theme: Option<String>,
        /// Icon set to start with: unicode, nerd or ascii [default: `[ui] icons`].
        #[arg(long, value_name = "SET")]
        icons: Option<String>,
        /// Print every icon and some alternatives between markers, to see
        /// which overlap their neighbours in this terminal and font.
        #[arg(long)]
        glyphs: bool,
    },
}

#[derive(Args, Clone, Copy)]
struct View {
    /// Include clean repos.
    #[arg(long, short = 'a')]
    all: bool,
    /// Include ignored and snoozed repos.
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
    #[arg(required = true, add = repo_completer())]
    patterns: Vec<String>,
    /// Sort above other repos in the same group.
    #[arg(long, overrides_with = "no_favourite")]
    favourite: bool,
    /// Not a favourite, whatever patterns in config say.
    #[arg(long)]
    no_favourite: bool,
    /// Pull with `argus pull`.
    #[arg(long, overrides_with = "no_pull")]
    pull: bool,
    /// Don't pull with `argus pull`, whatever patterns in config say.
    #[arg(long)]
    no_pull: bool,
    /// Hide, and leave out of pulls, clones and bulk operations.
    #[arg(long, overrides_with = "unignore")]
    ignore: bool,
    /// Not ignored, whatever patterns in config say.
    #[arg(long)]
    unignore: bool,
    /// Ignore one issue for these repos (`[ignore] <issue>`):
    /// non-default-branch, detached, no-remote, no-upstream, stashes,
    /// unpushed-branches, behind. Pinned to the current branch, tag or
    /// commit where the list pins those; stashes aren't pinned unless
    /// `--pin` says so.
    #[arg(long, value_name = "ISSUE", value_parser = issue_list)]
    ignore_issue: Vec<IssueList>,
    /// With `--ignore-issue`: pin to the current value(s), e.g. each
    /// stash's message, so a new one is flagged again.
    #[arg(long, overrides_with = "no_pin")]
    pin: bool,
    /// With `--ignore-issue`: don't pin, so the issue stays ignored
    /// whatever the branch, HEAD or stashes become.
    #[arg(long)]
    no_pin: bool,
    /// Stop ignoring an issue for these repos.
    #[arg(long, value_name = "ISSUE", value_parser = issue_list)]
    unignore_issue: Vec<IssueList>,
    /// Hide temporarily, e.g. `3d`, `12h`, `2w`.
    #[arg(long, value_name = "DURATION")]
    snooze: Option<String>,
    /// End a snooze early.
    #[arg(long)]
    unsnooze: bool,
    /// Remove the repos' own entries from the favourite, pull and ignore
    /// lists (patterns decide again) and end any snooze.
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
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FixArg {
    SwitchDefault,
    Push,
    Publish,
    PushBranches,
    Reattach,
    FastForward,
    Pull,
    Stash,
    StashPop,
    Abort,
    DeleteBranches,
}

impl From<FixArg> for FixKind {
    fn from(f: FixArg) -> Self {
        match f {
            FixArg::SwitchDefault => FixKind::SwitchDefault,
            FixArg::Push => FixKind::Push,
            FixArg::Publish => FixKind::Publish,
            FixArg::PushBranches => FixKind::PushBranches,
            FixArg::Reattach => FixKind::Reattach,
            FixArg::FastForward => FixKind::FastForward,
            FixArg::Pull => FixKind::Pull,
            FixArg::Stash => FixKind::Stash,
            FixArg::StashPop => FixKind::StashPop,
            FixArg::Abort => FixKind::Abort,
            FixArg::DeleteBranches => FixKind::DeleteBranches,
        }
    }
}

struct Ctx {
    config_path: PathBuf,
    state_path: PathBuf,
}

fn main() {
    // When the shell asks for completions (`COMPLETE=<shell>`), answer and
    // exit.
    clap_complete::CompleteEnv::with_factory(<Cli as clap::CommandFactory>::command).complete();
    if let Err(e) = run() {
        eprintln!("argus: {}", report(&e));
        std::process::exit(1);
    }
}

/// `e` and its causes, leaving out a cause whose text is already in the
/// message: many errors include their cause in their own message.
fn report(e: &anyhow::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.chain().skip(1) {
        let c = cause.to_string();
        if !msg.contains(&c) {
            msg = format!("{msg}: {c}");
        }
    }
    msg
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx {
        config_path: cli.config.clone().unwrap_or_else(paths::config_file),
        state_path: cli.state.clone().unwrap_or_else(paths::state_file),
    };
    let writes_nothing = matches!(cli.cmd, Some(Cmd::Paths | Cmd::Completions { .. }));
    if !writes_nothing && configfile::ensure_exists(&ctx.config_path)? {
        eprintln!(
            "argus: created {} (every setting listed with its default, commented out)",
            paths::contract_tilde(&ctx.config_path)
        );
    }
    let json = cli.json;
    if json {
        match &cli.cmd {
            None | Some(Cmd::Tui) => bail!("--json isn't available for the TUI"),
            Some(Cmd::Completions { .. }) => bail!("--json isn't available for completions"),
            Some(Cmd::Demo { glyphs: false, .. }) => {
                bail!("--json isn't available for the demo TUI (try demo --glyphs)")
            }
            _ => {}
        }
    }
    match cli.cmd {
        Some(Cmd::Root { cmd }) => return root_cmd(&ctx, cmd, json),
        Some(Cmd::Paths) => {
            if json {
                return print_json(&serde_json::json!({
                    "config": ctx.config_path,
                    "state": ctx.state_path,
                }));
            }
            println!("config: {}", ctx.config_path.display());
            println!("state:  {}", ctx.state_path.display());
            return Ok(());
        }
        Some(Cmd::Forge { cmd }) => return forge_cmd(cmd, json),
        Some(Cmd::Completions { shell }) => {
            // The shell code asks `argus` for candidates as you type; source it
            // fresh at shell startup, so it always matches the installed argus.
            let name = shell.to_string();
            let shells = clap_complete::env::Shells::builtins();
            let completer = shells
                .completer(&name)
                .with_context(|| format!("no completions for {name}"))?;
            completer.write_registration(
                "COMPLETE",
                "argus",
                "argus",
                "argus",
                &mut std::io::stdout(),
            )?;
            return Ok(());
        }
        Some(Cmd::Demo { glyphs: true, .. }) => return demo_glyphs(&ctx, json),
        Some(Cmd::Demo { theme, icons, .. }) => {
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
    if cli.one_file_system {
        ws.config.one_file_system = true;
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
                config_path: ctx.config_path.clone(),
                forge_factory: forge_factory(),
                notices,
                opener: argus_tui::default_opener().into(),
            };
            argus_tui::run(ws, opts).map_err(|e| anyhow::anyhow!(e))?;
        }
        Cmd::Status { view } => {
            scan(&mut ws);
            status(&ws, view.into(), json)?;
        }
        Cmd::Pull {
            dry_run,
            no_clone,
            patterns,
        } => {
            scan(&mut ws);
            pull_cmd(&mut ws, &ctx, dry_run, no_clone, &patterns, json)?;
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
            fix_cmd(&mut ws, &kinds, dry_run, &patterns, json, cli.yes)?;
        }
        Cmd::Mark(m) => {
            scan(&mut ws);
            mark(&mut ws, &ctx, &m, json, cli.yes)?;
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
            let acked: Vec<String> = sel
                .iter()
                .map(|&i| &ws.entries[i])
                .filter(|e| ws.state.new.contains(&e.key))
                .map(|e| e.display_name())
                .collect();
            let keys: Vec<String> = sel.iter().map(|&i| ws.entries[i].key.clone()).collect();
            ws.state.new.retain(|k| !keys.contains(k));
            if json {
                print_json(&serde_json::json!({ "acknowledged": acked }))?;
            } else {
                println!("acknowledged {}", acked.len());
            }
            save_state(&ws, &ctx)?;
        }
        Cmd::Root { .. }
        | Cmd::Paths
        | Cmd::Forge { .. }
        | Cmd::Demo { .. }
        | Cmd::Completions { .. } => unreachable!(),
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
    let mut notices = config.fix.warnings();
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
    if let Some(lines) = state.old_marks_as_config() {
        notices.push(format!(
            "{} has marks from an older argus, no longer used: they're set in config now. \
             Copy what you want into {}, then delete the [marks] tables from the state file:\n{lines}",
            paths::contract_tilde(&ctx.state_path),
            paths::contract_tilde(&ctx.config_path),
        ));
    }
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

/// Ask before a permanently destructive operation: proceed with `--yes`,
/// else ask on the terminal; without one, refuse.
fn confirm(what: &str, yes: bool) -> Result<()> {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    confirm_with(
        what,
        yes,
        interactive,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    )
}

fn confirm_with(
    what: &str,
    yes: bool,
    interactive: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !interactive {
        bail!("{what}: needs confirmation; pass --yes to proceed");
    }
    write!(out, "argus: {what}? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    match answer.trim().to_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => bail!("cancelled"),
    }
}

/// Completes repo patterns with the names of the repos argus finds, using
/// the config named by `ARGUS_CONFIG` or the default one (options like
/// `--root` aren't seen while completing).
fn repo_completer() -> clap_complete::ArgValueCompleter {
    clap_complete::ArgValueCompleter::new(|current: &std::ffi::OsStr| {
        let Some(current) = current.to_str() else {
            return vec![];
        };
        let path = std::env::var_os("ARGUS_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(paths::config_file);
        let mut config = Config::load(&path).unwrap_or_default();
        if config.roots.is_empty()
            && let Ok(cwd) = std::env::current_dir()
        {
            config.roots = vec![cwd];
        }
        let Ok(ws) = Workspace::new(config, State::default()) else {
            return vec![];
        };
        ws.repo_names()
            .into_iter()
            .filter(|n| n.starts_with(current))
            .map(clap_complete::CompletionCandidate::new)
            .collect()
    })
}

/// `--json` output: one pretty-printed document on stdout.
fn print_json(v: &impl serde::Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, v)?;
    writeln!(out)?;
    Ok(())
}

fn root_cmd(ctx: &Ctx, cmd: RootCmd, json: bool) -> Result<()> {
    let path = &ctx.config_path;
    match cmd {
        RootCmd::Add { dirs } => {
            let dirs = if dirs.is_empty() {
                vec![std::env::current_dir()?]
            } else {
                dirs
            };
            let r = configfile::add_roots(path, &dirs)?;
            let shown = |f: &std::path::Path| paths::contract_tilde(f);
            if json {
                return print_json(&serde_json::json!({
                    "added": r.added,
                    "file": shown(&r.file),
                    "already": r.already.iter().map(|(root, f)| serde_json::json!({
                        "root": root, "file": shown(f),
                    })).collect::<Vec<_>>(),
                    "replaces": r.replaces.iter().map(|f| shown(f)).collect::<Vec<_>>(),
                }));
            }
            // the file is named only when it isn't the main one
            let other = |f: &std::path::Path, word: &str| {
                if f == path.as_path() {
                    String::new()
                } else {
                    format!(" ({word} {})", shown(f))
                }
            };
            for (root, f) in &r.already {
                println!("{root} is already configured{}", other(f, "in"));
            }
            for a in &r.added {
                println!("added {a}{}", other(&r.file, "to"));
            }
            for f in &r.replaces {
                eprintln!(
                    "argus: note: roots in {} have no effect: those in {} replace them",
                    shown(f),
                    shown(&r.file)
                );
            }
        }
        RootCmd::Remove { dirs } => {
            let r = configfile::remove_roots(path, &dirs)?;
            let shown = |f: &std::path::Path| paths::contract_tilde(f);
            if json {
                return print_json(&serde_json::json!({
                    "removed": r.removed.iter().map(|x| serde_json::json!({
                        "root": x.root,
                        "files": x.files.iter().map(|f| shown(f)).collect::<Vec<_>>(),
                        "no_effect": x.no_effect.iter().map(|f| shown(f)).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "not_roots": r.not_roots,
                }));
            }
            for x in &r.removed {
                let from = if x.files == [path.clone()] {
                    String::new()
                } else {
                    let names: Vec<String> = x.files.iter().map(|f| shown(f)).collect();
                    format!(" from {}", names.join(", "))
                };
                println!("removed {}{from}", x.root);
                for f in &x.no_effect {
                    eprintln!(
                        "argus: note: removing it from {} had no effect: a higher file's roots replace those",
                        shown(f)
                    );
                }
            }
            for n in &r.not_roots {
                eprintln!("argus: {n} is not configured as a root");
            }
        }
        RootCmd::List => {
            let config = Config::load(path)?;
            if json {
                let roots: Vec<_> = config
                    .roots
                    .iter()
                    .map(|r| serde_json::json!({ "path": paths::contract_tilde(r), "exists": r.is_dir() }))
                    .collect();
                return print_json(&roots);
            }
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

fn forge_factory() -> pull::ForgeFactory<'static> {
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
            Role::DiffAdd => "32",
            Role::DiffRemove => "31",
            Role::DiffHunk => "36",
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
    let (icons, warnings) = ws.config.ui.icons();
    for w in warnings {
        eprintln!("argus: config: {w}");
    }
    icons
}

fn status(ws: &Workspace, filter: ViewFilter, json: bool) -> Result<()> {
    let order = ws.ordered(filter);
    if json {
        let v: Vec<_> = order.iter().map(|&i| &ws.entries[i]).collect();
        return print_json(&v);
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

// ---------- pull ----------

fn pull_cmd(
    ws: &mut Workspace,
    ctx: &Ctx,
    dry_run: bool,
    no_clone: bool,
    patterns: &[String],
    json: bool,
) -> Result<()> {
    let p = Paint::detect();
    let mut jobs = vec![];
    let mut tracks = pull::PullPlan::default();
    if !no_clone && !ws.config.tracks.is_empty() {
        let listings = pull::list_tracks(ws, forge_factory());
        for (k, e) in &listings.errors {
            eprintln!("{} {k}: {e}", p.role(Role::Error, "forge error"));
        }
        tracks = pull::plan_tracks(ws, &listings.repos, listings.protocol);
        jobs.extend(tracks.jobs.iter().cloned());
    }
    let mut updates = pull::plan_updates(ws);
    if !patterns.is_empty() {
        let sel: Vec<PathBuf> = ws
            .select(patterns, ViewFilter::default())
            .into_iter()
            .map(|i| ws.entries[i].path.clone())
            .collect();
        updates.retain(|j| sel.contains(&j.path));
    }
    jobs.extend(updates);

    let new_on_forge: Vec<String> = tracks.missing.iter().map(|r| r.id.slug()).collect();
    let orphans: Vec<String> = tracks
        .orphans
        .iter()
        .map(|o| paths::contract_tilde(o))
        .collect();
    let job_json = |j: &pull::Job, outcome: &str, output: Option<&str>| {
        serde_json::json!({
            "label": j.label,
            "path": j.path,
            "outcome": outcome,
            "output": output,
        })
    };
    if json && dry_run {
        return print_json(&serde_json::json!({
            "dry_run": true,
            "new_on_forge": new_on_forge,
            "orphans": orphans,
            "jobs": jobs.iter().map(|j| job_json(j, "planned", None)).collect::<Vec<_>>(),
        }));
    }
    for r in tracks.missing.iter().filter(|_| !json) {
        println!(
            "{} {} (not cloned)",
            p.role(Role::New, "new on forge:"),
            r.id.slug()
        );
    }
    for o in tracks.orphans.iter().filter(|_| !json) {
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
    let added = pull::record_clones(ws, &jobs, &results);
    for a in added.iter().filter(|_| !json) {
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
    if json {
        let done: Vec<_> = jobs
            .iter()
            .zip(&results)
            .map(|(j, r)| match r {
                Some(o) if o.success => job_json(j, "ok", Some(&o.output)),
                Some(o) => job_json(j, "failed", Some(&o.output)),
                None => job_json(j, "skipped", None),
            })
            .collect();
        print_json(&serde_json::json!({
            "dry_run": false,
            "new_on_forge": new_on_forge,
            "orphans": orphans,
            "jobs": done,
            "cloned": added,
            "summary": {
                "total": total,
                "ok": total - failed - skipped,
                "failed": failed,
                "skipped": skipped,
            },
        }))?;
    } else {
        println!(
            "{total} jobs: {} ok, {failed} failed, {skipped} skipped",
            total - failed - skipped
        );
    }
    ws.state.last_pull = Some(jiff::Timestamp::now());
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
        config_path: d.sandbox.path("config.toml"),
        forge_factory: forge_factory(),
        notices: vec![format!(
            "demo repos in {} (deleted on quit) · v t: theme · v i: icons · ?: legend",
            d.root.display()
        )],
        opener: argus_tui::default_opener().into(),
    };
    argus_tui::run(ws, opts).map_err(|e| anyhow::anyhow!(e))
}

/// One glyph as `|g|MgM`, with `*` if it takes two cells in CJK mode.
fn glyph_cell(g: &str) -> String {
    let wide = UnicodeWidthStr::width_cjk(g) > UnicodeWidthStr::width(g);
    format!("|{g}|M{g}M{}", if wide { "*" } else { " " })
}

fn demo_glyphs(ctx: &Ctx, json: bool) -> Result<()> {
    let ui = Config::load(&ctx.config_path)
        .map(|c| c.ui)
        .unwrap_or_default();
    let (yours, warnings) = ui.icons();
    for w in warnings {
        eprintln!("argus: config: {w}");
    }
    if json {
        let set = |i: &Icons| -> serde_json::Map<String, serde_json::Value> {
            icons::ICON_NAMES
                .iter()
                .map(|n| (n.to_string(), i.get(n).unwrap().into()))
                .collect()
        };
        let mut sets: serde_json::Map<String, serde_json::Value> = icons::SETS
            .iter()
            .map(|n| (n.to_string(), set(&icons::by_name(n)).into()))
            .collect();
        sets.insert("yours".into(), set(&yours).into());
        return print_json(&serde_json::json!({
            "sets": sets,
            "punctuation": icons::PUNCTUATION,
            "alternatives": icons::ALTERNATIVES,
        }));
    }
    println!("Each glyph is shown between bars and between letters. One drawn from a");
    println!("fallback font (missing from yours) may run into its neighbours.");
    println!("*: two cells wide where ambiguous-width characters are (CJK mode).\n");
    let sets: Vec<(&str, Icons)> = icons::SETS
        .iter()
        .map(|&n| (n, icons::by_name(n)))
        .chain([("yours", yours)])
        .collect();
    let cells: Vec<Vec<String>> = icons::ICON_NAMES
        .iter()
        .map(|n| {
            sets.iter()
                .map(|(_, i)| glyph_cell(i.get(n).unwrap()))
                .collect()
        })
        .collect();
    let width = |col: usize| {
        cells
            .iter()
            .map(|r| UnicodeWidthStr::width(r[col].as_str()))
            .chain([sets[col].0.len()])
            .max()
            .unwrap_or(0)
            + 2
    };
    let widths: Vec<usize> = (0..sets.len()).map(width).collect();
    let header: String = sets
        .iter()
        .zip(&widths)
        .map(|((n, _), &w)| pad(n, w))
        .collect();
    let name_w = icons::ICON_NAMES.iter().map(|n| n.len()).max().unwrap_or(0) + 2;
    println!("{}{}", pad("icon", name_w), header.trim_end());
    for (name, row) in icons::ICON_NAMES.iter().zip(&cells) {
        let line: String = row.iter().zip(&widths).map(|(c, &w)| pad(c, w)).collect();
        println!("{}{}", pad(name, name_w), line.trim_end());
    }
    let row = |c: char, what: &str| {
        let cell = glyph_cell(&c.to_string());
        let line = format!("{c}  U+{:04X}  {}{what}", c as u32, pad(&cell, 10));
        println!("{}", line.trim_end());
    };
    println!("\npunctuation (not icons)");
    for c in icons::PUNCTUATION.chars() {
        row(c, "used as text");
    }
    println!("\nalternatives ([ui.icon_overrides])");
    for c in icons::ALTERNATIVES.chars() {
        row(c, "");
    }
    Ok(())
}

// ---------- fix ----------

/// Fetch the repos `patterns` select and refresh their status. Failures are
/// reported; planning continues with what is known.
fn fetch_selected(ws: &mut Workspace, patterns: &[String]) {
    let p = Paint::detect();
    let sel = ws.select(patterns, ViewFilter::default());
    let jobs = pull::plan_fetches(ws, &sel);
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

fn fix_json(f: &fix::Fix) -> serde_json::Value {
    serde_json::json!({
        "kind": f.kind.name(),
        "op": f.op.label(),
        "reason": f.reason,
        "blocked": f.blocked,
    })
}

/// Every fix of the destructive `kinds` in the selected repos: listed, then
/// applied once confirmed (`yes`, or asked). Returns whether there were
/// any, and the JSON records.
fn destructive_fixes(
    ws: &Workspace,
    kinds: &[FixKind],
    dry_run: bool,
    patterns: &[String],
    json: bool,
    yes: bool,
) -> Result<(bool, Vec<serde_json::Value>)> {
    let p = Paint::detect();
    let ic = icons(ws);
    let mut found = vec![];
    for i in ws.select(patterns, ViewFilter::default()) {
        let e = &ws.entries[i];
        let Some(s) = e.status() else { continue };
        for f in fix::plan(s, &ws.config.stash) {
            if kinds.contains(&f.kind) && f.blocked.is_none() {
                found.push((e.display_name(), e.path.clone(), f));
            }
        }
    }
    if found.is_empty() {
        return Ok((false, vec![]));
    }
    if dry_run {
        let mut records = vec![];
        for (name, path, f) in &found {
            if json {
                records.push(serde_json::json!({
                    "repo": name, "path": path, "planned": fix_json(f), "asks": true,
                }));
            } else {
                println!(
                    "{name}: would {} ({}), after asking",
                    f.op.label(),
                    f.reason
                );
            }
        }
        return Ok((true, records));
    }
    if !yes {
        for (name, _, f) in &found {
            eprintln!("  {name}: {} ({})", f.op.label(), f.reason);
        }
    }
    confirm(&format!("run the {} fix(es) above", found.len()), yes)?;
    let mut records = vec![];
    for (name, path, f) in found {
        let out = argus_core::ops::run(&path, &f.op);
        if json {
            records.push(serde_json::json!({
                "repo": name, "path": path, "applied": fix_json(&f),
                "ok": out.success, "output": out.output,
            }));
        } else if out.success {
            println!(
                "{name}: {} {}",
                p.role(Role::Ahead, ic.ok.trim_end()),
                f.op.label()
            );
        } else {
            println!(
                "{name}: {} {}\n{}",
                p.role(Role::Error, ic.failed.trim_end()),
                f.op.label(),
                indent(&out.output)
            );
        }
    }
    Ok((true, records))
}

fn fix_cmd(
    ws: &mut Workspace,
    kinds: &[FixKind],
    dry_run: bool,
    patterns: &[String],
    json: bool,
    yes: bool,
) -> Result<()> {
    let p = Paint::detect();
    let ic = icons(ws);
    let stash = ws.config.stash;
    let (destructive, kinds): (Vec<FixKind>, Vec<FixKind>) =
        kinds.iter().partition(|k| k.needs_confirmation());
    let (mut any, mut records) = destructive_fixes(ws, &destructive, dry_run, patterns, json, yes)?;
    let kinds = kinds.as_slice();
    let sel = if kinds.is_empty() {
        vec![]
    } else {
        ws.select(patterns, ViewFilter::default())
    };
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
            } = fix::preview(s, &e.fix_kinds(kinds), &stash);
            if chain.is_empty() && alternatives.is_empty() {
                continue;
            }
            any = true;
            if json {
                records.push(serde_json::json!({
                    "repo": name,
                    "path": path,
                    "planned": chain.first().map(fix_json),
                    "then": chain.iter().skip(1).map(fix_json).collect::<Vec<_>>(),
                    "alternatives": alternatives.iter().map(|f| {
                        let mut v = fix_json(f);
                        v["needs_kind"] = (!kinds.contains(&f.kind)).into();
                        v
                    }).collect::<Vec<_>>(),
                }));
                continue;
            }
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
        if let Some(a) = fix::apply_fix(&path, &e.fix_kinds(kinds), &stash)? {
            any = true;
            if json {
                records.push(serde_json::json!({
                    "repo": name,
                    "path": path,
                    "applied": fix_json(&a.fix),
                    "ok": a.outcome.success,
                    "output": a.outcome.output,
                }));
            } else if a.outcome.success {
                println!(
                    "{name}: {} {}",
                    p.role(Role::Ahead, ic.ok.trim_end()),
                    a.fix.op.label()
                );
            } else {
                println!(
                    "{name}: {} {}\n{}",
                    p.role(Role::Error, ic.failed.trim_end()),
                    a.fix.op.label(),
                    indent(&a.outcome.output)
                );
            }
        }
    }
    if json {
        print_json(&records)?;
    } else if !any {
        println!("nothing to fix");
    }
    Ok(())
}

// ---------- mark ----------

/// An `[ignore]` issue list by name (`non-default-branch` or `non_default_branch`).
fn issue_list(s: &str) -> std::result::Result<IssueList, String> {
    IssueList::from_name(s).ok_or_else(|| {
        let names: Vec<String> = IssueList::ALL
            .iter()
            .map(|l| l.name().replace('_', "-"))
            .collect();
        format!(
            "{s:?} isn't an issue that can be ignored; these can: {}",
            names.join(", ")
        )
    })
}

fn mark(ws: &mut Workspace, ctx: &Ctx, m: &MarkArgs, json: bool, yes: bool) -> Result<()> {
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
    if m.clear {
        let n = ws.marked_count(&sel);
        if n > 1 {
            confirm(&format!("clear marks of {n} repos"), yes)?;
        }
    }
    let before = ws.config.clone();
    if m.clear {
        ws.clear_marks(&sel)?;
    }
    for (mark, on, off) in [
        (Mark::Favourite, m.favourite, m.no_favourite),
        (Mark::Pull, m.pull, m.no_pull),
        (Mark::Ignore, m.ignore, m.unignore),
    ] {
        if on || off {
            ws.set_mark(&sel, mark, on)?;
        }
    }
    for (lists, on) in [(&m.ignore_issue, true), (&m.unignore_issue, false)] {
        for &l in lists {
            let pin = (m.pin || m.no_pin).then_some(m.pin);
            ws.set_issue_ignored(&sel, l, on, pin)?;
        }
    }
    configfile::update_mark_lists(&ctx.config_path, &before, &ws.config)?;
    let mut records = vec![];
    for i in sel {
        let key = ws.entries[i].key.clone();
        if let Some(t) = snooze_until {
            ws.state.snoozed.insert(key.clone(), t);
        }
        if m.unsnooze || m.clear {
            ws.state.snoozed.remove(&key);
        }
        if json {
            let f = &ws.entries[i].flags;
            records.push(serde_json::json!({
                "repo": ws.entries[i].display_name(),
                "key": key,
                "marks": {
                    "favourite": f.favourite,
                    "pull": f.pull,
                    "ignore": f.ignored,
                },
                "ignored_issues": ws.entries[i].ignored_issues.iter()
                    .filter_map(|i| IssueList::of(*i).map(|l| l.name()))
                    .collect::<Vec<_>>(),
                "snoozed_until": ws.state.snoozed.get(&key),
            }));
        } else {
            println!("{}", ws.entries[i].display_name());
        }
    }
    if json {
        print_json(&records)?;
    }
    Ok(())
}

// ---------- forge ----------

fn forge_cmd(cmd: ForgeCmd, json: bool) -> Result<()> {
    match cmd {
        ForgeCmd::List { owner, team, host } => {
            let f = argus_forge::for_track("github", &host)?;
            let repos = f.list(&owner, team.as_deref())?;
            if json {
                print_json(&repos)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_are_reported_unless_the_message_already_says_them() {
        let inner = anyhow::anyhow!("disk full");
        assert_eq!(report(&inner.context("saving")), "saving: disk full");
        let inner = anyhow::anyhow!("disk full");
        assert_eq!(
            report(&inner.context("saving: disk full")),
            "saving: disk full",
            "not twice"
        );
    }

    #[test]
    fn confirmation_takes_yes_and_nothing_else() {
        let ask = |yes: bool, interactive: bool, typed: &str| {
            let mut out = vec![];
            let r = confirm_with("drop it", yes, interactive, &mut typed.as_bytes(), &mut out);
            (
                r.map_err(|e| e.to_string()),
                String::from_utf8(out).unwrap(),
            )
        };
        for typed in ["y\n", "Y\n", "yes\n", " y \n"] {
            let (r, out) = ask(false, true, typed);
            assert!(r.is_ok(), "{typed:?}");
            assert_eq!(out, "argus: drop it? [y/N] ");
        }
        // inverse: anything else, or nothing, cancels
        for typed in ["n\n", "\n", "", "yeah\n", "no\n"] {
            assert_eq!(
                ask(false, true, typed).0,
                Err("cancelled".into()),
                "{typed:?}"
            );
        }
        // --yes doesn't ask
        assert_eq!(ask(true, true, ""), (Ok(()), String::new()));
        assert_eq!(ask(true, false, ""), (Ok(()), String::new()));
        // no terminal: refuse without asking
        let (r, out) = ask(false, false, "y\n");
        assert!(r.unwrap_err().contains("pass --yes"));
        assert!(out.is_empty());
    }
}
