//! Steps for `-v`: what argus's own code reports. The sink is the
//! process's one (each test runs in a process of its own), and each
//! keeps the steps of its own repo.

use std::sync::{Arc, Mutex};

use argus_core::ops::{self, Op};
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::trace::{self, Step};

/// Each git command an operation runs is a step, with its time, saying
/// where it ran and whether it failed.
#[test]
fn git_commands_are_steps() {
    let sb = Sandbox::new();
    let repo = sb.init("steps-repo");
    write(&repo, "new", "x\n");
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Each,
        Arc::new(move |s: Step| {
            if s.what.contains("steps-repo") {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    let added = ops::run(
        &repo,
        &Op::Add {
            paths: vec!["new".into()],
        },
    );
    let missing = ops::run(
        &repo,
        &Op::Add {
            paths: vec!["missing".into()],
        },
    );
    trace::set_sink(None);
    assert!(added.success && !missing.success);
    let got = got.lock().unwrap().clone();
    let git: Vec<&Step> = got
        .iter()
        .filter(|s| s.what.starts_with("git add"))
        .collect();
    assert_eq!(git.len(), 2, "{got:?}");
    assert!(git.iter().all(|s| s.took.is_some()));
    assert!(
        git[0].what.contains("new in ") && git[0].what.ends_with("steps-repo"),
        "{git:?}"
    );
    assert!(git[1].what.ends_with("steps-repo, failed"), "{git:?}");
}

/// Told whole, a repo's status holds its parts, and the branches part a
/// step for each branch, holding its checks.
#[test]
fn a_status_holds_its_parts_and_each_branch_its_checks() {
    let sb = Sandbox::new();
    sb.remote("o", "tree-repo");
    let repo = sb.clone_as_github("o", "tree-repo", "tree-repo");
    git(&repo, &["switch", "-q", "-c", "topic"]);
    write(&repo, "topic", "x\n");
    commit(&repo, "topic");
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| {
            if s.what.contains("tree-repo") {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    let status = argus_core::status::status(&repo, Default::default());
    trace::set_sink(None);
    assert!(status.is_ok());
    let got = got.lock().unwrap().clone();
    let status = got.iter().find(|s| s.kind == "status").expect("a status");
    let kinds = |s: &Step| s.steps.iter().map(|s| s.kind.clone()).collect::<Vec<_>>();
    let parts = kinds(status);
    for part in ["open", "refs", "branches", "files", "stashes"] {
        assert!(parts.iter().any(|p| p == part), "{part} in {parts:?}");
    }
    let branches = status.steps.iter().find(|s| s.kind == "branches").unwrap();
    // the remote branches' tips gathered first, then each branch
    let [tips, main, topic] = &branches.steps[..] else {
        panic!("the remote tips and two branches in {branches:?}");
    };
    assert_eq!(tips.what, "remote tips: 1", "{tips:?}");
    assert!(
        main.what.starts_with("branch main in ") && main.what.ends_with("tree-repo"),
        "{main:?}"
    );
    assert!(main.took.is_some());
    // merged: not looked at for being absorbed
    assert_eq!(kinds(main), ["upstream", "push branch", "merged"]);
    // not: it is, and what's not on a remote counted
    assert!(topic.what.starts_with("branch topic in "), "{topic:?}");
    assert_eq!(
        kinds(topic),
        ["upstream", "push branch", "merged", "absorbed", "unpushed"]
    );
    let unpushed = topic.steps.last().unwrap();
    assert!(unpushed.what.starts_with("unpushed: 1, "), "{unpushed:?}");
}

/// A status that failed says so.
#[test]
fn a_failed_status_says_so() {
    let sb = Sandbox::new();
    let not_a_repo = sb.path("not-a-repo");
    std::fs::create_dir_all(&not_a_repo).unwrap();
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Each,
        Arc::new(move |s: Step| {
            if s.kind == "status" && s.what.contains("not-a-repo") {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    let status = argus_core::status::status(&not_a_repo, Default::default());
    trace::set_sink(None);
    assert!(status.is_err());
    let got = got.lock().unwrap().clone();
    let [status] = &got[..] else {
        panic!("one status in {got:?}");
    };
    assert!(status.what.ends_with("not-a-repo, failed"), "{status:?}");
}

/// Each job run is a step, said by its label, holding its git commands;
/// one that failed says so.
#[test]
fn jobs_are_steps_holding_their_commands() {
    use argus_core::pull::Job;
    let sb = Sandbox::new();
    let repo = sb.init("jobs-repo");
    write(&repo, "new", "x\n");
    let job = |label: &str, path: &str| Job {
        path: repo.clone(),
        op: Op::Add {
            paths: vec![path.into()],
        },
        family: repo.clone(),
        label: label.into(),
    };
    let jobs = [job("jobs-ok", "new"), job("jobs-bad", "missing")];
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| {
            if s.kind == "job" {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    let cancel = std::sync::atomic::AtomicBool::new(false);
    argus_core::runner::run_jobs(&jobs, 1, &cancel, &|_| {});
    trace::set_sink(None);
    let got = got.lock().unwrap().clone();
    let whats: Vec<&str> = got.iter().map(|s| s.what.as_str()).collect();
    assert_eq!(whats, ["jobs-ok", "jobs-bad, failed"]);
    assert!(
        got[0].steps.iter().any(|s| s.what.starts_with("git add")),
        "{got:?}"
    );
}

/// A scan's time is all in its parts: after the statuses, finding the
/// submodules (their statuses in it, theirs again within), linking
/// parents, a parent's lines for its submodules' content, and the flags.
#[test]
fn a_scan_holds_what_follows_the_statuses_too() {
    use argus_core::config::Config;
    use argus_core::state::State;
    let sb = Sandbox::new();
    let top = std::fs::canonicalize(sb.nested_submodules()).unwrap();
    let cfg = format!("roots = ['{}']", top.display());
    let mut w = argus_core::Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| {
            if s.kind == "scan" {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    w.scan();
    trace::set_sink(None);
    let got = got.lock().unwrap().clone();
    let [scan] = &got[..] else {
        panic!("one scan in {got:?}");
    };
    let kinds = |s: &Step| s.steps.iter().map(|s| s.kind.clone()).collect::<Vec<_>>();
    assert_eq!(
        kinds(scan),
        [
            "discover",
            "statuses",
            "children",
            "parents",
            "submodule content",
            "flags"
        ],
        "{scan:#?}"
    );
    let children = &scan.steps[2];
    assert_eq!(children.what, "children: 2 found", "{children:?}");
    // mid, found from top; then leaf, found from mid
    let more: Vec<&str> = children.steps.iter().map(|s| s.what.as_str()).collect();
    assert_eq!(
        more,
        [
            "statuses of 1 more (submodules and worktrees of those before)",
            "statuses of 1 more (submodules and worktrees of those before)",
        ],
        "{children:#?}"
    );
}

/// Looking up a repo's submodules is a part of its status (in a parent
/// of thousands it's no small part).
#[test]
fn a_status_says_its_submodules_looked_up() {
    let sb = Sandbox::new();
    let top = std::fs::canonicalize(sb.nested_submodules()).unwrap();
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| {
            if s.kind == "status" {
                into.lock().unwrap().push(s);
            }
        }),
    )));
    let status = argus_core::status::status(&top, Default::default());
    trace::set_sink(None);
    assert!(status.is_ok());
    let got = got.lock().unwrap().clone();
    let [status] = &got[..] else {
        panic!("one status in {got:?}");
    };
    let parts: Vec<&Step> = status
        .steps
        .iter()
        .filter(|s| s.kind == "submodules")
        .collect();
    let [part] = &parts[..] else {
        panic!("a submodules part in {status:#?}");
    };
    assert_eq!(part.what, "submodules: 1 checked out", "{part:?}");
}

/// A step begun holds the steps taken until it ends, said as it was
/// named; one begun without steps kept costs nothing and says nothing.
#[test]
fn a_step_begun_holds_what_follows_until_it_ends() {
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| into.lock().unwrap().push(s)),
    )));
    let begun = trace::begin("startup", || "startup of the test".into());
    trace::part("config", || ());
    trace::part("state", || ());
    begun.end();
    trace::set_sink(None);
    let off = trace::begin("startup", || panic!("not asked to say it"));
    off.end();
    let got = got.lock().unwrap().clone();
    let [startup] = &got[..] else {
        panic!("one step in {got:?}");
    };
    assert_eq!(
        (startup.kind.as_str(), startup.what.as_str()),
        ("startup", "startup of the test")
    );
    assert!(startup.took.is_some());
    let kinds: Vec<&str> = startup.steps.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["config", "state"]);
}

/// An outermost step keeps the process's CPU time over it (all threads'),
/// for how busy the cores were; the steps in it don't (others ran beside
/// them: it wouldn't be theirs), nor one that took none.
#[test]
fn an_outermost_step_keeps_the_cpu_time_over_it() {
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Whole,
        Arc::new(move |s: Step| into.lock().unwrap().push(s)),
    )));
    trace::step(
        "scan",
        || "scan".into(),
        || {
            trace::part("files", || {
                // (busy, not asleep: CPU time to count)
                let until = std::time::Instant::now() + std::time::Duration::from_millis(20);
                let mut n = 0u64;
                while std::time::Instant::now() < until {
                    n = n.wrapping_add(1);
                }
                std::hint::black_box(n);
            })
        },
    );
    // inverse: one that took no steps keeps none, and one inside another
    // none though it took steps; one holding only what's said by time (a
    // job's git commands) took steps
    trace::part("config", || ());
    trace::step(
        "refresh",
        || "refresh".into(),
        || trace::step("status", || "status".into(), || trace::part("files", || ())),
    );
    trace::step(
        "job",
        || "job".into(),
        || {
            trace::took(
                "git",
                || "git fetch".into(),
                std::time::Duration::from_millis(3),
            )
        },
    );
    trace::set_sink(None);
    let got = got.lock().unwrap().clone();
    let [scan, config, refresh, job] = &got[..] else {
        panic!("four steps in {got:?}");
    };
    assert_eq!(config.cpu, None);
    assert!(
        refresh.cpu.is_some() && refresh.steps[0].cpu.is_none(),
        "{refresh:?}"
    );
    assert!(job.cpu.is_some(), "{job:?}");
    let cpu = scan.cpu.expect("the scan's CPU time");
    assert!(cpu >= std::time::Duration::from_millis(15), "{cpu:?}");
    assert!(
        scan.with_split().contains("cores busy"),
        "{}",
        scan.with_split()
    );
    assert_eq!(scan.steps[0].cpu, None, "not the part's");
    // each said by itself, as -v does: the same, and said
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some((
        argus_core::trace::Form::Each,
        Arc::new(move |s: Step| into.lock().unwrap().push(s)),
    )));
    trace::step("scan", || "scan".into(), || trace::part("files", || ()));
    trace::set_sink(None);
    let got = got.lock().unwrap().clone();
    let cpus: Vec<(&str, bool)> = got
        .iter()
        .map(|s| (s.kind.as_str(), s.cpu.is_some()))
        .collect();
    assert_eq!(cpus, [("files", false), ("scan", true)]);
    assert!(got[1].to_string().contains("cores busy)"), "{}", got[1]);
}
