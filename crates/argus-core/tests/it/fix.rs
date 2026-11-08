//! Fix planning and application. Each fix has tests for when it must *not*
//! be offered, since an unsafe fix is far worse than a missing one.

use argus_core::config::StashConfig;
use argus_core::fix::{FixKind, apply_fix, chain, choose, plan, ranked};
use argus_core::ops::Op;
use argus_core::status::{Head, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};

fn kinds(p: &std::path::Path) -> Vec<FixKind> {
    plan(
        &status(p, StatusOptions::default()).unwrap(),
        &StashConfig::default(),
    )
    .into_iter()
    .map(|f| f.kind)
    .collect()
}

fn head(p: &std::path::Path) -> Head {
    status(p, StatusOptions::default()).unwrap().head
}

fn pushed_feature(sb: &Sandbox) -> std::path::PathBuf {
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    r
}

#[test]
fn clean_default_branch_needs_no_fixes() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    assert_eq!(kinds(&r), vec![]);
}

#[test]
fn switch_default_offered_for_pushed_branch() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    assert_eq!(kinds(&r), vec![FixKind::SwitchDefault]);
}

#[test]
fn switch_default_not_offered_when_unpushed() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "more");
    let k = kinds(&r);
    assert!(!k.contains(&FixKind::SwitchDefault), "{k:?}");
    assert!(k.contains(&FixKind::Push));
}

#[test]
fn switch_default_not_offered_when_tracked_changes() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    write(&r, "README", "dirty");
    let k = kinds(&r);
    assert!(!k.contains(&FixKind::SwitchDefault), "{k:?}");
    assert!(k.contains(&FixKind::Stash) && k.contains(&FixKind::Commit));
}

#[test]
fn switch_default_allowed_with_only_untracked_files() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    write(&r, "scratch.txt", "x");
    assert!(kinds(&r).contains(&FixKind::SwitchDefault));
}

#[test]
fn switch_default_offered_for_merged_branch_without_upstream() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "nothing-new"]);
    assert_eq!(kinds(&r), vec![FixKind::SwitchDefault]);
}

#[test]
fn unmerged_branch_without_upstream_gets_publish_not_switch() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "local-work"]);
    commit(&r, "work");
    let p = plan(
        &status(&r, StatusOptions::default()).unwrap(),
        &StashConfig::default(),
    );
    assert_eq!(p.len(), 1);
    assert_eq!(
        p[0].op,
        Op::PushSetUpstream {
            remote: "origin".into(),
            branch: "local-work".into()
        }
    );
}

#[test]
fn push_not_offered_when_diverged() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    commit(&r, "local");
    sb.advance_remote("acme", "w", "x");
    git(&r, &["fetch", "-q"]);
    let k = kinds(&r);
    assert!(!k.contains(&FixKind::Push), "{k:?}");
    assert!(k.contains(&FixKind::Pull));
}

#[test]
fn no_fixes_during_conflict() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["switch", "-q", "-c", "o"]);
    write(&r, "README", "o");
    commit(&r, "o");
    git(&r, &["switch", "-q", "main"]);
    write(&r, "README", "m");
    commit(&r, "m");
    let _ = std::process::Command::new("git")
        .current_dir(&r)
        .args(["merge", "o"])
        .output();
    assert_eq!(kinds(&r), vec![]);
}

#[test]
fn fast_forward_offered_for_stale_non_head_branch() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    sb.advance_remote("acme", "w", "x");
    git(&r, &["fetch", "-q"]);
    let k = kinds(&r);
    assert!(k.contains(&FixKind::FastForward), "{k:?}");
    // inverse: once fast-forwarded it's no longer offered
    apply_fix(&r, &[FixKind::FastForward], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(!kinds(&r).contains(&FixKind::FastForward));
    assert!(
        kinds(&r).contains(&FixKind::SwitchDefault),
        "feat itself is still pushed"
    );
}

#[test]
fn apply_switch_default() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    let a = apply_fix(&r, &[FixKind::SwitchDefault], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    assert_eq!(head(&r), Head::Branch("main".into()));
    // idempotent
    assert!(
        apply_fix(&r, &[FixKind::SwitchDefault], &StashConfig::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn apply_respects_allowed_kinds() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    assert!(
        apply_fix(&r, &[FixKind::Push], &StashConfig::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(head(&r), Head::Branch("feat".into()));
}

#[test]
fn one_fix_per_action_stash_then_switch() {
    // No chaining: each auto-fix action applies a single fix.
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    write(&r, "README", "dirty");
    let allowed = [FixKind::Stash, FixKind::SwitchDefault];
    assert_eq!(
        apply_fix(&r, &allowed, &StashConfig::default())
            .unwrap()
            .unwrap()
            .fix
            .kind,
        FixKind::Stash
    );
    let s = status(&r, StatusOptions::default()).unwrap();
    assert_eq!(
        s.head,
        Head::Branch("feat".into()),
        "not switched in the same action"
    );
    assert_eq!(s.stashes.len(), 1);
    assert_eq!(
        apply_fix(&r, &allowed, &StashConfig::default())
            .unwrap()
            .unwrap()
            .fix
            .kind,
        FixKind::SwitchDefault
    );
    assert_eq!(head(&r), Head::Branch("main".into()));
    assert!(
        apply_fix(&r, &allowed, &StashConfig::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn commit_is_never_auto_applied() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "dirty");
    assert!(
        apply_fix(&r, &[FixKind::Commit], &StashConfig::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn commit_all_includes_untracked_and_runs_hooks() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "new.txt", "x");
    write(&r, ".git/hooks/pre-commit", "#!/bin/sh\ntouch hook-ran\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            r.join(".git/hooks/pre-commit"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let o = argus_core::ops::run(
        &r,
        &Op::CommitAll {
            message: "msg".into(),
        },
    );
    assert!(o.success, "{}", o.output);
    assert!(
        r.join("hook-ran").exists(),
        "hooks must run: we use the git CLI on purpose"
    );
    let s = status(&r, StatusOptions::default()).unwrap();
    assert_eq!(
        s.changes.untracked, 1,
        "only the file created by the hook remains"
    );
}

#[test]
fn failed_fix_stops_the_chain() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    git(
        &r,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "/nonexistent/repo.git",
        ],
    );
    let a = apply_fix(
        &r,
        &[FixKind::Push, FixKind::SwitchDefault],
        &StashConfig::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(a.fix.kind, FixKind::Push);
    assert!(!a.outcome.success);
    assert_eq!(head(&r), Head::Branch("feat".into()));
}

#[test]
fn push_then_switch_takes_two_actions() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    let allowed = [FixKind::Push, FixKind::SwitchDefault];
    let a = apply_fix(&r, &allowed, &StashConfig::default())
        .unwrap()
        .unwrap();
    assert_eq!((a.fix.kind, a.outcome.success), (FixKind::Push, true));
    assert_eq!(head(&r), Head::Branch("feat".into()), "push alone");
    let a = apply_fix(&r, &allowed, &StashConfig::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        (a.fix.kind, a.outcome.success),
        (FixKind::SwitchDefault, true)
    );
    assert_eq!(head(&r), Head::Branch("main".into()));
}

#[test]
fn push_not_offered_when_upstream_has_another_name() {
    // `git push` here would go to someone else's repo/branch
    let sb = Sandbox::new();
    sb.remote("acme", "lib");
    sb.remote("me", "lib");
    let r = sb.clone_as_github("me", "lib", "lib");
    git(
        &r,
        &[
            "remote",
            "add",
            "upstream",
            "https://github.com/acme/lib.git",
        ],
    );
    git(&r, &["fetch", "-q", "upstream"]);
    git(
        &r,
        &["switch", "-q", "-c", "topic", "--track", "upstream/main"],
    );
    commit(&r, "topic work");
    let k = kinds(&r);
    assert!(!k.contains(&FixKind::Push), "{k:?}");
    assert!(
        !k.contains(&FixKind::SwitchDefault),
        "work is not on any remote yet"
    );
    git(&r, &["push", "-q", "origin", "topic"]);
    let k = kinds(&r);
    assert!(!k.contains(&FixKind::Push), "{k:?}");
    assert!(
        k.contains(&FixKind::SwitchDefault),
        "work is safe on origin: {k:?}"
    );
}

#[test]
fn default_auto_fix_pushes_but_never_force() {
    let auto = argus_core::Config::default().fix.auto;
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    let a = apply_fix(&r, &auto, &StashConfig::default())
        .unwrap()
        .unwrap();
    assert_eq!((a.fix.kind, a.outcome.success), (FixKind::Push, true));
    assert_eq!(head(&r), Head::Branch("feat".into()));
    // inverse: diverged from upstream -> no push, and so no switch either
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "local");
    let tmp = sb.path("other");
    git(
        &sb.root(),
        &[
            "clone",
            "-q",
            "-b",
            "feat",
            sb.path("forge/acme/w.git").to_str().unwrap(),
            tmp.to_str().unwrap(),
        ],
    );
    commit(&tmp, "remote");
    git(&tmp, &["push", "-q"]);
    git(&r, &["fetch", "-q"]);
    assert!(
        apply_fix(&r, &auto, &StashConfig::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(head(&r), Head::Branch("feat".into()));
}

/// A pushed feature branch with an untracked file offers both
/// switch-default and stash.
fn switch_or_stash(sb: &Sandbox) -> std::path::PathBuf {
    let r = pushed_feature(sb);
    write(&r, "new.txt", "untracked");
    let k = kinds(&r);
    assert!(
        k.contains(&FixKind::SwitchDefault) && k.contains(&FixKind::Stash),
        "{k:?}"
    );
    r
}

fn ranked_kinds(p: &std::path::Path, priority: &[FixKind]) -> Vec<FixKind> {
    ranked(
        &status(p, StatusOptions::default()).unwrap(),
        priority,
        &StashConfig::default(),
    )
    .into_iter()
    .map(|f| f.kind)
    .collect()
}

#[test]
fn the_order_given_is_the_priority() {
    let sb = Sandbox::new();
    let r = switch_or_stash(&sb);
    let s = status(&r, StatusOptions::default()).unwrap();
    let stash_first = [FixKind::Stash, FixKind::SwitchDefault];
    let switch_first = [FixKind::SwitchDefault, FixKind::Stash];
    assert_eq!(
        choose(&s, &stash_first, &StashConfig::default())
            .unwrap()
            .kind,
        FixKind::Stash
    );
    assert_eq!(
        choose(&s, &switch_first, &StashConfig::default())
            .unwrap()
            .kind,
        FixKind::SwitchDefault
    );
    assert_eq!(
        apply_fix(&r, &stash_first, &StashConfig::default())
            .unwrap()
            .unwrap()
            .fix
            .kind,
        FixKind::Stash
    );
    assert_eq!(
        head(&r),
        Head::Branch("feat".into()),
        "stashed, not switched"
    );
}

#[test]
fn unlisted_kinds_rank_after_listed_ones_in_default_order() {
    let sb = Sandbox::new();
    let r = switch_or_stash(&sb);
    // commit is available too (needs a message, so never chosen)
    assert_eq!(
        ranked_kinds(&r, &[FixKind::SwitchDefault]),
        [FixKind::SwitchDefault, FixKind::Stash, FixKind::Commit]
    );
    // default order: clean up uncommitted work first
    assert_eq!(
        ranked_kinds(&r, &[]),
        [FixKind::Stash, FixKind::Commit, FixKind::SwitchDefault]
    );
    let s = status(&r, StatusOptions::default()).unwrap();
    assert!(
        choose(&s, &[], &StashConfig::default()).is_none(),
        "nothing allowed, nothing chosen"
    );
    assert!(choose(&s, &[FixKind::Commit], &StashConfig::default()).is_none());
}

#[test]
fn stash_keeps_index_work_tree_and_untracked_apart() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "staged.txt", "base\n");
    write(&r, "unstaged.txt", "base\n");
    write(&r, ".gitignore", "*.log\n");
    git(&r, &["add", "."]);
    commit(&r, "files");
    write(&r, "staged.txt", "staged\n");
    git(&r, &["add", "staged.txt"]);
    write(&r, "unstaged.txt", "unstaged\n");
    write(&r, "untracked.txt", "new\n");
    write(&r, "ignored.log", "noise\n");
    let a = apply_fix(&r, &[FixKind::Stash], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    assert_eq!(
        git(&r, &["status", "--porcelain"]),
        "",
        "everything stashed"
    );
    assert!(
        r.join("ignored.log").exists(),
        "ignored files are left alone"
    );

    git(&r, &["stash", "pop", "--index"]);
    let st = git(&r, &["status", "--porcelain"]);
    let mut lines: Vec<&str> = st.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        [" M unstaged.txt", "?? untracked.txt", "M  staged.txt"],
        "staged stays staged, unstaged stays unstaged, untracked comes back"
    );
}

#[test]
fn default_order_cleans_up_before_pushing() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "more");
    write(&r, "README", "dirty");
    assert_eq!(
        ranked_kinds(&r, &[]),
        [FixKind::Stash, FixKind::Commit, FixKind::Push]
    );
}

/// The predicted chain must match what repeated auto-fix actions really do.
fn assert_chain_matches_reality(r: &std::path::Path, allowed: &[FixKind], expect: &[FixKind]) {
    let s = status(r, StatusOptions::default()).unwrap();
    let predicted: Vec<_> = chain(&s, allowed, &StashConfig::default())
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

/// Feature branch with one unpushed commit and a dirty tracked file.
fn ahead_and_dirty(sb: &Sandbox) -> std::path::PathBuf {
    let r = pushed_feature(sb);
    commit(&r, "more");
    write(&r, "README", "dirty");
    r
}

#[test]
fn chain_stash_push_switch() {
    let sb = Sandbox::new();
    let r = ahead_and_dirty(&sb);
    use FixKind::*;
    assert_chain_matches_reality(
        &r,
        &[Stash, Push, SwitchDefault],
        &[Stash, Push, SwitchDefault],
    );
}

#[test]
fn chain_stops_where_the_allowed_kinds_run_out() {
    // default auto (push, switch-default): the dirty tree blocks the switch
    let sb = Sandbox::new();
    let r = ahead_and_dirty(&sb);
    use FixKind::*;
    assert_chain_matches_reality(&r, &[Push, SwitchDefault], &[Push]);
}

#[test]
fn chain_publish_then_switch() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    use FixKind::*;
    assert_chain_matches_reality(&r, &[Publish, SwitchDefault], &[Publish, SwitchDefault]);
}

/// Pushed feature branch checked out while `main` is behind origin/main.
fn main_behind(sb: &Sandbox) -> std::path::PathBuf {
    let r = pushed_feature(sb);
    let other = sb.clone_as_github("acme", "w", "other");
    commit(&other, "upstream work");
    git(&other, &["push", "-q", "origin", "main"]);
    git(&r, &["fetch", "-q"]);
    r
}

#[test]
fn chain_switch_then_pull_the_default_branch() {
    let sb = Sandbox::new();
    let r = main_behind(&sb);
    use FixKind::*;
    assert_chain_matches_reality(
        &r,
        &[SwitchDefault, Pull, FastForward],
        &[SwitchDefault, Pull],
    );
}

#[test]
fn chain_fast_forward_first_makes_the_pull_unnecessary() {
    let sb = Sandbox::new();
    let r = main_behind(&sb);
    use FixKind::*;
    assert_chain_matches_reality(
        &r,
        &[FastForward, SwitchDefault, Pull],
        &[FastForward, SwitchDefault],
    );
}

#[test]
fn chain_is_empty_when_nothing_is_allowed() {
    let sb = Sandbox::new();
    let r = ahead_and_dirty(&sb);
    assert_chain_matches_reality(&r, &[], &[]);
    assert_chain_matches_reality(&r, &[FixKind::Commit], &[]);
}

#[test]
fn default_order_switches_before_catching_up() {
    let sb = Sandbox::new();
    let r = main_behind(&sb);
    assert_eq!(
        ranked_kinds(&r, &[]),
        [FixKind::SwitchDefault, FixKind::FastForward]
    );
    use FixKind::*;
    assert_chain_matches_reality(&r, &FixKind::ALL, &[SwitchDefault, Pull]);
}
