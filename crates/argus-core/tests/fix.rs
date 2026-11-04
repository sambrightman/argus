//! Fix planning and application. Each fix has tests for when it must *not*
//! be offered, since an unsafe fix is far worse than a missing one.

use argus_core::fix::{FixKind, apply_fixes, plan};
use argus_core::ops::Op;
use argus_core::status::{Head, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};

fn kinds(p: &std::path::Path) -> Vec<FixKind> {
    plan(&status(p, StatusOptions::default()).unwrap())
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
    let p = plan(&status(&r, StatusOptions::default()).unwrap());
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
    apply_fixes(&r, &[FixKind::FastForward], 5).unwrap();
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
    let applied = apply_fixes(&r, &[FixKind::SwitchDefault], 5).unwrap();
    assert_eq!(applied.len(), 1);
    assert!(applied[0].outcome.success, "{}", applied[0].outcome.output);
    assert_eq!(head(&r), Head::Branch("main".into()));
    // idempotent
    assert!(
        apply_fixes(&r, &[FixKind::SwitchDefault], 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn apply_respects_allowed_kinds() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    assert!(apply_fixes(&r, &[FixKind::Push], 5).unwrap().is_empty());
    assert_eq!(head(&r), Head::Branch("feat".into()));
}

#[test]
fn apply_chains_stash_then_switch() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    write(&r, "README", "dirty");
    let applied = apply_fixes(&r, &[FixKind::Stash, FixKind::SwitchDefault], 5).unwrap();
    let ks: Vec<_> = applied.iter().map(|a| a.fix.kind).collect();
    assert_eq!(ks, vec![FixKind::Stash, FixKind::SwitchDefault]);
    let s = status(&r, StatusOptions::default()).unwrap();
    assert_eq!(s.head, Head::Branch("main".into()));
    assert_eq!(s.stashes, 1);
    assert!(s.changes.is_clean());
}

#[test]
fn commit_is_never_auto_applied() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "dirty");
    assert!(apply_fixes(&r, &[FixKind::Commit], 5).unwrap().is_empty());
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
    let applied = apply_fixes(&r, &[FixKind::Push, FixKind::SwitchDefault], 5).unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].fix.kind, FixKind::Push);
    assert!(!applied[0].outcome.success);
    assert_eq!(head(&r), Head::Branch("feat".into()));
}

#[test]
fn push_then_switch() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    let applied = apply_fixes(&r, &[FixKind::Push, FixKind::SwitchDefault], 5).unwrap();
    let ks: Vec<_> = applied
        .iter()
        .map(|a| (a.fix.kind, a.outcome.success))
        .collect();
    assert_eq!(
        ks,
        vec![(FixKind::Push, true), (FixKind::SwitchDefault, true)]
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
