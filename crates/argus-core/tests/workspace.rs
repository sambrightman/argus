//! Workspace: discovery + status + flags + ordering, and sync end-to-end
//! against local bare "forge" repositories.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use argus_core::config::Config;
use argus_core::discover::RepoKind;
use argus_core::forge::{Protocol, RemoteRepo};
use argus_core::identity::RepoId;
use argus_core::ops::Op;
use argus_core::runner::{Event, run_jobs};
use argus_core::state::State;
use argus_core::sync::{plan_tracks, plan_updates};
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{ViewFilter, Workspace};

fn ws(sb: &Sandbox, extra: &str) -> Workspace {
    let cfg = format!("roots = ['{}']\n{extra}", sb.path("dev").display());
    let mut w = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    w.scan();
    w
}

fn names(w: &Workspace, f: ViewFilter) -> Vec<String> {
    w.ordered(f).into_iter().map(|i| w.entries[i].display_name()).collect()
}

const ALL: ViewFilter = ViewFilter { attention_only: false, show_hidden: true };
const DEFAULT: ViewFilter = ViewFilter { attention_only: false, show_hidden: false };
const ATTN: ViewFilter = ViewFilter { attention_only: true, show_hidden: false };

#[test]
fn identities_and_keys() {
    let sb = Sandbox::new();
    sb.remote("acme", "a");
    sb.clone_as_github("acme", "a", "dev/acme/a");
    sb.init("dev/local");
    let w = ws(&sb, "");
    assert_eq!(w.entries.len(), 2);
    let a = w.entries.iter().find(|e| e.path.ends_with("acme/a")).unwrap();
    assert_eq!(a.id, Some(RepoId::new("github.com", "acme", "a")));
    assert_eq!(a.key, "github.com/acme/a");
    assert_eq!(a.display_name(), "acme/a");
    let l = w.entries.iter().find(|e| e.path.ends_with("local")).unwrap();
    assert_eq!(l.id, None);
    assert_eq!(l.key, l.path.display().to_string());
}

#[test]
fn ordering_favourites_then_severity_then_changes_then_name() {
    let sb = Sandbox::new();
    for n in ["clean-b", "clean-a", "dirty1", "dirty3", "fav", "conflict"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty1"), "x", "1");
    for f in ["x", "y", "z"] {
        write(&sb.path("dev/dirty3"), f, "1");
    }
    let c = sb.path("dev/conflict");
    git(&c, &["switch", "-q", "-c", "o"]);
    write(&c, "README", "o");
    commit(&c, "o");
    git(&c, &["switch", "-q", "main"]);
    write(&c, "README", "m");
    commit(&c, "m");
    let _ = std::process::Command::new("git").current_dir(&c).args(["merge", "o"]).output();

    let w = ws(&sb, "[[rule]]\nmatch='acme/fav'\nfavourite=true");
    assert_eq!(
        names(&w, DEFAULT),
        vec!["acme/fav", "acme/conflict", "acme/dirty3", "acme/dirty1", "acme/clean-a", "acme/clean-b"]
    );
    assert_eq!(names(&w, ATTN), vec!["acme/conflict", "acme/dirty3", "acme/dirty1"], "clean favourite is not attention");
}

#[test]
fn excluded_and_snoozed_hidden_by_default() {
    let sb = Sandbox::new();
    for n in ["keep", "gone", "nap"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    let mut w = ws(&sb, "[[rule]]\nmatch='acme/gone'\nexclude=true");
    w.state.snoozed.insert("github.com/acme/nap".into(), "2999-01-01T00:00:00Z".parse().unwrap());
    w.resolve_flags();
    assert_eq!(names(&w, DEFAULT), vec!["acme/keep"]);
    assert_eq!(names(&w, ALL).len(), 3);
}

#[test]
fn worktrees_and_submodules_become_children() {
    let sb = Sandbox::new();
    let lib = sb.init("libsrc");
    sb.remote("acme", "app");
    let app = sb.clone_as_github("acme", "app", "dev/app");
    git(&app, &["-c", "protocol.file.allow=always", "submodule", "add", "-q", lib.to_str().unwrap(), "lib"]);
    commit(&app, "sub");
    // worktree outside any root: must still be found via its main repo
    let wt = sb.path("elsewhere/app-wt");
    git(&app, &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()]);
    let w = ws(&sb, "");
    let order = w.ordered(ALL);
    let kinds: Vec<_> = order.iter().map(|&i| (w.entries[i].kind, w.depth(i))).collect();
    assert_eq!(kinds, vec![(RepoKind::Main, 0), (RepoKind::Worktree, 1), (RepoKind::Submodule, 1)]);
    for &i in &order[1..] {
        assert_eq!(w.entries[i].parent.as_deref(), Some(app.as_path()));
    }
}

#[test]
fn hidden_parent_shown_when_child_needs_attention() {
    // inverse of hiding: a visible child keeps its parent row for context
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    let app = sb.clone_as_github("acme", "app", "dev/app");
    let wt = sb.path("dev/app-wt");
    git(&app, &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()]);
    write(&wt, "dirty", "x");
    let w = ws(&sb, "");
    let n = names(&w, ATTN);
    assert_eq!(n.len(), 2, "{n:?}");
    assert_eq!(n[0], "acme/app");
}

#[test]
fn errors_are_entries_not_failures() {
    let sb = Sandbox::new();
    let broken = sb.path("dev/broken");
    std::fs::create_dir_all(broken.join(".git")).unwrap(); // not a valid git dir
    sb.init("dev/ok");
    let w = ws(&sb, "");
    let b = w.entries.iter().find(|e| e.path == broken).unwrap();
    assert!(b.error().is_some());
    assert!(b.needs_attention());
    assert_eq!(names(&w, ATTN)[0], b.display_name());
}

#[test]
fn sync_pulls_only_syncable_visible_repos() {
    let sb = Sandbox::new();
    for n in ["s1", "s2", "nosync", "excl"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
        sb.advance_remote("acme", n, "new");
    }
    sb.init("dev/local");
    let w = ws(
        &sb,
        "[[rule]]\nmatch='acme/*'\nsync=true\n[[rule]]\nmatch='acme/nosync'\nsync=false\n\
         [[rule]]\nmatch='acme/excl'\nexclude=true\n[[rule]]\nmatch='**/local'\nsync=true",
    );
    let jobs = plan_updates(&w);
    let mut labels: Vec<_> = jobs.iter().map(|j| j.label.clone()).collect();
    labels.sort();
    assert_eq!(labels, vec!["pull acme/s1", "pull acme/s2"], "no remote => nothing to pull");

    let events = Mutex::new(vec![]);
    let res = run_jobs(&jobs, 4, &AtomicBool::new(false), &|e| events.lock().unwrap().push(e));
    assert!(res.iter().all(|r| r.as_ref().is_some_and(|o| o.success)), "{res:?}");
    assert_eq!(events.lock().unwrap().iter().filter(|e| matches!(e, Event::Finished { .. })).count(), 2);
    assert!(sb.path("dev/s1/new").exists());
    assert!(!sb.path("dev/nosync/new").exists());
    assert!(!sb.path("dev/excl/new").exists());
}

#[test]
fn sync_uses_fetch_when_pull_is_impossible() {
    let sb = Sandbox::new();
    sb.remote("acme", "d");
    let r = sb.clone_as_github("acme", "d", "dev/d");
    git(&r, &["checkout", "-q", "--detach"]);
    let w = ws(&sb, "[[rule]]\nmatch='acme/*'\nsync=true");
    assert_eq!(plan_updates(&w)[0].op, Op::Fetch);
}

#[test]
fn pull_honours_user_config_autostash() {
    // We don't implement integration; git + the user's settings do.
    let sb = Sandbox::new();
    sb.remote("acme", "r");
    let r = sb.clone_as_github("acme", "r", "dev/r");
    sb.advance_remote("acme", "r", "remote-file");
    write(&r, "README", "local edit\n");
    git(&r, &["config", "pull.rebase", "true"]);
    git(&r, &["config", "rebase.autoStash", "true"]);
    let o = argus_core::ops::run(&r, &Op::Pull);
    assert!(o.success, "{}", o.output);
    assert!(r.join("remote-file").exists());
    assert_eq!(std::fs::read_to_string(r.join("README")).unwrap(), "local edit\n");

    // inverse: without autostash a dirty rebase-pull is refused by git, and we report it
    sb.advance_remote("acme", "r", "remote-2");
    git(&r, &["config", "rebase.autoStash", "false"]);
    git(&r, &["add", "README"]);
    let o = argus_core::ops::run(&r, &Op::Pull);
    assert!(!o.success);
    assert!(!r.join("remote-2").exists());
}

fn listing(sb: &Sandbox, owner: &str, names: &[(&str, bool, bool)]) -> Vec<RemoteRepo> {
    names
        .iter()
        .map(|(n, archived, fork)| {
            let url = sb.path(&format!("forge/{owner}/{n}.git")).display().to_string();
            RemoteRepo {
                id: RepoId::new("github.com", owner, *n),
                ssh_url: url.clone(),
                https_url: url,
                archived: *archived,
                fork: *fork,
                default_branch: Some("main".into()),
                description: None,
            }
        })
        .collect()
}

#[test]
fn track_clones_missing_and_reports_orphans() {
    let sb = Sandbox::new();
    for n in ["have", "missing", "old", "forked", "secret"] {
        sb.remote("acme", n);
    }
    sb.clone_as_github("acme", "have", "dev/acme/have");
    sb.remote("acme", "deleted");
    sb.clone_as_github("acme", "deleted", "dev/acme/deleted");
    let w = ws(
        &sb,
        &format!(
            "[[track]]\nowner='acme'\ninclude_forks=false\ninto='{}'\n[[rule]]\nmatch='acme/secret'\nexclude=true",
            sb.path("dev/acme").display()
        ),
    );
    let mut l = BTreeMap::new();
    l.insert(
        "github.com/acme".to_string(),
        listing(
            &sb,
            "acme",
            &[("have", false, false), ("missing", false, false), ("old", true, false), ("forked", false, true), ("secret", false, false)],
        ),
    );
    let plan = plan_tracks(&w, &l, Protocol::Https);
    let clones: Vec<_> = plan.clones().map(|j| j.label.clone()).collect();
    assert_eq!(clones, vec!["clone acme/missing"], "archived, forks, excluded and existing skipped");
    assert_eq!(plan.orphans, vec![sb.path("dev/acme/deleted")]);
    assert!(plan.missing.is_empty());

    let res = run_jobs(&plan.jobs, 2, &AtomicBool::new(false), &|_| {});
    assert!(res[0].as_ref().unwrap().success, "{:?}", res[0]);
    assert!(sb.path("dev/acme/missing/README").exists());
}

#[test]
fn track_notify_mode_and_occupied_destination() {
    let sb = Sandbox::new();
    sb.remote("acme", "a");
    sb.remote("acme", "b");
    std::fs::create_dir_all(sb.path("dev/acme/b")).unwrap(); // something already there
    let into = sb.path("dev/acme").display().to_string();
    let l = BTreeMap::from([("github.com/acme".to_string(), listing(&sb, "acme", &[("a", false, false), ("b", false, false)]))]);

    let w = ws(&sb, &format!("[[track]]\nowner='acme'\nclone='notify'\ninto='{into}'"));
    let plan = plan_tracks(&w, &l, Protocol::Https);
    assert_eq!(plan.jobs.len(), 0);
    assert_eq!(plan.missing.len(), 2);

    let w = ws(&sb, &format!("[[track]]\nowner='acme'\ninto='{into}'"));
    let plan = plan_tracks(&w, &l, Protocol::Https);
    assert_eq!(plan.jobs.len(), 1, "a only");
    assert_eq!(plan.missing.iter().map(|r| r.id.name.as_str()).collect::<Vec<_>>(), vec!["b"]);
}

#[test]
fn track_without_listing_does_nothing() {
    // e.g. the forge API failed: must not report every local repo as orphaned
    let sb = Sandbox::new();
    sb.remote("acme", "a");
    sb.clone_as_github("acme", "a", "dev/a");
    let w = ws(&sb, "[[track]]\nowner='acme'");
    let plan = plan_tracks(&w, &BTreeMap::new(), Protocol::Https);
    assert!(plan.jobs.is_empty() && plan.orphans.is_empty() && plan.missing.is_empty());
}

#[test]
fn runner_serialises_a_family_and_stops_it_on_failure() {
    let sb = Sandbox::new();
    sb.remote("acme", "a");
    let a = sb.clone_as_github("acme", "a", "dev/a");
    let jobs = vec![
        argus_core::sync::Job { path: sb.path("nowhere"), op: Op::Fetch, family: a.clone(), label: "bad".into() },
        argus_core::sync::Job { path: a.clone(), op: Op::Fetch, family: a.clone(), label: "never".into() },
    ];
    let res = run_jobs(&jobs, 8, &AtomicBool::new(false), &|_| {});
    assert!(!res[0].as_ref().unwrap().success);
    assert!(res[1].is_none(), "second job in a failed family is skipped");
}

#[test]
fn runner_cancel_prevents_new_jobs() {
    let sb = Sandbox::new();
    let a = sb.init("dev/a");
    let jobs = vec![argus_core::sync::Job { path: a.clone(), op: Op::Fetch, family: a, label: "x".into() }];
    let res = run_jobs(&jobs, 1, &AtomicBool::new(true), &|_| {});
    assert!(res[0].is_none());
}
