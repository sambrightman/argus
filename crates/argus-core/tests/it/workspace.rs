//! Workspace: discovery + status + flags + ordering, and pull end-to-end
//! against local bare "forge" repositories.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use argus_core::attention::Issue;
use argus_core::config::Config;
use argus_core::discover::RepoKind;
use argus_core::forge::{Protocol, RemoteRepo};
use argus_core::identity::RepoId;
use argus_core::ops::Op;
use argus_core::paths::contract_tilde;
use argus_core::pull::{plan_tracks, plan_updates};
use argus_core::runner::{Event, run_jobs};
use argus_core::state::State;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{ViewFilter, Workspace};

fn ws(sb: &Sandbox, extra: &str) -> Workspace {
    let cfg = format!("roots = ['{}']\n{extra}", sb.path("dev").display());
    let mut w = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    w.scan();
    w
}

fn names(w: &Workspace, f: ViewFilter) -> Vec<String> {
    w.ordered(f)
        .into_iter()
        .map(|i| w.entries[i].display_name())
        .collect()
}

const ALL: ViewFilter = ViewFilter {
    attention_only: false,
    show_hidden: true,
};
const DEFAULT: ViewFilter = ViewFilter {
    attention_only: false,
    show_hidden: false,
};
const ATTN: ViewFilter = ViewFilter {
    attention_only: true,
    show_hidden: false,
};

#[test]
fn identities_and_keys() {
    let sb = Sandbox::new();
    sb.remote("acme", "a");
    sb.clone_as_github("acme", "a", "dev/acme/a");
    sb.init("dev/local");
    let w = ws(&sb, "");
    assert_eq!(w.entries.len(), 2);
    let a = w
        .entries
        .iter()
        .find(|e| e.path.ends_with("acme/a"))
        .unwrap();
    assert_eq!(a.id, Some(RepoId::new("github.com", "acme", "a")));
    assert_eq!(a.key, "github.com/acme/a");
    assert_eq!(a.display_name(), "acme/a");
    let l = w
        .entries
        .iter()
        .find(|e| e.path.ends_with("local"))
        .unwrap();
    assert_eq!(l.id, None);
    assert_eq!(l.key, l.path.display().to_string());
}

#[test]
/// Unclean before clean; within each, favourites first, then severity,
/// amount of change and name.
fn ordering_attention_then_favourites_then_severity_then_changes_then_name() {
    let sb = Sandbox::new();
    for n in [
        "clean-b",
        "clean-a",
        "dirty1",
        "dirty3",
        "fav",
        "fav-dirty",
        "conflict",
    ] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty1"), "x", "1");
    write(&sb.path("dev/fav-dirty"), "x", "1");
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
    assert!(!argus_core::testkit::git_may_fail(&c, &["merge", "o"]));

    let w = ws(&sb, "favourite = ['acme/fav*']");
    assert_eq!(
        names(&w, DEFAULT),
        vec![
            "acme/fav-dirty",
            "acme/conflict",
            "acme/dirty3",
            "acme/dirty1",
            "acme/fav",
            "acme/clean-a",
            "acme/clean-b"
        ]
    );
    assert_eq!(
        names(&w, ATTN),
        vec![
            "acme/fav-dirty",
            "acme/conflict",
            "acme/dirty3",
            "acme/dirty1"
        ],
        "clean favourite is not attention"
    );
}

#[test]
fn ignored_and_snoozed_hidden_by_default() {
    let sb = Sandbox::new();
    for n in ["keep", "gone", "nap"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    let mut w = ws(&sb, "ignore.repos = ['acme/gone']");
    w.state.snoozed.insert(
        "github.com/acme/nap".into(),
        "2999-01-01T00:00:00Z".parse().unwrap(),
    );
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
    git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "lib",
        ],
    );
    commit(&app, "sub");
    // worktree outside any root: must still be found via its main repo
    let wt = sb.path("elsewhere/app-wt");
    git(
        &app,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    let w = ws(&sb, "");
    let order = w.ordered(ALL);
    let kinds: Vec<_> = order
        .iter()
        .map(|&i| (w.entries[i].kind, w.depth(i)))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (RepoKind::Main, 0),
            (RepoKind::Worktree, 1),
            (RepoKind::Submodule, 1)
        ]
    );
    for &i in &order[1..] {
        assert_eq!(w.entries[i].parent.as_deref(), Some(app.as_path()));
    }
    // children are named compactly: relative to the parent, else by directory name
    let names: Vec<_> = order[1..]
        .iter()
        .map(|&i| w.entries[i].display_name())
        .collect();
    assert_eq!(names, vec!["app-wt", "lib"]);
    // what a change in a child may change the status of
    let lib = app.join("lib");
    assert_eq!(w.with_parents(&lib), [lib.clone(), app.clone()]);
    assert_eq!(w.with_parents(&app), std::slice::from_ref(&app));
    // a loop of parents, should one ever be recorded, is cut
    let mut w = w;
    let i = w.index_of(&app).unwrap();
    w.entries[i].parent = Some(lib.clone());
    assert_eq!(w.with_parents(&lib), [lib, app]);
}

#[test]
fn hidden_parent_shown_when_child_needs_attention() {
    // inverse of hiding: a visible child keeps its parent row for context
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    let app = sb.clone_as_github("acme", "app", "dev/app");
    let wt = sb.path("dev/app-wt");
    git(
        &app,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
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
    // the entry shows its path; the error doesn't repeat it
    let e = b.error().unwrap();
    assert!(!e.contains(&*broken.to_string_lossy()), "{e}");
    assert!(e.contains("does not appear to be a git repository"), "{e}");
    assert!(!e.starts_with([' ', '"', ':']), "{e:?}");
}

#[test]
fn pull_pulls_only_visible_repos_marked_for_pull() {
    let sb = Sandbox::new();
    for n in ["s1", "s2", "nopull", "excl"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
        sb.advance_remote("acme", n, "new");
    }
    sb.init("dev/local");
    let w = ws(
        &sb,
        "pull = ['acme/*', '!acme/nopull', '**/local']\nignore.repos = ['acme/excl']",
    );
    let jobs = plan_updates(&w);
    let mut labels: Vec<_> = jobs.iter().map(|j| j.label.clone()).collect();
    labels.sort();
    assert_eq!(
        labels,
        vec!["pull acme/s1", "pull acme/s2"],
        "no remote => nothing to pull"
    );

    let events = Mutex::new(vec![]);
    let res = run_jobs(&jobs, 4, &AtomicBool::new(false), &|e| {
        events.lock().unwrap().push(e)
    });
    assert!(
        res.iter().all(|r| r.as_ref().is_some_and(|o| o.success)),
        "{res:?}"
    );
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::Finished { .. }))
            .count(),
        2
    );
    assert!(sb.path("dev/s1/new").exists());
    assert!(!sb.path("dev/nopull/new").exists());
    assert!(!sb.path("dev/excl/new").exists());
}

#[test]
fn pull_uses_fetch_when_pull_is_impossible() {
    let sb = Sandbox::new();
    sb.remote("acme", "d");
    let r = sb.clone_as_github("acme", "d", "dev/d");
    git(&r, &["checkout", "-q", "--detach"]);
    let w = ws(&sb, "pull = ['acme/*']");
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
    assert_eq!(
        std::fs::read_to_string(r.join("README")).unwrap(),
        "local edit\n"
    );

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
            let url = sb
                .path(&format!("forge/{owner}/{n}.git"))
                .display()
                .to_string();
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
            "ignore.repos = ['acme/secret']\n[[track]]\nowner='acme'\ninclude_forks=false\ninto='{}'",
            sb.path("dev/acme").display()
        ),
    );
    let mut l = BTreeMap::new();
    l.insert(
        "github.com/acme".to_string(),
        listing(
            &sb,
            "acme",
            &[
                ("have", false, false),
                ("missing", false, false),
                ("old", true, false),
                ("forked", false, true),
                ("secret", false, false),
            ],
        ),
    );
    let plan = plan_tracks(&w, &l, Protocol::Https);
    let clones: Vec<_> = plan.clones().map(|j| j.label.clone()).collect();
    assert_eq!(
        clones,
        vec!["clone acme/missing"],
        "archived, forks, ignored and existing skipped"
    );
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
    let l = BTreeMap::from([(
        "github.com/acme".to_string(),
        listing(&sb, "acme", &[("a", false, false), ("b", false, false)]),
    )]);

    let w = ws(
        &sb,
        &format!("[[track]]\nowner='acme'\nclone='notify'\ninto='{into}'"),
    );
    let plan = plan_tracks(&w, &l, Protocol::Https);
    assert_eq!(plan.jobs.len(), 0);
    assert_eq!(plan.missing.len(), 2);

    let w = ws(&sb, &format!("[[track]]\nowner='acme'\ninto='{into}'"));
    let plan = plan_tracks(&w, &l, Protocol::Https);
    assert_eq!(plan.jobs.len(), 1, "a only");
    assert_eq!(
        plan.missing
            .iter()
            .map(|r| r.id.name.as_str())
            .collect::<Vec<_>>(),
        vec!["b"]
    );
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
        argus_core::pull::Job {
            path: sb.path("nowhere"),
            op: Op::Fetch,
            family: a.clone(),
            label: "bad".into(),
        },
        argus_core::pull::Job {
            path: a.clone(),
            op: Op::Fetch,
            family: a.clone(),
            label: "never".into(),
        },
    ];
    let res = run_jobs(&jobs, 8, &AtomicBool::new(false), &|_| {});
    assert!(!res[0].as_ref().unwrap().success);
    assert!(res[1].is_none(), "second job in a failed family is skipped");
}

#[test]
fn runner_cancel_prevents_new_jobs() {
    let sb = Sandbox::new();
    let a = sb.init("dev/a");
    let jobs = vec![argus_core::pull::Job {
        path: a.clone(),
        op: Op::Fetch,
        family: a,
        label: "x".into(),
    }];
    let res = run_jobs(&jobs, 1, &AtomicBool::new(true), &|_| {});
    assert!(res[0].is_none());
}

struct FakeForge(Vec<RemoteRepo>, bool);

impl argus_core::forge::Forge for FakeForge {
    fn kind(&self) -> &str {
        "fake"
    }
    fn host(&self) -> &str {
        "github.com"
    }
    fn list(
        &self,
        _owner: &str,
        _team: Option<&str>,
    ) -> Result<Vec<RemoteRepo>, argus_core::forge::ForgeError> {
        if self.1 {
            Err(argus_core::forge::ForgeError::Auth("nope".into()))
        } else {
            Ok(self.0.clone())
        }
    }
}

#[test]
fn full_track_pull_flags_new_clones_and_team_membership() {
    let sb = Sandbox::new();
    sb.remote("acme", "fresh");
    sb.remote("acme", "have");
    sb.clone_as_github("acme", "have", "dev/acme/have");
    let into = sb.path("dev/acme").display().to_string();
    let mut w = ws(
        &sb,
        &format!("[[track]]\nowner='acme'\nteam='core'\ninto='{into}'"),
    );
    let have = w.entries[0].path.clone();
    assert!(
        !w.get(&have).unwrap().flags.pull,
        "team membership unknown before listing"
    );

    let list = listing(
        &sb,
        "acme",
        &[("fresh", false, false), ("have", false, false)],
    );
    let factory = move |_: &str,
                        _: &str|
          -> Result<
        Box<dyn argus_core::forge::Forge>,
        argus_core::forge::ForgeError,
    > { Ok(Box::new(FakeForge(list.clone(), false))) };
    let listings = argus_core::pull::list_tracks(&mut w, &factory);
    assert!(listings.errors.is_empty());
    assert!(w.get(&have).unwrap().flags.pull, "membership now known");
    assert!(w.state.tracks["github.com/acme#core"].listed_at.is_some());

    let plan = plan_tracks(&w, &listings.repos, listings.protocol);
    let res = run_jobs(&plan.jobs, 2, &AtomicBool::new(false), &|_| {});
    let added = argus_core::pull::record_clones(&mut w, &plan.jobs, &res);
    assert_eq!(added, vec![sb.path("dev/acme/fresh")]);
    let fresh = w.get(&added[0]).unwrap();
    assert!(fresh.flags.new, "fresh clones are highlighted");
    assert!(fresh.needs_attention());
    // the clone came from a local path, so its key is its path; either way it's in state.new
    assert!(w.state.new.contains(&fresh.key));
    assert!(
        !w.get(&have).unwrap().flags.new,
        "existing clones are not new"
    );
}

#[test]
fn failed_listing_is_reported_and_keeps_old_membership() {
    let sb = Sandbox::new();
    let mut w = ws(&sb, "[[track]]\nowner='acme'\nteam='core'");
    w.state
        .tracks
        .entry("github.com/acme#core".into())
        .or_default()
        .repos
        .insert("github.com/acme/x".into());
    let factory = |_: &str,
                   _: &str|
     -> Result<Box<dyn argus_core::forge::Forge>, argus_core::forge::ForgeError> {
        Ok(Box::new(FakeForge(vec![], true)))
    };
    let l = argus_core::pull::list_tracks(&mut w, &factory);
    assert_eq!(l.errors.len(), 1);
    assert!(l.errors[0].1.contains("nope"));
    assert_eq!(
        w.state.tracks["github.com/acme#core"].repos.len(),
        1,
        "not wiped by a failure"
    );
}

#[test]
fn pull_fetches_instead_when_a_pull_would_not_be_clean() {
    let sb = Sandbox::new();
    for (n, conflict) in [("clean", false), ("conflict", true), ("busy", true)] {
        let r = sb.diverged("acme", n, &format!("dev/{n}"), conflict);
        git(&r, &["config", "pull.rebase", "false"]);
    }
    let _ = std::process::Command::new("git")
        .current_dir(sb.path("dev/busy"))
        .args(["merge", "-q", "@{u}"])
        .envs(argus_core::testkit::subprocess_env(&sb.root()))
        .output()
        .unwrap();
    let w = ws(&sb, "pull = ['acme/*']");
    let mut labels: Vec<_> = plan_updates(&w).into_iter().map(|j| j.label).collect();
    labels.sort();
    assert_eq!(
        labels,
        ["fetch acme/busy", "fetch acme/conflict", "pull acme/clean"]
    );
}

#[test]
fn marking_edits_the_config_lists() {
    use argus_core::rules::Mark;
    let sb = Sandbox::new();
    for n in ["app", "lib", "big"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    sb.init("dev/local");
    let mut w = ws(&sb, "pull = ['acme/*']");
    let at = |w: &Workspace, name: &str| {
        w.entries
            .iter()
            .position(|e| e.display_name().ends_with(name))
            .unwrap()
    };
    let (app, lib, big, local) = (at(&w, "app"), at(&w, "lib"), at(&w, "big"), at(&w, "local"));
    // on: the repo's identity is added, and flags follow
    w.set_mark(&[app, lib], Mark::Favourite, true).unwrap();
    assert_eq!(
        w.config.favourite,
        ["github.com/acme/app", "github.com/acme/lib"]
    );
    assert!(w.entries[app].flags.favourite && !w.entries[big].flags.favourite);
    // a repo without an identity is named by its path
    w.set_mark(&[local], Mark::Ignore, true).unwrap();
    assert_eq!(
        w.config.ignore.repos,
        [contract_tilde(&w.entries[local].path)]
    );
    assert!(w.entries[local].flags.ignored);
    // off against a pattern: a `!` entry
    w.set_mark(&[big], Mark::Pull, false).unwrap();
    assert_eq!(w.config.pull, ["acme/*", "!github.com/acme/big"]);
    assert!(!w.entries[big].flags.pull && w.entries[app].flags.pull);
    // marked: repos with entries of their own (patterns don't count)
    assert_eq!(w.marked_count(&[app, lib, big, local]), 4);
    assert_eq!(w.marked_count(&[app]), 1);
    // clearing: own entries go, patterns decide again
    w.clear_marks(&[app, big]).unwrap();
    assert_eq!(w.config.favourite, ["github.com/acme/lib"]);
    assert_eq!(w.config.pull, ["acme/*"]);
    assert!(w.entries[big].flags.pull && !w.entries[app].flags.favourite);
    assert_eq!(w.marked_count(&[app, big]), 0);
}

#[test]
fn unmarking_a_tracked_repo_for_pull_writes_a_bang() {
    use argus_core::rules::Mark;
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    sb.clone_as_github("acme", "app", "dev/app");
    let mut w = ws(&sb, "[[track]]\nowner='acme'");
    assert!(w.entries[0].flags.pull, "tracked: pulled by default");
    w.set_mark(&[0], Mark::Pull, false).unwrap();
    assert_eq!(w.config.pull, ["!github.com/acme/app"]);
    assert!(!w.entries[0].flags.pull);
    w.set_mark(&[0], Mark::Pull, true).unwrap();
    assert!(w.config.pull.is_empty() && w.entries[0].flags.pull);
}

#[test]
fn marks_of_missing_repos_stay_in_config() {
    let sb = Sandbox::new();
    sb.init("dev/app");
    let mut w = ws(&sb, "favourite = ['/elsewhere/gone', 'acme/*']");
    w.scan();
    std::fs::remove_dir_all(sb.path("dev/app")).unwrap();
    w.scan();
    assert!(w.entries.is_empty());
    assert_eq!(w.config.favourite, ["/elsewhere/gone", "acme/*"]);
}

#[test]
fn skip_matches_names_anywhere_and_paths_with_a_slash() {
    let sb = Sandbox::new();
    for r in [
        "keep",
        "archive/old",
        "archive2/x",
        "a/vendored/v",
        "deep/er/vendored/w",
        "py/lib.egg-info/r",
        "sub/rel/r",
        "sub/relx/r",
    ] {
        sb.init(&format!("dev/{r}"));
    }
    let dev = sb.path("dev").display().to_string();
    let found = |extra: &str| -> Vec<String> {
        let w = ws(&sb, extra);
        let real = std::fs::canonicalize(sb.path("dev")).unwrap();
        let mut v: Vec<String> = w
            .entries
            .iter()
            .map(|e| e.path.strip_prefix(&real).unwrap().display().to_string())
            .collect();
        v.sort();
        v
    };
    let all = found("exclude_paths = []");
    assert_eq!(all.len(), 8, "{all:?}");
    // no slash: a directory name (globs allowed) at any depth
    let v = found("exclude_paths = ['vendored', '*.egg-info']");
    assert!(
        !v.iter()
            .any(|p| p.contains("vendored") || p.contains("egg-info")),
        "{v:?}"
    );
    assert_eq!(v.len(), 5, "{v:?}");
    // a trailing slash still means a name
    assert_eq!(found("exclude_paths = ['vendored/']").len(), 6);
    // absolute (or ~) paths, globs allowed; siblings sharing a prefix stay
    let v = found(&format!(
        "exclude_paths = ['{dev}/archive', '{dev}/*/vendored']"
    ));
    assert!(
        v.contains(&"archive2/x".into()) && !v.contains(&"archive/old".into()),
        "{v:?}"
    );
    assert!(!v.contains(&"a/vendored/v".into()), "{v:?}");
    assert!(
        v.contains(&"deep/er/vendored/w".into()),
        "* is one level: {v:?}"
    );
    // relative with a slash: below each root
    let v = found("exclude_paths = ['sub/rel']");
    assert!(
        !v.contains(&"sub/rel/r".into()) && v.contains(&"sub/relx/r".into()),
        "{v:?}"
    );
    // patterns through a symlink match the real paths scans walk
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(sb.path("dev"), sb.path("link")).unwrap();
        let link = sb.path("link").display().to_string();
        let v = found(&format!("exclude_paths = ['{link}/archive']"));
        assert!(!v.contains(&"archive/old".into()), "{v:?}");
    }
    // roots are always scanned, as with hidden directories
    for skip in [format!("'{dev}/archive'"), "'archive'".into()] {
        let cfg = format!("roots = ['{dev}/archive']\nexclude_paths = [{skip}]");
        let mut w = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
        w.scan();
        assert_eq!(w.entries.len(), 1, "{skip}");
    }
    // bad patterns are config errors
    let cfg = "exclude_paths = ['~/dev/[oops']";
    let Err(err) = Workspace::new(Config::parse(cfg).unwrap(), State::default()) else {
        panic!("bad pattern accepted");
    };
    assert!(err.to_string().contains("[oops"), "{err}");
}

#[test]
fn default_skip_includes_the_go_module_cache_below_each_root() {
    let sb = Sandbox::new();
    sb.init("dev/go/pkg/mod/github.com/x/y");
    sb.init("dev/go/src/mine");
    sb.init("dev/other/go/pkg/r");
    let config = Config {
        roots: vec![sb.path("dev")],
        ..Config::default()
    };
    let mut w = Workspace::new(config, State::default()).unwrap();
    w.scan();
    let mut v: Vec<String> = w.entries.iter().map(|e| e.display_name()).collect();
    v.sort();
    assert!(!v.iter().any(|n| n.contains("go/pkg/mod")), "{v:?}");
    assert!(v.iter().any(|n| n.ends_with("go/src/mine")), "{v:?}");
    // only directly below a root, as relative patterns are
    assert!(v.iter().any(|n| n.ends_with("other/go/pkg/r")), "{v:?}");
}

#[test]
fn status_runs_on_a_pool_of_the_given_size() {
    use argus_core::workspace::in_pool;
    assert_eq!(in_pool(2, rayon::current_num_threads), 2);
    assert_eq!(in_pool(3, rayon::current_num_threads), 3);
}

#[test]
fn an_ignored_issue_stops_counting_and_a_pin_to_another_value_doesnt() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    let entry = |extra: &str| ws(&sb, extra).entries[0].clone();
    let e = entry("");
    assert!(e.issues.contains(&Issue::NonDefaultBranch) && e.needs_attention());
    let e = entry("[ignore]\nnon_default_branch = ['acme/w@feat']");
    assert!(
        !e.issues.contains(&Issue::NonDefaultBranch),
        "{:?}",
        e.issues
    );
    assert_eq!(e.ignored_issues, [Issue::NonDefaultBranch]);
    assert!(!e.needs_attention());
    assert!(!e.flags.hidden(), "still shown: only the issue is ignored");
    // pinned to another branch: this one is flagged
    let e = entry("[ignore]\nnon_default_branch = ['acme/w@release']");
    assert!(e.issues.contains(&Issue::NonDefaultBranch) && e.ignored_issues.is_empty());
}

#[test]
fn fix_kinds_leave_out_those_undoing_an_ignored_state() {
    use argus_core::fix::FixKind;
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    let kinds = [FixKind::SwitchDefault, FixKind::Push];
    let e = ws(&sb, "[ignore]\nnon_default_branch = ['acme/w']").entries[0].clone();
    assert_eq!(e.fix_kinds(&kinds), [FixKind::Push]);
    let e = ws(&sb, "").entries[0].clone();
    assert_eq!(e.fix_kinds(&kinds), kinds);
}

#[test]
fn ignoring_an_issue_through_the_workspace_pins_it_and_resolves_it() {
    use argus_core::rules::IssueList;
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    let mut w = ws(&sb, "");
    w.set_issue_ignored(&[0], IssueList::NonDefaultBranch, true, None)
        .unwrap();
    assert_eq!(
        w.config.ignore.non_default_branch,
        ["github.com/acme/w@feat"]
    );
    assert_eq!(w.entries[0].ignored_issues, [Issue::NonDefaultBranch]);
    w.set_issue_ignored(&[0], IssueList::NonDefaultBranch, false, None)
        .unwrap();
    assert!(w.config.ignore.non_default_branch.is_empty());
    assert!(w.entries[0].issues.contains(&Issue::NonDefaultBranch));
}

#[test]
fn detached_values_are_its_tags_describe_and_commit() {
    use argus_core::rules::IssueList;
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    git(&r, &["tag", "v1"]);
    git(&r, &["switch", "-q", "--detach"]);
    let id = git(&r, &["rev-parse", "HEAD"]);
    let id = id.trim();
    let w = ws(&sb, "");
    let v = IssueList::Detached.values(w.entries[0].status().unwrap());
    assert_eq!(v, ["v1".to_string(), id[..7].to_string(), id.to_string()]);
    // past the tag: the describe instead
    commit(&r, "after");
    git(&r, &["switch", "-q", "--detach"]);
    let w = ws(&sb, "");
    let s = w.entries[0].status().unwrap();
    let v = IssueList::Detached.values(s);
    assert_eq!(v[0], s.describe.clone().unwrap());
    // inverse: on a branch there's nothing to pin
    git(&r, &["switch", "-q", "main"]);
    let w = ws(&sb, "");
    assert!(
        IssueList::Detached
            .values(w.entries[0].status().unwrap())
            .is_empty()
    );
}

#[test]
fn the_configured_describe_limit_reaches_status() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    git(&r, &["tag", "v1"]);
    commit(&r, "a");
    commit(&r, "b");
    git(&r, &["switch", "-q", "--detach"]);
    let s = |cfg: &str| ws(&sb, cfg).entries[0].status().unwrap().clone();
    assert!(s("describe_limit = 1").describe_gave_up);
    assert!(s("describe_limit = 2").describe.is_some());
}

#[test]
fn refreshing_one_repo_rereads_its_status() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    let mut w = ws(&sb, "");
    let dirty = |w: &Workspace| {
        w.entries[0]
            .issues
            .iter()
            .any(|i| matches!(i, Issue::Dirty(1)))
    };
    assert!(!dirty(&w));
    write(&r, "README", "changed\n");
    w.refresh(&r);
    assert!(dirty(&w), "{:?}", w.entries[0].issues);
}

fn kept_ws(sb: &Sandbox) -> Workspace {
    let cfg = format!("roots = ['{}']\n", sb.path("dev").display());
    let mut w = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    w.keep_repos_open();
    w.scan();
    w
}

fn url_of(w: &Workspace, p: &std::path::Path) -> Option<String> {
    w.get(p)
        .and_then(|e| e.status())
        .and_then(|s| s.remote_url.clone())
}

#[test]
fn a_workspace_reads_afresh_unless_told_to_keep_repos_open() {
    let sb = Sandbox::new();
    sb.init("dev/a");
    let mut w = ws(&sb, "");
    w.refresh_all();
    assert_eq!(w.kept_opens(), None);
    let mut w = kept_ws(&sb);
    w.refresh_all();
    assert_eq!(w.kept_opens(), Some(1), "scan and refresh_all share it");
    git(
        &sb.path("dev/a"),
        &["remote", "add", "origin", "https://a/x"],
    );
    w.refresh(&sb.path("dev/a"));
    assert_eq!(w.kept_opens(), Some(2), "refresh goes through it too");
}

#[test]
fn a_workspace_keeping_repos_open_still_sees_config_and_work_change() {
    let sb = Sandbox::new();
    let a = sb.init("dev/a");
    let mut w = kept_ws(&sb);
    assert_eq!(url_of(&w, &a), None);
    git(&a, &["remote", "add", "origin", "https://a/x"]);
    write(&a, "new", "1");
    w.refresh_all();
    assert_eq!(url_of(&w, &a).as_deref(), Some("https://a/x"));
    assert_eq!(w.get(&a).unwrap().status().unwrap().changes.total(), 1);
}

#[test]
fn clones_of_a_workspace_share_its_kept_repos() {
    let sb = Sandbox::new();
    let a = sb.init("dev/a");
    let w = kept_ws(&sb);
    w.clone().status_reader().read(&a).unwrap();
    assert_eq!(w.kept_opens(), Some(1));
}

#[test]
fn a_rescan_forgets_kept_repos_that_are_gone() {
    let sb = Sandbox::new();
    let (a, b) = (sb.init("dev/a"), sb.init("dev/b"));
    let mut w = kept_ws(&sb);
    std::fs::rename(&b, sb.path("elsewhere")).unwrap();
    w.scan();
    assert_eq!(w.kept_opens(), Some(2));
    std::fs::rename(sb.path("elsewhere"), &b).unwrap();
    w.status_reader().read(&b).unwrap();
    assert_eq!(w.kept_opens(), Some(3), "b was forgotten, so reopened");
    w.status_reader().read(&a).unwrap();
    assert_eq!(w.kept_opens(), Some(3), "a still kept");
}

/// Stash a change to README with `message` (git's own without one).
fn stash(r: &std::path::Path, message: Option<&str>) {
    write(r, "README", &format!("{message:?}\n"));
    match message {
        Some(m) => git(r, &["stash", "push", "-q", "-m", m]),
        None => git(r, &["stash", "push", "-q"]),
    };
}

fn stash_issues(e: &argus_core::workspace::Entry) -> (Vec<Issue>, Vec<Issue>) {
    let only = |v: &[Issue]| -> Vec<Issue> {
        v.iter()
            .filter(|i| matches!(i, Issue::Stashes(_)))
            .copied()
            .collect()
    };
    (only(&e.issues), only(&e.ignored_issues))
}

#[test]
fn own_stashes_are_ignored_by_default_and_argus_stashes_are_not() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    stash(&r, Some("experiment"));
    stash(&r, None);
    let e = ws(&sb, "").entries[0].clone();
    assert_eq!(stash_issues(&e), (vec![], vec![Issue::Stashes(2)]));
    assert!(!e.needs_attention(), "{:?}", e.issues);
    stash(&r, Some("argus 2026-11-08 12:00"));
    let e = ws(&sb, "").entries[0].clone();
    assert_eq!(
        stash_issues(&e),
        (vec![Issue::Stashes(1)], vec![Issue::Stashes(2)])
    );
    assert!(e.needs_attention());
    // inverse: an empty list ignores none
    let e = ws(&sb, "[ignore]\nstashes = []").entries[0].clone();
    assert_eq!(stash_issues(&e), (vec![Issue::Stashes(3)], vec![]));
}

#[test]
fn a_pinned_stash_ignore_covers_only_matching_messages() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    stash(&r, Some("experiment"));
    stash(&r, Some("mail me@host"));
    let issues =
        |list: &str| stash_issues(&ws(&sb, &format!("[ignore]\nstashes = {list}")).entries[0]);
    assert_eq!(
        issues("['acme/w@experiment']"),
        (vec![Issue::Stashes(1)], vec![Issue::Stashes(1)])
    );
    assert_eq!(
        issues("['acme/w@exp*', 'acme/w@mail me@host']"),
        (vec![], vec![Issue::Stashes(2)]),
        "a glob, and an @ in the message"
    );
    assert_eq!(
        issues("['acme/w', '!acme/w@mail*']"),
        (vec![Issue::Stashes(1)], vec![Issue::Stashes(1)]),
        "the last matching entry decides, stash by stash"
    );
    assert_eq!(
        issues("['acme/w@other']"),
        (vec![Issue::Stashes(2)], vec![]),
        "pinned to another message"
    );
}

#[test]
fn ignoring_stashes_is_unpinned_unless_asked_and_pins_each_message_literally() {
    use argus_core::rules::IssueList;
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    stash(&r, Some("fix [x] *"));
    stash(&r, Some("experiment"));
    // all already ignored (the default covers both): nothing to add
    let mut w = ws(&sb, "");
    let before = w.config.ignore.stashes.clone();
    w.set_issue_ignored(&[0], IssueList::Stashes, true, Some(true))
        .unwrap();
    assert_eq!(w.config.ignore.stashes, before);
    // one stash already covered by a pattern's pin: ignoring adds the rest
    let mut w = ws(&sb, "[ignore]\nstashes = ['acme/*@experiment']");
    w.set_issue_ignored(&[0], IssueList::Stashes, true, None)
        .unwrap();
    assert_eq!(
        w.config.ignore.stashes,
        ["acme/*@experiment", "github.com/acme/w"]
    );
    let mut w = ws(&sb, "[ignore]\nstashes = []");
    w.set_issue_ignored(&[0], IssueList::Stashes, true, None)
        .unwrap();
    assert_eq!(w.config.ignore.stashes, ["github.com/acme/w"]);
    assert_eq!(
        stash_issues(&w.entries[0]),
        (vec![], vec![Issue::Stashes(2)])
    );
    w.set_issue_ignored(&[0], IssueList::Stashes, false, None)
        .unwrap();
    assert!(w.config.ignore.stashes.is_empty());
    w.set_issue_ignored(&[0], IssueList::Stashes, true, Some(true))
        .unwrap();
    assert_eq!(
        w.config.ignore.stashes.len(),
        2,
        "{:?}",
        w.config.ignore.stashes
    );
    assert_eq!(
        stash_issues(&w.entries[0]),
        (vec![], vec![Issue::Stashes(2)])
    );
    // a new stash isn't covered, not even one the pinned glob characters
    // would have matched
    stash(&r, Some("fix x anything"));
    let cfg = toml::to_string(&w.config.ignore).unwrap();
    let e = ws(&sb, &format!("[ignore]\n{cfg}")).entries[0].clone();
    assert_eq!(
        stash_issues(&e),
        (vec![Issue::Stashes(1)], vec![Issue::Stashes(2)])
    );
}

#[test]
fn ignoring_a_branch_is_pinned_unless_asked_not_to() {
    use argus_core::rules::IssueList;
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    let mut w = ws(&sb, "");
    w.set_issue_ignored(&[0], IssueList::NonDefaultBranch, true, Some(false))
        .unwrap();
    assert_eq!(w.config.ignore.non_default_branch, ["github.com/acme/w"]);
    w.set_issue_ignored(&[0], IssueList::NonDefaultBranch, false, None)
        .unwrap();
    w.set_issue_ignored(&[0], IssueList::NonDefaultBranch, true, None)
        .unwrap();
    assert_eq!(
        w.config.ignore.non_default_branch,
        ["github.com/acme/w@feat"]
    );
}

#[test]
fn pinning_a_detached_head_writes_one_of_its_names() {
    use argus_core::rules::IssueList;
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    git(&r, &["tag", "v1"]);
    git(&r, &["switch", "-q", "--detach"]);
    let mut w = ws(&sb, "");
    w.set_issue_ignored(&[0], IssueList::Detached, true, None)
        .unwrap();
    assert_eq!(w.config.ignore.detached, ["github.com/acme/w@v1"]);
}

#[test]
fn a_bare_clone_is_known_by_its_identity_and_a_worktree_by_its_path() {
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    let app = sb.clone_as_github("acme", "app", "dev/app");
    let forge = format!("{}/", sb.path("forge").display());
    let rewrite = format!("url.{forge}.insteadOf=https://github.com/");
    let bare = sb.path("dev/lib.git");
    sb.remote("acme", "lib");
    git(
        &sb.root(),
        &[
            "-c",
            &rewrite,
            "clone",
            "-q",
            "--bare",
            "https://github.com/acme/lib.git",
            bare.to_str().unwrap(),
        ],
    );
    let wt = sb.path("dev/app-wt");
    git(
        &app,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    let w = ws(&sb, "");
    let key = |p: &std::path::Path| w.get(&p.canonicalize().unwrap()).unwrap().key.clone();
    assert_eq!(key(&bare), "github.com/acme/lib");
    assert_eq!(key(&app), "github.com/acme/app");
    assert_ne!(key(&wt), key(&app), "a worktree has a key of its own");
}

#[test]
fn several_worktrees_outside_the_roots_are_all_children() {
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    let app = sb.clone_as_github("acme", "app", "dev/app");
    for name in ["wt1", "wt2"] {
        let wt = sb.path(&format!("elsewhere/{name}"));
        git(
            &app,
            &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()],
        );
    }
    let w = ws(&sb, "");
    let n = names(&w, ALL);
    assert_eq!(n, ["acme/app", "wt1", "wt2"]);
}

#[test]
fn depth_stops_at_a_cycle() {
    let sb = Sandbox::new();
    sb.init("dev/a");
    sb.init("dev/b");
    let mut w = ws(&sb, "");
    let (a, b) = (w.entries[0].path.clone(), w.entries[1].path.clone());
    w.entries[0].parent = Some(b);
    w.entries[1].parent = Some(a);
    assert_eq!(w.depth(0), 16);
}

#[test]
fn a_repo_can_be_picked_by_its_name_or_identity() {
    let sb = Sandbox::new();
    sb.remote("acme", "app");
    sb.clone_as_github("acme", "app", "dev/app");
    sb.init("dev/other");
    let w = ws(&sb, "");
    for p in ["acme/app", "github.com/acme/app", "ACME/*"] {
        let picked: Vec<_> = w
            .select(&[p.into()], ALL)
            .into_iter()
            .map(|i| w.entries[i].display_name())
            .collect();
        assert_eq!(picked, ["acme/app"], "{p}");
    }
}

#[test]
fn a_bare_repo_is_fetched_even_with_an_upstream() {
    let sb = Sandbox::new();
    sb.remote("acme", "lib");
    let forge = format!("{}/", sb.path("forge").display());
    let rewrite = format!("url.{forge}.insteadOf=https://github.com/");
    let bare = sb.path("dev/lib.git");
    git(
        &sb.root(),
        &[
            "-c",
            &rewrite,
            "clone",
            "-q",
            "--bare",
            "--config",
            &rewrite,
            "https://github.com/acme/lib.git",
            bare.to_str().unwrap(),
        ],
    );
    git(
        &bare,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(&bare, &["fetch", "-q"]);
    git(&bare, &["config", "branch.main.remote", "origin"]);
    git(&bare, &["config", "branch.main.merge", "refs/heads/main"]);
    let w = ws(&sb, "pull = ['acme/*']");
    let s = w.entries[0].status().unwrap();
    assert!(s.bare && s.upstream.is_some(), "{s:?}");
    assert_eq!(plan_updates(&w)[0].op, Op::Fetch);
}

#[test]
fn only_successful_clones_are_recorded() {
    use argus_core::ops::OpOutcome;
    use argus_core::pull::Job;
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    let mut w = ws(&sb, "");
    let job = |op| Job {
        label: String::new(),
        path: r.clone(),
        op,
        family: r.clone(),
    };
    let outcome = |success| {
        Some(OpOutcome {
            success,
            code: Some(0),
            output: String::new(),
            duration: std::time::Duration::ZERO,
        })
    };
    let clone = Op::Clone {
        url: String::new(),
        dest: r.clone(),
    };
    let added = argus_core::pull::record_clones(
        &mut w,
        &[job(clone), job(Op::Pull)],
        &[outcome(false), outcome(true)],
    );
    assert!(added.is_empty(), "{added:?}");
    assert!(w.state.new.is_empty());
}

#[test]
fn clones_of_a_directory_forge_are_known_by_its_identity() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let from = sb.path("forge/acme/w.git");
    let to = sb.path("dev/w");
    git(
        &sb.root(),
        &["clone", "-q", from.to_str().unwrap(), to.to_str().unwrap()],
    );
    sb.init("dev/local");
    let host = sb.path("forge").display().to_string();
    let id = format!("{}/acme/w", host.to_lowercase());
    let tracked = |forge: &str| {
        let w = ws(
            &sb,
            &format!("[[track]]\nforge = '{forge}'\nhost = '{host}'\nowner = 'acme'"),
        );
        let key = w.get(&to.canonicalize().unwrap()).unwrap().key.clone();
        (key, w.repo_names())
    };
    let path = |p: &str| sb.path(p).canonicalize().unwrap().display().to_string();
    let (key, names) = tracked("dir");
    assert_eq!(key, id);
    assert_eq!(names, [path("dev/local"), "acme/w".into()]);
    // inverse: the same directory as another kind of forge is just a path
    let (key, names) = tracked("github");
    assert_ne!(key, id);
    assert_eq!(names, [path("dev/local"), path("dev/w")]);
}

#[test]
fn repos_inside_work_trees_are_found_only_when_asked() {
    let sb = Sandbox::new();
    let lib = sb.init("libsrc");
    commit(&lib, "l");
    let app = sb.init("dev/app");
    commit(&app, "a");
    git(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "lib",
        ],
    );
    commit(&app, "sub");
    let nested = sb.init("dev/app/tools/nested");
    commit(&nested, "n");
    let rows = |extra: &str| {
        let w = ws(&sb, extra);
        w.ordered(ALL)
            .into_iter()
            .map(|i| (w.entries[i].display_name(), w.entries[i].kind, w.depth(i)))
            .collect::<Vec<_>>()
    };
    let app_name = contract_tilde(&app.canonicalize().unwrap());
    let nested_name = contract_tilde(&nested.canonicalize().unwrap());
    // inverse: by default the submodule is learnt from its parent, the
    // nested repo is never seen
    assert_eq!(
        rows(""),
        [
            (app_name.clone(), RepoKind::Main, 0),
            ("lib".into(), RepoKind::Submodule, 1),
        ]
    );
    // the submodule found by the walk is still its parent's child, once
    assert_eq!(
        rows("scan_inside_repos = true"),
        [
            (app_name, RepoKind::Main, 0),
            ("lib".into(), RepoKind::Submodule, 1),
            (nested_name, RepoKind::Main, 0),
        ]
    );
}
