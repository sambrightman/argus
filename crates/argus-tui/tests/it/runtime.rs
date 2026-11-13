//! The runtime carrying out the app's effects: files written, background
//! work done and its results applied, URLs opened, programs run. Real
//! repos and files; only the forge is faked.

use std::path::{Path, PathBuf};
use std::time::Duration;

use argus_core::config::Config;
use argus_core::forge::{Forge, ForgeError, RemoteRepo};
use argus_core::identity::RepoId;
use argus_core::status::Head;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{Action, App, Effect, Scope};
use argus_tui::{Options, Runtime, run_suspended};

const WAIT: Duration = Duration::from_secs(60);

fn no_forge(_: &str, _: &str) -> Result<Box<dyn Forge>, ForgeError> {
    Err(ForgeError::Auth("no forge in this test".into()))
}

/// A scanned app over three clones (`clean`, `dirty`, `feature` on a
/// pushed branch), and a runtime writing to the sandbox's config and state.
fn setup(extra: &str) -> (Sandbox, App, Runtime) {
    let sb = Sandbox::new();
    for n in ["clean", "dirty", "feature"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty"), "README", "changed\n");
    let f = sb.path("dev/feature");
    git(&f, &["switch", "-q", "-c", "feat"]);
    argus_core::testkit::commit(&f, "feat");
    git(&f, &["push", "-q", "-u", "origin", "feat"]);
    let rt = runtime(&sb, &sb.path("config.toml"), "true", &no_forge);
    let cfg = format!(
        "roots = ['{}']\n{extra}\n[ui]\nattention_only = false",
        sb.path("dev").display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    (sb, App::new(ws), rt)
}

fn runtime(
    sb: &Sandbox,
    config: &Path,
    opener: &str,
    forge: argus_core::pull::ForgeFactory<'static>,
) -> Runtime {
    Runtime::new(Options {
        state_path: sb.path("state.toml"),
        config_path: config.to_path_buf(),
        forge_factory: forge,
        notices: vec![],
        opener: opener.into(),
    })
}

/// Put the cursor on the repo named `name`.
fn select(app: &mut App, name: &str) {
    app.act(Action::Top);
    while app.current().unwrap().display_name() != name {
        let at = app.cursor;
        app.act(Action::NextRepo);
        assert_ne!(app.cursor, at, "{name} not listed");
    }
}

/// Carry out `effects` and everything that follows, until no background
/// work is left.
fn run(app: &mut App, rt: &mut Runtime, effects: Vec<Effect>) {
    for e in effects {
        assert!(rt.exec(app, e).is_empty(), "nothing needs the terminal");
    }
    assert!(rt.settle(app, WAIT), "background work didn't finish");
}

fn path(app: &App, name: &str) -> PathBuf {
    let i = app
        .ws
        .entries
        .iter()
        .position(|e| e.display_name() == name)
        .unwrap();
    app.ws.entries[i].path.clone()
}

#[test]
fn marks_are_written_to_the_config_file() {
    let (sb, mut app, mut rt) = setup("");
    select(&mut app, "acme/clean");
    let fx = app.act(Action::ToggleLabel(0));
    run(&mut app, &mut rt, fx);
    let saved = Config::load(&sb.path("config.toml")).unwrap();
    assert_eq!(saved.marks["favourite"].repos, ["github.com/acme/clean"]);
    // inverse: toggling back removes it
    let fx = app.act(Action::ToggleLabel(0));
    run(&mut app, &mut rt, fx);
    assert!(
        Config::load(&sb.path("config.toml")).unwrap().marks["favourite"]
            .repos
            .is_empty()
    );
}

#[test]
fn a_failed_save_is_reported() {
    let (sb, mut app, _) = setup("");
    // a directory where the config file should be
    std::fs::create_dir_all(sb.path("blocked/config.toml")).unwrap();
    let mut rt = runtime(&sb, &sb.path("blocked/config.toml"), "true", &no_forge);
    select(&mut app, "acme/clean");
    let fx = app.act(Action::ToggleLabel(0));
    run(&mut app, &mut rt, fx);
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(is_error && msg.contains("config.toml"), "{msg}");
}

#[test]
fn state_is_written() {
    let (sb, mut app, mut rt) = setup("");
    select(&mut app, "acme/clean");
    app.act(Action::Snooze); // prompts, with 3d filled in
    let fx = app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    run(&mut app, &mut rt, fx);
    let state = State::load(&sb.path("state.toml")).unwrap();
    assert!(
        state.snoozed.contains_key("github.com/acme/clean"),
        "{state:?}"
    );
}

#[test]
fn a_scan_finds_the_repos() {
    let (sb, _, mut rt) = setup("");
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    let mut app = App::new(ws);
    app.scanning = true;
    run(&mut app, &mut rt, vec![Effect::Scan]);
    assert!(!app.scanning);
    assert_eq!(app.ws.entries.len(), 3);
    assert!(
        app.message.as_ref().unwrap().0.contains("3 repos"),
        "{:?}",
        app.message
    );
}

#[test]
fn a_refresh_reads_new_status() {
    let (sb, mut app, mut rt) = setup("");
    let clean = path(&app, "acme/clean");
    write(&sb.path("dev/clean"), "new-file", "x\n");
    run(
        &mut app,
        &mut rt,
        vec![Effect::Refresh(vec![clean.clone()])],
    );
    let s = app.ws.get(&clean).unwrap().status().unwrap();
    assert_eq!(s.changes.untracked, 1);
}

#[test]
fn jobs_run_and_their_results_are_applied() {
    let (_sb, mut app, mut rt) = setup("");
    select(&mut app, "acme/dirty");
    let dirty = path(&app, "acme/dirty");
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('z'),
        crossterm::event::KeyModifiers::NONE,
    ));
    let fx = app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('z'),
        crossterm::event::KeyModifiers::NONE,
    ));
    run(&mut app, &mut rt, fx);
    assert!(app.running.is_empty());
    let entry = app.log.last().unwrap();
    assert!(
        entry.ok && entry.label.starts_with("stash"),
        "{:?}",
        entry.label
    );
    // the status was refreshed after the job
    let s = app.ws.get(&dirty).unwrap().status().unwrap();
    assert!(
        s.changes.is_clean() && s.stashes.len() == 1,
        "{:?}",
        s.changes
    );
}

#[test]
fn a_failing_job_is_logged_and_reported() {
    let (_sb, mut app, mut rt) = setup("");
    select(&mut app, "acme/clean");
    let clean = path(&app, "acme/clean");
    // a job that fails: switching to a branch that doesn't exist
    let fx = vec![Effect::Jobs(vec![argus_core::pull::Job {
        path: clean.clone(),
        family: clean.clone(),
        label: "switch nowhere".into(),
        op: argus_core::ops::Op::Switch {
            branch: "nowhere".into(),
        },
    }])];
    run(&mut app, &mut rt, fx);
    let entry = app.log.last().unwrap();
    assert!(!entry.ok && !entry.output.is_empty(), "{:?}", entry.output);
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(is_error && msg.contains("failed"), "{msg}");
}

#[test]
fn auto_fix_applies_and_refreshes() {
    let (_sb, mut app, mut rt) = setup("[fix]\nauto = ['switch-default']");
    select(&mut app, "acme/feature");
    let feature = path(&app, "acme/feature");
    let fx = app.act(Action::AutoFix(Scope::Selected));
    run(&mut app, &mut rt, fx);
    let s = app.ws.get(&feature).unwrap().status().unwrap();
    assert_eq!(s.head, Head::Branch("main".into()));
    assert!(app.log.last().unwrap().ok);
}

#[test]
fn a_fix_asked_for_an_ignored_state_still_leaves_it_alone() {
    let (_sb, mut app, mut rt) = setup(
        "[fix]\nauto = ['switch-default']\n[ignore]\nnon_default_branch = ['acme/feature@feat']",
    );
    let feature = path(&app, "acme/feature");
    // as a stale selection might: the runtime checks for itself
    let fx = vec![Effect::Fix {
        paths: vec![feature.clone()],
        kinds: app.ws.config.fix.auto.clone(),
    }];
    run(&mut app, &mut rt, fx);
    let s = app.ws.get(&feature).unwrap().status().unwrap();
    assert_eq!(s.head, Head::Branch("feat".into()));
}

#[test]
fn opening_details_describes_a_head_past_the_limit_and_the_badge_follows() {
    let (_sb, mut app, mut rt) = setup("describe_limit = 1");
    let clean = path(&app, "acme/clean");
    git(&clean, &["tag", "v1"]);
    for i in 0..3 {
        commit(&clean, &format!("c{i}"));
    }
    git(&clean, &["switch", "-q", "--detach"]);
    let fx = app.act(Action::Refresh);
    run(&mut app, &mut rt, fx);
    let s = app.ws.get(&clean).unwrap().status().unwrap();
    assert!(s.describe_gave_up && s.describe.is_none(), "{s:?}");
    select(&mut app, "acme/clean");
    let fx = app.act(Action::ToggleExpand);
    assert_eq!(fx, [Effect::Describe(clean.clone())]);
    run(&mut app, &mut rt, fx);
    let want = git(&clean, &["describe", "--tags"]);
    let e = app.ws.get(&clean).unwrap();
    assert_eq!(e.status().unwrap().describe.as_deref(), Some(want.trim()));
    let badge = argus_core::badges::branch(e, &argus_core::icons::UNICODE)
        .unwrap()
        .text;
    assert!(badge.ends_with(want.trim()), "{badge}");
    // later refreshes keep it
    let fx = app.act(Action::Refresh);
    run(&mut app, &mut rt, fx);
    assert_eq!(
        app.ws
            .get(&clean)
            .unwrap()
            .status()
            .unwrap()
            .describe
            .as_deref(),
        Some(want.trim())
    );
}

#[test]
fn pull_and_clone_lists_the_forge_and_clones_new_repos() {
    let sb = Sandbox::new();
    sb.remote("acme", "fresh");
    let into = sb.path("dev/acme");
    let url = sb.path("forge/acme/fresh.git").display().to_string();
    let listed = vec![RemoteRepo {
        id: RepoId::new("github.com", "acme", "fresh"),
        ssh_url: url.clone(),
        https_url: url,
        archived: false,
        fork: false,
        default_branch: Some("main".into()),
        description: None,
    }];
    struct Fake(Vec<RemoteRepo>);
    impl Forge for Fake {
        fn kind(&self) -> &str {
            "fake"
        }
        fn host(&self) -> &str {
            "github.com"
        }
        fn list(&self, _: &str, _: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError> {
            Ok(self.0.clone())
        }
    }
    let factory: argus_core::pull::ForgeFactory<'static> = Box::leak(Box::new(
        move |_: &str, _: &str| -> Result<Box<dyn Forge>, ForgeError> {
            Ok(Box::new(Fake(listed.clone())))
        },
    ));
    std::fs::create_dir_all(&into).unwrap();
    let cfg = format!(
        "roots = ['{}']\n[[track]]\nowner = 'acme'\ninto = '{}'\nclone = 'auto'",
        sb.path("dev").display(),
        into.display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    let mut rt = runtime(&sb, &sb.path("config.toml"), "true", factory);
    let fx = app.act(Action::PullAndClone);
    run(&mut app, &mut rt, fx);
    assert!(into.join("fresh/.git").is_dir(), "cloned");
    // listed (by path: its remote is a local path, not a forge URL)
    let fresh = std::fs::canonicalize(into.join("fresh")).unwrap();
    assert!(
        app.ws.entries.iter().any(|e| e.path == fresh),
        "{:?}",
        app.ws.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
    // recorded as new, and saved
    let state = State::load(&sb.path("state.toml")).unwrap();
    assert_eq!(state.new.len(), 1, "{state:?}");
    assert!(state.last_pull.is_some());
}

#[test]
fn a_failed_forge_listing_is_reported() {
    let sb = Sandbox::new();
    let cfg = format!(
        "roots = ['{}']\n[[track]]\nowner = 'acme'",
        sb.path("dev").display()
    );
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    let ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    let mut app = App::new(ws);
    let mut rt = runtime(&sb, &sb.path("config.toml"), "true", &no_forge);
    let fx = app.act(Action::PullAndClone);
    run(&mut app, &mut rt, fx);
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(is_error && msg.contains("forge listing"), "{msg}");
}

#[test]
fn urls_are_handed_to_the_opener() {
    let (sb, mut app, _) = setup("");
    // an opener that records what it was given; renamed into place, as
    // `>` creates the file before echo writes to it
    let script = sb.path("opener.sh");
    let got = sb.path("opened.txt");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$1\" > '{0}.tmp' && mv '{0}.tmp' '{0}'\n",
            got.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut rt = runtime(
        &sb,
        &sb.path("config.toml"),
        script.to_str().unwrap(),
        &no_forge,
    );
    select(&mut app, "acme/clean");
    let fx = app.act(Action::Browse);
    run(&mut app, &mut rt, fx);
    // the opener runs on its own; wait (briefly) for it to write
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !got.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let opened = std::fs::read_to_string(&got).expect("the configured opener was run");
    assert!(opened.contains("acme/clean"), "{opened}");
    // inverse: an opener that doesn't exist is reported
    let mut rt = runtime(&sb, &sb.path("config.toml"), "/no/such/opener", &no_forge);
    let fx = app.act(Action::Browse);
    run(&mut app, &mut rt, fx);
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(is_error && msg.contains("/no/such/opener"), "{msg}");
}

#[test]
fn suspending_comes_back_to_the_caller() {
    let (_sb, mut app, mut rt) = setup("");
    select(&mut app, "acme/clean");
    let fx = app.act(Action::Shell);
    assert!(matches!(fx.as_slice(), [Effect::Suspend { .. }]), "{fx:?}");
    let left = rt.exec(&mut app, fx.into_iter().next().unwrap());
    assert!(matches!(left.as_slice(), [Effect::Suspend { .. }]));
}

#[test]
fn suspended_programs_report_failure_but_shells_dont() {
    let dir = std::env::temp_dir();
    assert_eq!(run_suspended("true", &["x".into()], &dir, false), Ok(()));
    let e = run_suspended("false", &["x".into()], &dir, false).unwrap_err();
    assert!(e.contains("false exited"), "{e}");
    // a shell exits with its last command's code: not an error
    assert_eq!(run_suspended("false", &[], &dir, true), Ok(()));
    let e = run_suspended("/no/such/program", &[], &dir, true).unwrap_err();
    assert!(e.contains("/no/such/program"), "{e}");
}

#[test]
fn the_shell_is_a_login_shell_as_in_a_terminal() {
    // Not interactive (`-i`): an interactive bash takes over the terminal
    // for job control, which stops this test process once it exits. With
    // `-c`, bash still says whether it's a login shell.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let script = r#"shopt -q login_shell && echo "$0 $SHELL login" > out || echo "$0 not" > out"#;
    for login in [true, false] {
        let args = ["-c".to_string(), script.to_string()];
        let mut c = argus_tui::suspended_command("/bin/bash", &args, dir.path(), login);
        c.env("SHELL", "/my/shell")
            .stdin(std::process::Stdio::null());
        assert!(c.status().unwrap().success());
        let got = std::fs::read_to_string(&out).unwrap();
        if login {
            // named -bash, so a login shell; $SHELL as it was
            assert_eq!(got, "-bash /my/shell login\n");
        } else {
            assert_eq!(got, "/bin/bash not\n");
        }
    }
}

#[test]
fn quitting() {
    let (_sb, mut app, mut rt) = setup("");
    run(&mut app, &mut rt, vec![Effect::Quit]);
    assert!(app.quit);
}
