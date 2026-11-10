//! Pulling safely: conflict prediction for diverged branches, never leaving
//! a repo mid-merge/rebase, and never waiting on an editor.

use std::path::Path;

use argus_core::config::StashConfig;
use argus_core::fix::{Fix, FixKind, apply_fix, chain, choose, plan, simulate};
use argus_core::ops::{self, Op};
use argus_core::status::{InProgress, Integration, PullBy, RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};

fn st(r: &Path) -> RepoStatus {
    status(r, StatusOptions::default()).unwrap()
}

fn pull_fix(r: &Path) -> Option<Fix> {
    plan(&st(r), &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::Pull)
}

fn head(r: &Path) -> String {
    git(r, &["rev-parse", "HEAD"])
}

#[test]
fn predicts_a_clean_merge() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", false);
    git(&r, &["config", "pull.rebase", "false"]);
    assert_eq!(st(&r).integration, Some(Integration::Clean(PullBy::Merge)));
    let f = pull_fix(&r).unwrap();
    assert_eq!(f.blocked, None);
    assert!(
        f.reason.contains("merges cleanly, adding a merge commit"),
        "{}",
        f.reason
    );
    // inverse: a rebase adds no commit
    git(&r, &["config", "pull.rebase", "true"]);
    assert_eq!(st(&r).integration, Some(Integration::Clean(PullBy::Rebase)));
    let f = pull_fix(&r).unwrap();
    assert!(f.reason.contains("rebases cleanly"), "{}", f.reason);
    assert!(!f.reason.contains("merge commit"), "{}", f.reason);
}

#[test]
fn the_branch_setting_decides_merge_or_rebase_over_pull_rebase() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", false);
    let by = |r: &Path| match st(r).integration {
        Some(Integration::Clean(by)) => by,
        other => panic!("{other:?}"),
    };
    for (pull, branch, expect) in [
        ("true", "false", PullBy::Merge),
        ("false", "true", PullBy::Rebase),
        ("false", "merges", PullBy::Rebase),
        ("false", "interactive", PullBy::Rebase),
    ] {
        git(&r, &["config", "pull.rebase", pull]);
        git(&r, &["config", "branch.feat.rebase", branch]);
        assert_eq!(
            by(&r),
            expect,
            "pull.rebase={pull} branch.feat.rebase={branch}"
        );
    }
    git(&r, &["config", "--unset", "branch.feat.rebase"]);
    for (pull, expect) in [
        ("merges", PullBy::Rebase),
        ("true", PullBy::Rebase),
        ("false", PullBy::Merge),
    ] {
        git(&r, &["config", "pull.rebase", pull]);
        assert_eq!(by(&r), expect, "pull.rebase={pull}");
    }
    git(&r, &["config", "--unset", "pull.rebase"]);
    git(&r, &["config", "pull.ff", "true"]);
    assert_eq!(by(&r), PullBy::Merge, "pull.ff alone merges");
}

#[test]
fn predicted_counts_after_a_pull_match_reality() {
    for (rebase, ahead) in [("false", 2), ("true", 1)] {
        let sb = Sandbox::new();
        let r = sb.diverged("acme", "w", "w", false);
        git(&r, &["config", "pull.rebase", rebase]);
        let before = st(&r);
        let predicted = simulate(&before, &pull_fix(&r).unwrap());
        assert!(ops::run(&r, &Op::Pull).success);
        let actual = st(&r);
        assert_eq!(actual.ahead(), ahead, "pull.rebase={rebase}");
        assert_eq!(predicted.ahead(), actual.ahead(), "pull.rebase={rebase}");
        assert_eq!(predicted.behind(), actual.behind(), "pull.rebase={rebase}");
        let head = |s: &RepoStatus| {
            let b = s.branches.iter().find(|b| b.is_head).unwrap();
            let t = b.upstream.as_ref().unwrap();
            (b.unpushed, t.ahead, t.behind)
        };
        assert_eq!(head(&predicted), head(&actual), "pull.rebase={rebase}");
    }
}

#[test]
fn no_prediction_unless_diverged() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    assert_eq!(st(&r).integration, None, "up to date");
    sb.advance_remote("acme", "w", "new");
    git(&r, &["fetch", "-q"]);
    assert_eq!(st(&r).integration, None, "behind only: a fast-forward");
    assert_eq!(pull_fix(&r).unwrap().blocked, None);
}

#[test]
fn predicts_conflicting_paths_and_never_auto_pulls_them() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", true);
    git(&r, &["config", "pull.rebase", "false"]);
    let s = st(&r);
    assert_eq!(
        s.integration,
        Some(Integration::Conflicts(vec!["README".into()]))
    );
    let f = pull_fix(&r).unwrap();
    assert_eq!(f.blocked.as_deref(), Some("would conflict in README"));
    assert!(choose(&s, &[FixKind::Pull], &StashConfig::default()).is_none());
    assert!(chain(&s, &[FixKind::Pull], &StashConfig::default()).is_empty());
    let before = head(&r);
    assert!(
        apply_fix(&r, &[FixKind::Pull], &StashConfig::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(head(&r), before);
}

#[test]
fn diverged_without_pull_config_is_refused_as_git_would() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", false);
    assert!(
        matches!(st(&r).integration, Some(Integration::Refused(_))),
        "{:?}",
        st(&r).integration
    );
    let f = pull_fix(&r).unwrap();
    assert!(
        f.blocked.as_deref().unwrap().contains("pull.rebase"),
        "{f:?}"
    );
    // inverse: any of these settles merge vs rebase
    for (k, v) in [
        ("pull.rebase", "true"),
        ("pull.rebase", "false"),
        ("branch.feat.rebase", "true"),
        ("pull.ff", "true"),
    ] {
        git(&r, &["config", k, v]);
        assert!(
            matches!(st(&r).integration, Some(Integration::Clean(_))),
            "{k}={v}"
        );
        git(&r, &["config", "--unset", k]);
    }
    git(&r, &["config", "pull.ff", "only"]);
    assert!(matches!(st(&r).integration, Some(Integration::Refused(_))));
}

#[test]
fn dirty_tree_blocks_the_pull_fix() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    sb.advance_remote("acme", "w", "new");
    git(&r, &["fetch", "-q"]);
    write(&r, "README", "dirty");
    assert_eq!(
        pull_fix(&r).unwrap().blocked.as_deref(),
        Some("uncommitted changes")
    );
    assert!(
        apply_fix(&r, &[FixKind::Pull], &StashConfig::default())
            .unwrap()
            .is_none()
    );
}

fn assert_chain_matches_reality(r: &Path, allowed: &[FixKind], expect: &[FixKind]) {
    let predicted: Vec<_> = chain(&st(r), allowed, &StashConfig::default())
        .into_iter()
        .map(|f| f.kind)
        .collect();
    assert_eq!(predicted, expect, "predicted chain");
    let mut actual = vec![];
    while let Some(a) = apply_fix(r, allowed, &StashConfig::default()).unwrap() {
        assert!(a.outcome.success, "{:?}: {}", a.fix.kind, a.outcome.output);
        actual.push(a.fix.kind);
        assert!(actual.len() <= 10, "runaway: {actual:?}");
    }
    assert_eq!(actual, expect, "actual successive auto-fixes");
}

#[test]
fn chain_pull_push_switch_by_merge_and_by_rebase() {
    use FixKind::*;
    for rebase in ["false", "true"] {
        let sb = Sandbox::new();
        let r = sb.diverged("acme", "w", "w", false);
        git(&r, &["config", "pull.rebase", rebase]);
        assert_chain_matches_reality(&r, &FixKind::ALL, &[Pull, Push, SwitchDefault]);
    }
}

fn in_progress(r: &Path) -> Option<InProgress> {
    st(r).in_progress
}

#[test]
fn a_conflicting_pull_is_aborted_leaving_the_repo_as_found() {
    for rebase in ["false", "true"] {
        let sb = Sandbox::new();
        let r = sb.diverged("acme", "w", "w", true);
        git(&r, &["config", "pull.rebase", rebase]);
        let before = head(&r);
        let o = ops::run(&r, &Op::Pull);
        assert!(!o.success, "pull.rebase={rebase}");
        assert!(o.output.contains("aborted"), "{}", o.output);
        assert_eq!(in_progress(&r), None, "pull.rebase={rebase}");
        assert_eq!(head(&r), before);
        assert_eq!(git(&r, &["status", "--porcelain"]), "");
    }
}

#[test]
fn an_aborted_rebase_restores_autostashed_changes() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", true);
    git(&r, &["config", "pull.rebase", "true"]);
    git(&r, &["config", "rebase.autoStash", "true"]);
    write(&r, "notes.txt", "my edit\n");
    let o = ops::run(&r, &Op::Pull);
    assert!(!o.success);
    assert_eq!(in_progress(&r), None);
    assert_eq!(
        std::fs::read_to_string(r.join("notes.txt")).unwrap(),
        "my edit\n"
    );
    assert_eq!(
        git(&r, &["stash", "list"]),
        "",
        "autostash re-applied, not left behind"
    );
}

#[test]
fn pull_never_starts_or_aborts_during_the_users_own_merge() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", true);
    git(&r, &["config", "pull.rebase", "false"]);
    let m = std::process::Command::new("git")
        .current_dir(&r)
        .args(["merge", "-q", "@{u}"])
        .envs(argus_core::testkit::subprocess_env(&sb.root()))
        .output()
        .unwrap();
    assert!(!m.status.success(), "set-up: the merge conflicts");
    assert_eq!(in_progress(&r), Some(InProgress::Merge));
    let o = ops::run(&r, &Op::Pull);
    assert!(!o.success);
    assert!(o.output.contains("in progress"), "{}", o.output);
    assert_eq!(
        in_progress(&r),
        Some(InProgress::Merge),
        "the user's merge is untouched"
    );
}

#[test]
fn a_merging_pull_never_waits_for_an_editor() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "w", false);
    git(&r, &["config", "pull.rebase", "false"]);
    let marker = sb.path("editor-ran");
    git(
        &r,
        &[
            "config",
            "core.editor",
            &format!("touch '{}'; false", marker.display()),
        ],
    );
    let o = ops::run(&r, &Op::Pull);
    assert!(o.success, "{}", o.output);
    assert!(!marker.exists(), "an editor was launched");
    let parents = git(&r, &["rev-list", "--parents", "-n1", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 3, "a merge commit");
    commit(&r, "still usable");
}

/// Each way a successful pull can re-apply an autostash: fast-forward
/// (merge or rebase settings) and a real rebase of a diverged branch.
fn autostash_cases(sb: &Sandbox) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut v = vec![];
    for (name, rebase) in [("ff-merge", false), ("ff-rebase", true)] {
        sb.remote("acme", name);
        let r = sb.clone_as_github("acme", name, name);
        sb.advance_remote("acme", name, "README");
        git(&r, &["fetch", "-q"]);
        git(&r, &["config", "pull.rebase", &rebase.to_string()]);
        v.push((name, r));
    }
    let r = sb.diverged("acme", "div", "div", false);
    // README changed on origin/feat too
    let other = sb.clone_as_github("acme", "div", "div-other");
    git(&other, &["switch", "-q", "feat"]);
    write(&other, "README", "theirs\n");
    commit(&other, "theirs README");
    git(&other, &["push", "-q"]);
    git(&r, &["fetch", "-q"]);
    git(&r, &["config", "pull.rebase", "true"]);
    v.push(("diverged-rebase", r));
    for (_, r) in &v {
        git(r, &["config", "rebase.autoStash", "true"]);
        git(r, &["config", "merge.autoStash", "true"]);
    }
    v
}

#[test]
fn a_pull_whose_autostash_conflicts_is_undone() {
    let sb = Sandbox::new();
    for (name, r) in autostash_cases(&sb) {
        // a stash of the user's own, which must stay as it is
        write(&r, "notes.txt", "kept\n");
        git(&r, &["add", "notes.txt"]);
        git(&r, &["stash", "push", "-q", "-m", "mine"]);
        let mine = git(&r, &["rev-parse", "refs/stash"]);
        let before = head(&r);
        write(&r, "README", "staged edit\n");
        git(&r, &["add", "README"]);
        write(&r, "README", "unstaged edit\n");
        let out = ops::run(&r, &Op::Pull);
        assert!(!out.success, "{name}: {}", out.output);
        assert!(
            out.output
                .contains("conflicts with your uncommitted changes in README; undone"),
            "{name}: {}",
            out.output
        );
        // as it was: same commit, same changes (staged ones staged), same stashes
        assert_eq!(head(&r), before, "{name}");
        assert_eq!(
            std::fs::read_to_string(r.join("README")).unwrap(),
            "unstaged edit\n",
            "{name}"
        );
        assert_eq!(git(&r, &["show", ":README"]), "staged edit\n", "{name}");
        let s = st(&r);
        assert_eq!(s.changes.conflicted, 0, "{name}");
        let codes: Vec<String> = s
            .changes
            .files
            .iter()
            .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
            .collect();
        assert_eq!(codes, ["MM README"], "{name}");
        assert_eq!(s.stashes, ["mine"], "{name}");
        assert_eq!(git(&r, &["rev-parse", "refs/stash"]), mine, "{name}");
    }
}

#[test]
fn a_pull_whose_autostash_applies_cleanly_stands() {
    let sb = Sandbox::new();
    for (name, r) in autostash_cases(&sb) {
        // one git left behind after an earlier pull: not this pull's
        write(&r, "README", "old edit\n");
        let old = git(&r, &["stash", "create"]);
        git(&r, &["stash", "store", "-m", "autostash", old.trim()]);
        git(&r, &["checkout", "-q", "README"]);
        let before = head(&r);
        write(&r, "elsewhere.txt", "x\n");
        git(&r, &["add", "elsewhere.txt"]);
        let out = ops::run(&r, &Op::Pull);
        assert!(out.success, "{name}: {}", out.output);
        assert_ne!(head(&r), before, "{name}: pulled");
        let s = st(&r);
        assert_eq!(s.stashes, ["autostash"], "{name}: the old one, untouched");
        assert_eq!(s.changes.staged, 1, "{name}: the change is back");
    }
}

#[test]
fn only_the_pulls_own_autostash_is_undone() {
    // a hook stashing during the pull: not git's autostash, left alone
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    sb.advance_remote("acme", "w", "README");
    let hook = r.join(".git/hooks/post-merge");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho x > hooked.txt && git add hooked.txt && git stash push -q -m hooked\n",
    )
    .unwrap();
    let mut p = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    std::fs::set_permissions(&hook, p).unwrap();
    let before = head(&r);
    let out = ops::run(&r, &Op::Pull);
    assert!(out.success, "{}", out.output);
    assert_ne!(head(&r), before, "pulled");
    assert_eq!(st(&r).stashes, ["hooked"]);
}

/// `owner/app` with `owner/lib` as a submodule at `lib`, cloned (with the
/// submodule checked out) at `rel`; then a new `lib` commit is pushed and
/// `app` moved to it upstream. Returns the clone and the new `lib` commit.
fn submodule_bumped(sb: &Sandbox, rel: &str) -> (std::path::PathBuf, String) {
    let lib = sb.remote("acme", "lib");
    let app = sb.remote("acme", "app");
    let seed = sb.path(&format!("scratch/{rel}-seed"));
    git(
        &sb.root(),
        &["clone", "-q", app.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git(
        &seed,
        &["submodule", "add", "-q", lib.to_str().unwrap(), "lib"],
    );
    commit(&seed, "add lib");
    git(&seed, &["push", "-q"]);
    let r = sb.path(rel);
    git(
        &sb.root(),
        &[
            "clone",
            "-q",
            "--recurse-submodules",
            app.to_str().unwrap(),
            r.to_str().unwrap(),
        ],
    );
    sb.advance_remote("acme", "lib", "new");
    git(&seed, &["submodule", "update", "-q", "--remote", "lib"]);
    commit(&seed, "bump lib");
    git(&seed, &["push", "-q"]);
    let new = git(&seed.join("lib"), &["rev-parse", "HEAD"]);
    (r, new)
}

#[test]
fn a_pull_checks_out_the_submodules_it_moves() {
    for rebase in ["false", "true"] {
        let sb = Sandbox::new();
        let (r, new) = submodule_bumped(&sb, "app");
        git(&r, &["config", "pull.rebase", rebase]);
        let old = git(&r.join("lib"), &["rev-parse", "HEAD"]);
        assert_ne!(old, new);
        let out = ops::run(&r, &Op::Pull);
        assert!(out.success, "pull.rebase={rebase}: {}", out.output);
        assert_eq!(
            git(&r.join("lib"), &["rev-parse", "HEAD"]),
            new,
            "pull.rebase={rebase}"
        );
        assert!(st(&r).changes.is_clean(), "pull.rebase={rebase}");
    }
}

#[test]
fn a_clone_checks_out_submodules() {
    let sb = Sandbox::new();
    let (_, new) = submodule_bumped(&sb, "app");
    let dest = sb.path("fresh");
    let out = ops::run(
        &sb.root(),
        &Op::Clone {
            url: sb.path("forge/acme/app.git").display().to_string(),
            dest: dest.clone(),
        },
    );
    assert!(out.success, "{}", out.output);
    assert_eq!(git(&dest.join("lib"), &["rev-parse", "HEAD"]), new);
    assert!(st(&dest).changes.is_clean());
}

#[test]
fn undoing_a_pull_puts_its_submodules_back() {
    for rebase in ["false", "true"] {
        let sb = Sandbox::new();
        let (r, _) = submodule_bumped(&sb, "app");
        sb.advance_remote("acme", "app", "README");
        git(&r, &["config", "pull.rebase", rebase]);
        git(&r, &["config", "rebase.autoStash", "true"]);
        git(&r, &["config", "merge.autoStash", "true"]);
        let (before, old) = (head(&r), git(&r.join("lib"), &["rev-parse", "HEAD"]));
        write(&r, "README", "mine\n");
        let out = ops::run(&r, &Op::Pull);
        assert!(!out.success, "pull.rebase={rebase}: {}", out.output);
        assert!(out.output.contains("undone"), "{}", out.output);
        assert_eq!(head(&r), before, "pull.rebase={rebase}");
        assert_eq!(
            git(&r.join("lib"), &["rev-parse", "HEAD"]),
            old,
            "pull.rebase={rebase}"
        );
        let codes: Vec<String> = st(&r)
            .changes
            .files
            .iter()
            .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
            .collect();
        assert_eq!(codes, [" M README"], "pull.rebase={rebase}");
    }
}

/// A pull of `r` that moves its submodules and then conflicts with the
/// autostash; returns the outcome.
fn conflicting_pull(sb: &Sandbox, r: &Path) -> ops::OpOutcome {
    sb.advance_remote("acme", "app", "README");
    git(r, &["config", "merge.autoStash", "true"]);
    git(r, &["config", "pull.rebase", "false"]);
    write(r, "README", "mine\n");
    ops::run(r, &Op::Pull)
}

#[test]
fn undoing_a_pull_puts_nested_submodules_back() {
    let sb = Sandbox::new();
    let url = |name: &str| {
        sb.path(&format!("forge/acme/{name}.git"))
            .display()
            .to_string()
    };
    let clone = |name: &str, rel: &str| {
        let dest = sb.path(rel);
        git(
            &sb.root(),
            &[
                "clone",
                "-q",
                "--recurse-submodules",
                &url(name),
                dest.to_str().unwrap(),
            ],
        );
        dest
    };
    for name in ["inner", "lib", "app"] {
        sb.remote("acme", name);
    }
    // app has lib at `lib`, which has inner at `inner`
    let lib_seed = clone("lib", "scratch/lib-seed");
    git(
        &lib_seed,
        &["submodule", "add", "-q", &url("inner"), "inner"],
    );
    commit(&lib_seed, "add inner");
    git(&lib_seed, &["push", "-q"]);
    let app_seed = clone("app", "scratch/app-seed");
    git(&app_seed, &["submodule", "add", "-q", &url("lib"), "lib"]);
    git(
        &app_seed,
        &["submodule", "update", "-q", "--init", "--recursive"],
    );
    commit(&app_seed, "add lib");
    git(&app_seed, &["push", "-q"]);
    let r = clone("app", "app");
    let nested = r.join("lib/inner");
    let old = git(&nested, &["rev-parse", "HEAD"]);
    // upstream, inner moves in lib, and lib (with it) in app
    sb.advance_remote("acme", "inner", "new");
    git(
        &lib_seed,
        &["submodule", "update", "-q", "--remote", "inner"],
    );
    commit(&lib_seed, "bump inner");
    git(&lib_seed, &["push", "-q"]);
    git(
        &app_seed,
        &[
            "submodule",
            "update",
            "-q",
            "--remote",
            "--recursive",
            "lib",
        ],
    );
    commit(&app_seed, "bump lib");
    git(&app_seed, &["push", "-q"]);
    let out = conflicting_pull(&sb, &r);
    assert!(out.output.contains("; undone"), "{}", out.output);
    assert_eq!(git(&nested, &["rev-parse", "HEAD"]), old);
    assert_eq!(git(&r.join("lib"), &["status", "--porcelain"]), "");
}

#[test]
fn undoing_a_pull_leaves_submodules_that_arent_checked_out() {
    let sb = Sandbox::new();
    let (r, _) = submodule_bumped(&sb, "app");
    git(&r, &["submodule", "deinit", "-q", "lib"]);
    // or the clone's `submodule.active=.` keeps it active: git checks it out again
    git(&r, &["config", "--unset", "submodule.active"]);
    let out = conflicting_pull(&sb, &r);
    assert!(out.output.contains("; undone"), "{}", out.output);
    assert!(!r.join("lib/README").exists());
}

#[test]
fn switching_checks_out_the_branchs_submodules() {
    let sb = Sandbox::new();
    let (r, new) = submodule_bumped(&sb, "app");
    git(&r, &["fetch", "-q"]);
    git(&r, &["branch", "-q", "bumped", "origin/main"]);
    let lib = r.join("lib");
    let old = git(&lib, &["rev-parse", "HEAD"]);
    let switch = |b: &str| {
        let out = ops::run(&r, &Op::Switch { branch: b.into() });
        assert!(out.success, "{}", out.output);
    };
    switch("bumped");
    assert_eq!(git(&lib, &["rev-parse", "HEAD"]), new);
    assert!(st(&r).changes.is_clean());
    switch("main");
    assert_eq!(git(&lib, &["rev-parse", "HEAD"]), old);
    assert!(st(&r).changes.is_clean());
}

#[test]
fn switching_never_overwrites_a_submodules_changes() {
    let sb = Sandbox::new();
    let (r, _) = submodule_bumped(&sb, "app");
    git(&r, &["fetch", "-q"]);
    git(&r, &["branch", "-q", "bumped", "origin/main"]);
    let lib = r.join("lib");
    let old = git(&lib, &["rev-parse", "HEAD"]);
    // the bumped commit adds `new`, which the switch would overwrite
    write(&lib, "new", "mine\n");
    let out = ops::run(
        &r,
        &Op::Switch {
            branch: "bumped".into(),
        },
    );
    assert!(!out.success, "{}", out.output);
    assert!(
        out.output
            .contains("switched, but checking out the submodules failed"),
        "{}",
        out.output
    );
    assert_eq!(std::fs::read_to_string(lib.join("new")).unwrap(), "mine\n");
    assert_eq!(git(&lib, &["rev-parse", "HEAD"]), old);
    assert_eq!(git(&r, &["branch", "--show-current"]).trim(), "bumped");
}

#[test]
fn a_pull_never_overwrites_a_submodules_changes() {
    // the bumped commit adds `new` (in the way) and leaves README (carried)
    for (file, in_the_way) in [("new", true), ("README", false)] {
        let sb = Sandbox::new();
        let (r, new) = submodule_bumped(&sb, "app");
        let lib = r.join("lib");
        let old = git(&lib, &["rev-parse", "HEAD"]);
        write(&lib, file, "mine\n");
        let out = ops::run(&r, &Op::Pull);
        assert_eq!(out.success, !in_the_way, "{file}: {}", out.output);
        assert_eq!(std::fs::read_to_string(lib.join(file)).unwrap(), "mine\n");
        let at = if in_the_way { old } else { new };
        assert_eq!(git(&lib, &["rev-parse", "HEAD"]), at, "{file}");
    }
}

#[test]
fn a_failed_switch_leaves_submodules_where_they_are() {
    let sb = Sandbox::new();
    let (r, new) = submodule_bumped(&sb, "app");
    let lib = r.join("lib");
    // the user's own choice of commit, not the one recorded
    git(&lib, &["fetch", "-q"]);
    git(&lib, &["checkout", "-q", new.trim()]);
    let out = ops::run(
        &r,
        &Op::Switch {
            branch: "nope".into(),
        },
    );
    assert!(!out.success, "{}", out.output);
    assert!(!out.output.contains("switched"), "{}", out.output);
    assert_eq!(git(&lib, &["rev-parse", "HEAD"]), new);
}
