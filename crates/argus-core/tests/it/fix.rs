//! Fix planning and application. Each fix has tests for when it must *not*
//! be offered, since an unsafe fix is far worse than a missing one.

use argus_core::attention::{self, Issue};
use argus_core::config::StashConfig;
use argus_core::fix::{FixKind, apply_fix, chain, choose, plan, ranked, simulate};
use argus_core::ops::Op;
use argus_core::status::{Head, PathKind, StatusOptions, status};
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

/// A repo where `o` and `main` both changed README, on `main`.
fn conflicting(sb: &Sandbox) -> std::path::PathBuf {
    let r = sb.init("r");
    git(&r, &["switch", "-q", "-c", "o"]);
    write(&r, "README", "o");
    commit(&r, "o");
    git(&r, &["switch", "-q", "main"]);
    write(&r, "README", "m");
    commit(&r, "m");
    r
}

#[test]
fn only_abort_during_an_operation_and_never_automatically() {
    use argus_core::status::InProgress;
    for (what, args) in [
        (InProgress::Merge, ["merge", "o"].as_slice()),
        (InProgress::Rebase, &["rebase", "o"]),
        (InProgress::CherryPick, &["cherry-pick", "o"]),
        (InProgress::Revert, &["revert", "--no-edit", "HEAD~1"]),
        (InProgress::Bisect, &["bisect", "start"]),
    ] {
        let sb = Sandbox::new();
        let r = conflicting(&sb);
        // so that reverting `m` conflicts too
        write(&r, "README", "later");
        commit(&r, "later");
        let head_before = git(&r, &["rev-parse", "HEAD"]);
        argus_core::testkit::git_may_fail(&r, args);
        let before = st(&r);
        assert_eq!(before.in_progress, Some(what));
        assert_eq!(kinds(&r), [FixKind::Abort], "{what:?}");
        assert!(choose(&before, &FixKind::ALL, &StashConfig::default()).is_none());
        assert!(
            argus_core::fix::preview(&before, &FixKind::ALL, &StashConfig::default())
                .alternatives
                .is_empty(),
            "not offered as an alternative either"
        );
        let out = argus_core::ops::run(&r, &Op::Abort { what });
        assert!(out.success, "{what:?}: {}", out.output);
        let after = st(&r);
        assert_eq!(after.in_progress, None, "{what:?}");
        assert!(after.changes.is_clean(), "{what:?}");
        assert_eq!(git(&r, &["rev-parse", "HEAD"]), head_before, "{what:?}");
        assert_eq!(after.head, Head::Branch("main".into()));
    }
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
fn commit_takes_only_whats_staged() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "staged\n");
    git(&r, &["add", "README"]);
    write(&r, "README", "and then unstaged\n");
    write(&r, "new.txt", "x");
    let o = argus_core::ops::run(
        &r,
        &Op::Commit {
            message: "msg".into(),
        },
    );
    assert!(o.success, "{}", o.output);
    assert_eq!(git(&r, &["show", "HEAD:README"]), "staged\n");
    let s = status(&r, StatusOptions::default()).unwrap();
    assert_eq!((s.changes.unstaged, s.changes.untracked), (1, 1));
}

#[test]
fn untracked_files_offer_a_gitignore_fix_never_automatic() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    assert!(
        !kinds(&r).contains(&FixKind::Gitignore),
        "nothing untracked"
    );
    write(&r, "new.txt", "x");
    write(&r, "build/out.o", "x");
    let s = status(&r, StatusOptions::default()).unwrap();
    let f = plan(&s, &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::Gitignore)
        .unwrap();
    assert_eq!(f.reason, "2 untracked path(s)");
    assert_eq!(
        f.op,
        Op::Ignore {
            file: argus_core::gitignore::IgnoreFile::TopLevel,
            paths: vec!["build/".into(), "new.txt".into()],
            anchored: true
        }
    );
    // which file, and how: always asked
    assert!(FixKind::Gitignore.needs_input());
    assert_eq!(FixKind::Gitignore.name(), "gitignore");
    assert!(choose(&s, &[FixKind::Gitignore], &StashConfig::default()).is_none());
}

#[test]
fn reset_moves_the_branch_each_way() {
    use argus_core::ops::ResetMode;
    // HEAD~1 "initial" has README "hello"; HEAD changes it and adds b.txt
    let setup = || {
        let sb = Sandbox::new();
        let r = sb.init("r");
        write(&r, "README", "changed\n");
        write(&r, "b.txt", "b\n");
        commit(&r, "second");
        let target = git(&r, &["rev-parse", "HEAD~1"]).trim().to_string();
        (sb, r, target)
    };
    let reset = |r: &std::path::Path, mode, commit: &str| {
        argus_core::ops::run(
            r,
            &Op::Reset {
                mode,
                commit: commit.into(),
            },
        )
    };
    let codes = |r: &std::path::Path| {
        let s = status(r, StatusOptions::default()).unwrap();
        s.changes
            .files
            .iter()
            .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
            .collect::<Vec<_>>()
    };
    for (mode, expect) in [
        (ResetMode::Soft, vec!["M  README", "A  b.txt"]),
        (ResetMode::Mixed, vec![" M README", "?? b.txt"]),
        (ResetMode::Hard, vec![]),
        (ResetMode::Keep, vec![]),
    ] {
        let (_sb, r, target) = setup();
        let o = reset(&r, mode, &target);
        assert!(o.success, "{mode:?}: {}", o.output);
        assert_eq!(git(&r, &["rev-parse", "HEAD"]).trim(), target, "{mode:?}");
        assert_eq!(codes(&r), expect, "{mode:?}");
    }
    // keep refuses to touch uncommitted changes the reset would overwrite
    let (_sb, r, target) = setup();
    write(&r, "README", "mine\n");
    let before = git(&r, &["rev-parse", "HEAD"]);
    assert!(!reset(&r, ResetMode::Keep, &target).success);
    assert_eq!(git(&r, &["rev-parse", "HEAD"]), before);
    assert_eq!(std::fs::read_to_string(r.join("README")).unwrap(), "mine\n");
}

#[test]
fn amend_adds_whats_staged_keeping_or_replacing_the_message() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let amend = |message: Option<&str>| {
        argus_core::ops::run(
            &r,
            &Op::Amend {
                message: message.map(str::to_string),
            },
        )
    };
    let before = git(&r, &["rev-parse", "HEAD"]);
    write(&r, "README", "staged\n");
    git(&r, &["add", "README"]);
    write(&r, "other.txt", "not staged\n");
    assert!(amend(None).success);
    assert_ne!(git(&r, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&r, &["log", "--format=%B", "-1"]).trim(), "initial");
    assert_eq!(git(&r, &["show", "HEAD:README"]), "staged\n");
    assert_eq!(git(&r, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert!(amend(Some("new words")).success);
    assert_eq!(git(&r, &["log", "--format=%B", "-1"]).trim(), "new words");
    assert!(r.join("other.txt").exists());
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
    // commit and gitignore are available too (need input, so never chosen)
    assert_eq!(
        ranked_kinds(&r, &[FixKind::SwitchDefault]),
        [
            FixKind::SwitchDefault,
            FixKind::Stash,
            FixKind::Commit,
            FixKind::Gitignore
        ]
    );
    // default order: clean up uncommitted work first
    assert_eq!(
        ranked_kinds(&r, &[]),
        [
            FixKind::Stash,
            FixKind::Commit,
            FixKind::Gitignore,
            FixKind::SwitchDefault
        ]
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
    // push, switch-default: the dirty tree blocks the switch
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

fn st(p: &std::path::Path) -> argus_core::status::RepoStatus {
    status(p, StatusOptions::default()).unwrap()
}

/// The fixes of `kind`, as (op label, blocked).
fn of_kind(p: &std::path::Path, kind: FixKind) -> Vec<(String, Option<String>)> {
    plan(&st(p), &StashConfig::default())
        .into_iter()
        .filter(|f| f.kind == kind)
        .map(|f| (f.op.label(), f.blocked))
        .collect()
}

/// The commit a branch of the forge repo `acme/w` points at.
fn forge_tip(sb: &Sandbox, branch: &str) -> Option<String> {
    let bare = sb.path("forge/acme/w.git");
    argus_core::testkit::git_may_fail(&bare, &["rev-parse", "--verify", "-q", branch])
        .then(|| git(&bare, &["rev-parse", branch]))
}

/// `feat`, pulling from `origin/main`, with one commit, `push.default` as given.
fn feat_pulls_from_main(sb: &Sandbox, push_default: &str) -> std::path::PathBuf {
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["config", "push.default", push_default]);
    git(
        &r,
        &["switch", "-q", "-c", "feat", "--track", "origin/main"],
    );
    commit(&r, "feat");
    r
}

#[test]
fn a_branch_pulling_from_main_is_pushed_to_its_own_name() {
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "current");
    let main_before = forge_tip(&sb, "main");
    let own = st(&r).branches[0].own.clone().unwrap();
    assert_eq!((own.name.as_str(), own.gone), ("origin/feat", true));
    // not on the remote yet: publishing puts it there, upstream untouched
    assert_eq!(
        of_kind(&r, FixKind::Publish),
        [("push feat to origin/feat".to_string(), None)]
    );
    assert!(of_kind(&r, FixKind::Push).is_empty());
    // inverse: nothing of its own yet (all on main), nothing to publish
    git(
        &r,
        &["switch", "-q", "-c", "fresh", "--track", "origin/main"],
    );
    assert!(st(&r).branches[0].own.as_ref().unwrap().gone);
    assert!(of_kind(&r, FixKind::Publish).is_empty());
    assert!(of_kind(&r, FixKind::Push).is_empty());
    git(&r, &["switch", "-q", "feat"]);
    let before = st(&r);
    let a = apply_fix(&r, &[FixKind::Publish], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    let predicted = argus_core::fix::simulate(&before, &a.fix);
    assert_eq!(
        predicted.branches,
        st(&r).branches,
        "the prediction matches"
    );
    assert!(
        of_kind(&r, FixKind::Push).is_empty(),
        "nothing more to push"
    );
    assert_eq!(
        forge_tip(&sb, "feat"),
        Some(git(&r, &["rev-parse", "HEAD"]))
    );
    assert_eq!(forge_tip(&sb, "main"), main_before, "main untouched");
    let s = st(&r);
    assert_eq!(s.upstream.as_ref().unwrap().name, "origin/main");
    assert_eq!(s.branches[0].unpushed, Some(0));
    // more work: the default auto-fix pushes it there, never to main
    commit(&r, "more");
    assert_eq!(
        of_kind(&r, FixKind::Push),
        [("push feat to origin/feat".to_string(), None)]
    );
    let auto = argus_core::Config::default().fix.auto;
    let a = apply_fix(&r, &auto, &StashConfig::default())
        .unwrap()
        .unwrap();
    assert_eq!((a.fix.kind, a.outcome.success), (FixKind::Push, true));
    assert_eq!(
        forge_tip(&sb, "feat"),
        Some(git(&r, &["rev-parse", "HEAD"]))
    );
    assert_eq!(forge_tip(&sb, "main"), main_before, "main untouched");
    // rewritten after pushing: pushing would need force, so it's blocked
    git(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&r, "rewritten");
    let push = of_kind(&r, FixKind::Push);
    assert!(
        push[0].1.as_deref().is_some_and(|b| b.contains("force")),
        "{push:?}"
    );
    assert!(choose(&st(&r), &auto, &StashConfig::default()).is_none());
}

#[test]
fn nothing_is_pushed_where_git_push_would_go_to_main_or_refuse() {
    for push_default in ["upstream", "simple"] {
        let sb = Sandbox::new();
        let r = feat_pulls_from_main(&sb, push_default);
        assert_eq!(st(&r).branches[0].own, None, "{push_default}");
        let k = kinds(&r);
        assert!(
            !k.contains(&FixKind::Push) && !k.contains(&FixKind::Publish),
            "{push_default}: {k:?}"
        );
        git(&r, &["push", "-q", "origin", "feat"]);
        commit(&r, "more");
        let k = kinds(&r);
        assert!(!k.contains(&FixKind::Push), "{push_default}: {k:?}");
    }
    // a push refspec sending it to another name: not its own name either
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "current");
    git(
        &r,
        &[
            "config",
            "remote.origin.push",
            "refs/heads/feat:refs/heads/other",
        ],
    );
    git(&r, &["push", "-q", "origin"]);
    assert_eq!(st(&r).branches[0].own, None);
}

#[test]
fn chain_publish_to_own_name_then_switch() {
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "current");
    use FixKind::*;
    assert_chain_matches_reality(&r, &[Publish, SwitchDefault], &[Publish, SwitchDefault]);
}

#[test]
fn a_gone_upstream_with_unpushed_work_is_published_again() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    assert!(st(&r).upstream.unwrap().gone);
    assert_eq!(
        of_kind(&r, FixKind::Publish),
        [("push -u origin feat".to_string(), None)]
    );
    use FixKind::*;
    assert_chain_matches_reality(&r, &[Publish, SwitchDefault], &[Publish, SwitchDefault]);
    assert!(!st(&r).upstream.unwrap().gone);
    // inverse: merged before the branch was deleted, nothing to publish
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    git(&r, &["push", "-q", "origin", "feat:main"]);
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    let k = kinds(&r);
    assert!(!k.contains(&Publish), "{k:?}");
    assert!(k.contains(&SwitchDefault), "{k:?}");
}

#[test]
fn other_branches_are_pushed_to_their_remote_branches() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    git(&r, &["switch", "-q", "main"]);
    assert_eq!(
        of_kind(&r, FixKind::PushBranches),
        [("push feat to origin/feat".to_string(), None)]
    );
    assert!(
        of_kind(&r, FixKind::Push).is_empty(),
        "not the current branch"
    );
    let a = apply_fix(&r, &[FixKind::PushBranches], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    assert_eq!(
        forge_tip(&sb, "feat"),
        Some(git(&r, &["rev-parse", "feat"]))
    );
    assert_eq!(head(&r), Head::Branch("main".into()));
    assert!(of_kind(&r, FixKind::PushBranches).is_empty(), "done");
    // inverse: a local-only branch isn't published
    git(&r, &["branch", "local"]);
    git(&r, &["switch", "-q", "local"]);
    commit(&r, "local work");
    git(&r, &["switch", "-q", "main"]);
    assert!(of_kind(&r, FixKind::PushBranches).is_empty());
    // inverse: rewritten after pushing, so blocked
    git(&r, &["switch", "-q", "feat"]);
    git(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&r, "rewritten");
    git(&r, &["switch", "-q", "main"]);
    let p = of_kind(&r, FixKind::PushBranches);
    assert!(
        p.len() == 1 && p[0].1.as_deref().is_some_and(|b| b.contains("force")),
        "{p:?}"
    );
}

#[test]
fn other_branches_pulling_from_main_are_pushed_to_their_own_name_only() {
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "current");
    git(&r, &["push", "-q", "origin", "feat"]);
    commit(&r, "more");
    git(&r, &["switch", "-q", "main"]);
    let main_before = forge_tip(&sb, "main");
    let a = apply_fix(&r, &[FixKind::PushBranches], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    assert_eq!(
        forge_tip(&sb, "feat"),
        Some(git(&r, &["rev-parse", "feat"]))
    );
    assert_eq!(forge_tip(&sb, "main"), main_before);
    // inverse: not on the remote yet, so not created
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "current");
    git(&r, &["switch", "-q", "main"]);
    assert!(of_kind(&r, FixKind::PushBranches).is_empty());
    // inverse: git push would push it to main
    let sb = Sandbox::new();
    let r = feat_pulls_from_main(&sb, "upstream");
    git(&r, &["push", "-q", "origin", "feat:feat"]);
    commit(&r, "more");
    git(&r, &["switch", "-q", "main"]);
    assert!(of_kind(&r, FixKind::PushBranches).is_empty());
}

#[test]
fn a_detached_head_at_a_branch_is_reattached() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "--detach", "main"]);
    write(&r, "README", "dirty: switching to the same commit is fine");
    assert_eq!(st(&r).at_head, ["main"]);
    assert_eq!(
        of_kind(&r, FixKind::Reattach),
        [("switch main".to_string(), None)]
    );
    use FixKind::*;
    assert_chain_matches_reality(&r, &[Reattach], &[Reattach]);
    assert_eq!(head(&r), Head::Branch("main".into()));
    // a single other branch here: that one
    git(&r, &["switch", "-q", "-c", "solo"]);
    commit(&r, "solo");
    git(&r, &["switch", "-q", "--detach", "solo"]);
    assert_eq!(
        of_kind(&r, FixKind::Reattach),
        [("switch solo".to_string(), None)]
    );
    git(&r, &["switch", "-q", "main"]);
    // several branches here: the default branch wins…
    git(&r, &["branch", "a"]);
    git(&r, &["switch", "-q", "--detach", "main"]);
    assert_eq!(
        of_kind(&r, FixKind::Reattach),
        [("switch main".to_string(), None)]
    );
    // …else the user chooses
    commit(&r, "past main");
    git(&r, &["branch", "-f", "a"]);
    git(&r, &["branch", "b"]);
    git(&r, &["switch", "-q", "--detach", "a"]);
    let re = of_kind(&r, FixKind::Reattach);
    assert_eq!(re.len(), 2, "{re:?}");
    assert!(
        re.iter()
            .all(|(_, b)| b.as_deref() == Some("2 branches are at HEAD: a, b"))
    );
    assert!(choose(&st(&r), &[Reattach], &StashConfig::default()).is_none());
    // inverse: no branch at HEAD
    commit(&r, "detached work");
    assert!(st(&r).at_head.is_empty());
    assert!(of_kind(&r, FixKind::Reattach).is_empty());
    // inverse: on a branch, nothing at HEAD is reported
    git(&r, &["switch", "-q", "a"]);
    assert!(st(&r).at_head.is_empty());
}

#[test]
fn commits_on_no_branch_get_a_branch_by_name_never_automatically() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "--detach", "main"]);
    // inverse: nothing on no branch, nothing to name
    assert!(of_kind(&r, FixKind::Branch).is_empty());
    commit(&r, "adrift");
    let before = st(&r);
    let fixes = plan(&before, &StashConfig::default());
    let f = fixes.iter().find(|f| f.kind == FixKind::Branch).unwrap();
    assert_eq!(f.reason, "1 commit(s) on no branch");
    assert!(choose(&before, &FixKind::ALL, &StashConfig::default()).is_none());
    let named = argus_core::fix::Fix {
        op: Op::CreateBranch {
            name: "rescue".into(),
        },
        ..f.clone()
    };
    let out = argus_core::ops::run(&r, &named.op);
    assert!(out.success, "{}", out.output);
    let after = st(&r);
    assert_eq!(after.head, Head::Branch("rescue".into()));
    assert_eq!(after.detached_orphans, 0);
    let predicted = argus_core::fix::simulate(&before, &named);
    assert_eq!(
        (
            &predicted.head,
            &predicted.branches,
            predicted.detached_orphans
        ),
        (&after.head, &after.branches, after.detached_orphans),
        "the prediction matches"
    );
}

#[test]
fn a_repo_without_a_remote_gets_one_by_url_never_automatically() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let before = st(&r);
    let fixes = plan(&before, &StashConfig::default());
    let f = fixes.iter().find(|f| f.kind == FixKind::AddRemote).unwrap();
    assert!(
        choose(&before, &FixKind::ALL, &StashConfig::default())
            .is_none_or(|c| c.kind != FixKind::AddRemote)
    );
    let bare = sb.remote("acme", "w");
    let added = argus_core::fix::Fix {
        op: Op::AddRemote {
            name: "origin".into(),
            url: bare.display().to_string(),
        },
        ..f.clone()
    };
    let out = argus_core::ops::run(&r, &added.op);
    assert!(out.success, "{}", out.output);
    let after = st(&r);
    let predicted = argus_core::fix::simulate(&before, &added);
    assert_eq!(
        (&predicted.remote, &predicted.remote_url),
        (&after.remote, &after.remote_url)
    );
    // inverse: a clone has one
    let c = sb.clone_as_github("acme", "w", "c");
    assert!(of_kind(&c, FixKind::AddRemote).is_empty());
}

#[test]
fn merged_branches_without_a_live_upstream_can_be_deleted_never_automatically() {
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    git(&r, &["push", "-q", "origin", "feat:main"]);
    git(&r, &["switch", "-q", "main"]);
    git(&r, &["pull", "-q"]);
    git(&r, &["branch", "local-done"]);
    git(&r, &["branch", "local-wip"]);
    git(&r, &["switch", "-q", "local-wip"]);
    commit(&r, "wip");
    git(&r, &["switch", "-q", "main"]);
    // inverse: feat's upstream is still there, local-wip isn't merged
    assert_eq!(
        of_kind(&r, FixKind::DeleteBranches),
        [("delete branch local-done".to_string(), None)]
    );
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    let s = st(&r);
    let fixes: Vec<_> = plan(&s, &StashConfig::default())
        .into_iter()
        .filter(|f| f.kind == FixKind::DeleteBranches)
        .map(|f| f.reason)
        .collect();
    assert_eq!(
        fixes,
        [
            "feat is merged into main and its upstream is gone",
            "local-done is merged into main"
        ]
    );
    assert!(
        choose(&s, &FixKind::ALL, &StashConfig::default())
            .is_none_or(|f| f.kind != FixKind::DeleteBranches)
    );
    for f in plan(&s, &StashConfig::default())
        .into_iter()
        .filter(|f| f.kind == FixKind::DeleteBranches)
    {
        let predicted = argus_core::fix::simulate(&st(&r), &f);
        let out = argus_core::ops::run(&r, &f.op);
        assert!(out.success, "{}", out.output);
        assert_eq!(predicted.branches, st(&r).branches);
    }
    let names: Vec<String> = st(&r).branches.into_iter().map(|b| b.name).collect();
    assert_eq!(names, ["main", "local-wip"]);
    // inverse: never the current or the default branch
    git(&r, &["switch", "-q", "-c", "here"]);
    git(&r, &["branch", "--unset-upstream", "main"]);
    let k = of_kind(&r, FixKind::DeleteBranches);
    assert!(k.is_empty(), "{k:?}");
}

#[test]
fn stash_ids_are_the_stash_commits() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    for n in ["one", "two"] {
        write(&r, "README", n);
        git(&r, &["stash", "push", "-q", "-m", n]);
    }
    let s = st(&r);
    assert_eq!(s.stashes, ["two", "one"]);
    assert_eq!(
        s.stash_ids,
        [
            git(&r, &["rev-parse", "stash@{0}"]).trim(),
            git(&r, &["rev-parse", "stash@{1}"]).trim()
        ]
    );
}

#[test]
fn the_newest_stash_is_popped_only_when_it_applies_cleanly() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "staged");
    git(&r, &["add", "README"]);
    write(&r, "new", "untracked");
    git(&r, &["stash", "push", "-q", "-u", "-m", "mine"]);
    let before = st(&r);
    assert_eq!(
        of_kind(&r, FixKind::StashPop),
        [("pop stash".to_string(), None)]
    );
    // it can be automatic, if asked for
    let a = apply_fix(&r, &[FixKind::StashPop], &StashConfig::default())
        .unwrap()
        .unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    let after = st(&r);
    assert!(after.stashes.is_empty());
    assert_eq!(changes_of(&after), ["M  README", "?? new"], "index kept");
    let predicted = argus_core::fix::simulate(&before, &a.fix);
    assert_eq!(predicted.stashes, after.stashes);
    assert!(!predicted.changes.is_clean(), "predicted dirty");
    // inverse: tracked changes in the way
    git(&r, &["stash", "push", "-q", "-u"]);
    write(&r, "README", "in the way");
    assert!(of_kind(&r, FixKind::StashPop).is_empty());
    // inverse: HEAD moved on and would conflict: refused, nothing changed
    commit(&r, "moved on");
    let out = argus_core::ops::run(&r, &Op::StashPop);
    assert!(!out.success);
    assert!(
        out.output.contains("would conflict in README"),
        "{}",
        out.output
    );
    let s = st(&r);
    assert_eq!(s.stashes.len(), 1);
    assert!(s.changes.is_clean(), "{:?}", s.changes);
}

fn changes_of(s: &argus_core::status::RepoStatus) -> Vec<String> {
    s.changes
        .files
        .iter()
        .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
        .collect()
}

#[test]
fn a_stash_is_dropped_only_if_it_is_still_the_one_asked_about() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "old");
    git(&r, &["stash", "push", "-q", "-m", "old"]);
    let s = st(&r);
    let drop = Op::StashDrop {
        index: 0,
        id: s.stash_ids[0].clone(),
    };
    // another stash arrived since: stash@{0} is that one now
    write(&r, "README", "new");
    git(&r, &["stash", "push", "-q", "-m", "new"]);
    let out = argus_core::ops::run(&r, &drop);
    assert!(!out.success);
    assert!(
        out.output.contains("isn't the stash it was"),
        "{}",
        out.output
    );
    assert_eq!(st(&r).stashes, ["new", "old"]);
    // as listed now: dropped
    let s = st(&r);
    let drop = Op::StashDrop {
        index: 1,
        id: s.stash_ids[1].clone(),
    };
    let out = argus_core::ops::run(&r, &drop);
    assert!(out.success, "{}", out.output);
    assert_eq!(st(&r).stashes, ["new"]);
}

#[test]
fn default_auto_fixes_push_other_branches_and_reattach() {
    let auto = argus_core::Config::default().fix.auto;
    let sb = Sandbox::new();
    let r = pushed_feature(&sb);
    commit(&r, "unpushed");
    git(&r, &["switch", "-q", "--detach", "main"]);
    use FixKind::*;
    // the branch's commits first, then back onto main
    assert_chain_matches_reality(&r, &auto, &[PushBranches, Reattach]);
    assert_eq!(head(&r), Head::Branch("main".into()));
}

#[test]
fn merged_only_into_a_local_main_is_not_pushed() {
    use argus_core::attention::{Issue, issues};
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    git(&r, &["switch", "-q", "main"]);
    git(&r, &["merge", "-q", "feat"]);
    // no remote: nothing is pushed, and no_remote says it once
    let is = issues(&st(&r));
    assert!(is.contains(&Issue::NoRemote), "{is:?}");
    assert!(
        !is.iter().any(|i| matches!(i, Issue::UnpushedBranches(_))),
        "{is:?}"
    );
    // a remote with nothing on it yet: main and feat are on no remote
    let bare = sb.path("empty.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    git(&r, &["remote", "add", "origin", bare.to_str().unwrap()]);
    let s = st(&r);
    assert!(
        s.branches.iter().all(|b| b.merged && b.is_unpushed()),
        "{:?}",
        s.branches
    );
    let is = issues(&s);
    assert!(is.contains(&Issue::NoUpstream), "{is:?}");
    assert!(is.contains(&Issue::UnpushedBranches(1)), "{is:?}");
    assert_eq!(
        of_kind(&r, FixKind::Publish),
        [("push -u origin main".to_string(), None)]
    );
    // leaving feat for main hides nothing: it's merged, if only locally
    git(&r, &["switch", "-q", "feat"]);
    let k = kinds(&r);
    assert!(
        k.contains(&FixKind::SwitchDefault) && k.contains(&FixKind::Publish),
        "{k:?}"
    );
    // once main is pushed, feat's commits are on the remote too
    git(&r, &["push", "-q", "origin", "main"]);
    let s = st(&r);
    assert!(
        s.branches.iter().all(|b| !b.is_unpushed()),
        "{:?}",
        s.branches
    );
    assert!(
        !issues(&s)
            .iter()
            .any(|i| matches!(i, Issue::UnpushedBranches(_)))
    );
}

#[test]
fn conflicts_with_nothing_in_progress_get_no_fixes() {
    // a stash pop that conflicted: no operation to abort, a human resolves it
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "stashed\n");
    git(&r, &["stash", "push", "-q"]);
    write(&r, "README", "committed\n");
    commit(&r, "moved on");
    assert!(!argus_core::testkit::git_may_fail(&r, &["stash", "pop"]));
    let s = st(&r);
    assert!(s.changes.conflicted > 0 && s.in_progress.is_none());
    assert_eq!(kinds(&r), []);
}

#[test]
fn alternatives_are_what_the_chain_leaves_out() {
    let sb = Sandbox::new();
    let r = switch_or_stash(&sb);
    let s = st(&r);
    let p = argus_core::fix::preview(&s, &[FixKind::SwitchDefault], &StashConfig::default());
    let kinds_of = |v: &[argus_core::fix::Fix]| v.iter().map(|f| f.kind).collect::<Vec<_>>();
    assert_eq!(kinds_of(&p.chain), [FixKind::SwitchDefault]);
    // stash is there, switch-default isn't repeated, commit needs input
    assert_eq!(kinds_of(&p.alternatives), [FixKind::Stash]);
}

/// `pushed_feature`, with `lib` a submodule since before the branch.
fn with_submodule(sb: &Sandbox) -> (std::path::PathBuf, std::path::PathBuf) {
    let src = sb.init("libsrc");
    commit(&src, "l");
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(
        &r,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            src.to_str().unwrap(),
            "lib",
        ],
    );
    commit(&r, "sub");
    git(&r, &["push", "-q"]);
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    let lib = r.join("lib");
    (r, lib)
}

fn sub_entry(p: &std::path::Path) -> (char, char, PathKind) {
    let s = status(p, StatusOptions::default()).unwrap();
    let f = s.changes.entry("lib").unwrap();
    (f.index, f.worktree, f.kind)
}

/// Changes inside a submodule are its own to stash, commit or ignore, on
/// its own row: the parent can't (git's stash ignores submodules).
#[test]
fn changes_inside_a_submodule_are_fixed_in_it_not_its_parent() {
    let sb = Sandbox::new();
    let (r, lib) = with_submodule(&sb);
    write(&lib, "new", "x");
    // as `git status --short`: untracked content, then modified content
    assert_eq!(sub_entry(&r), (' ', '?', PathKind::Submodule));
    write(&lib, ".gitkeep", "x");
    git(&lib, &["add", ".gitkeep"]);
    assert_eq!(sub_entry(&r), (' ', 'm', PathKind::Submodule));
    let s = status(&r, StatusOptions::default()).unwrap();
    let c = &s.changes;
    assert_eq!(
        (c.staged, c.unstaged, c.untracked, c.submodule_content),
        (0, 0, 0, 1)
    );
    assert!(
        !attention::issues(&s)
            .iter()
            .any(|i| matches!(i, Issue::Dirty(_)))
    );
    let k = kinds(&r);
    assert!(
        !k.contains(&FixKind::Stash) && !k.contains(&FixKind::Commit),
        "{k:?}"
    );
    assert!(k.contains(&FixKind::SwitchDefault), "{k:?}");
    assert!(kinds(&lib).contains(&FixKind::Stash), "its own row");
    // what an auto-fix does is what was offered
    let applied = apply_fix(
        &r,
        &[FixKind::Stash, FixKind::SwitchDefault],
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(applied.fix.kind, FixKind::SwitchDefault);
    assert!(lib.join("new").exists(), "the submodule's changes stay");
    // and stay after committing everything else
    write(&r, "README", "changed\n");
    assert_eq!(after_commit(&r), [(' ', 'm')]);
}

/// What a preview expects to be left after committing everything.
fn after_commit(r: &std::path::Path) -> Vec<(char, char)> {
    let s = status(r, StatusOptions::default()).unwrap();
    let commit = plan(&s, &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::Commit)
        .unwrap();
    let c = simulate(&s, &commit).changes;
    assert_eq!((c.uncommitted(), c.submodule_content), (0, c.files.len()));
    c.files.iter().map(|f| (f.index, f.worktree)).collect()
}

/// A submodule at another commit than its parent records: a commit records
/// it; no stash can take it, and switching away would lose track of it.
#[test]
fn a_submodule_at_another_commit_is_committed_in_its_parent() {
    let sb = Sandbox::new();
    let (r, lib) = with_submodule(&sb);
    commit(&lib, "l2");
    assert_eq!(sub_entry(&r), (' ', 'M', PathKind::Submodule));
    let only = |r: &std::path::Path| {
        let k = kinds(r);
        assert!(k.contains(&FixKind::Commit), "{k:?}");
        assert!(!k.contains(&FixKind::Stash), "{k:?}");
        assert!(!k.contains(&FixKind::SwitchDefault), "{k:?}");
    };
    only(&r);
    git(&r, &["add", "lib"]);
    assert_eq!(sub_entry(&r), ('M', ' ', PathKind::Submodule));
    only(&r);
    assert!(
        apply_fix(
            &r,
            &[FixKind::Stash, FixKind::SwitchDefault],
            &Default::default()
        )
        .unwrap()
        .is_none()
    );
    // inverse: an ordinary change alongside is stashed, the submodule stays
    write(&r, "README", "changed\n");
    let s = status(&r, StatusOptions::default()).unwrap();
    let stash = plan(&s, &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::Stash)
        .unwrap();
    assert_eq!(stash.reason, "1 uncommitted path(s)");
    let after = simulate(&s, &stash);
    assert_eq!((after.changes.staged, after.changes.unstaged), (1, 0));
    assert_eq!(after.changes.files.len(), 1);
    assert_eq!(after.changes.files[0].path, "lib");
    // a commit records the submodule's commit; what's inside it stays
    assert_eq!(after_commit(&r), []);
    write(&lib, "new", "x");
    assert_eq!(after_commit(&r), [(' ', '?')]);
}

/// A repo nested untracked in the work tree: no stash takes it (git skips
/// it) and a commit mustn't add it as an embedded repo; ignoring it can.
#[test]
fn a_nested_repo_is_ignored_not_stashed_or_committed() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    sb.init("w/top");
    sb.init("w/tools/deep");
    let k = kinds(&r);
    assert!(k.contains(&FixKind::Gitignore), "{k:?}");
    assert!(
        !k.contains(&FixKind::Stash) && !k.contains(&FixKind::Commit),
        "{k:?}"
    );
    // inverse: with a file alongside, they take the file only
    write(&r, "README", "changed\n");
    let s = status(&r, StatusOptions::default()).unwrap();
    let fixes = plan(&s, &StashConfig::default());
    for kind in [FixKind::Stash, FixKind::Commit] {
        let f = fixes.iter().find(|f| f.kind == kind).unwrap();
        assert_eq!(f.reason, "1 uncommitted path(s)", "{kind:?}");
    }
    // what a commit's preview leaves: the nested repos, as they were
    let commit = fixes.iter().find(|f| f.kind == FixKind::Commit).unwrap();
    let c = simulate(&s, commit).changes;
    assert_eq!((c.untracked, c.untracked_repos, c.uncommitted()), (2, 2, 0));
    let left: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(left, ["tools/", "top"]);
    let out = argus_core::ops::run(
        &r,
        &Op::CommitAll {
            message: "readme".into(),
        },
    );
    assert!(out.success, "{}", out.output);
    assert_eq!(git(&r, &["ls-files", "--", "top", "tools"]), "");
    assert_eq!(git(&r, &["status", "--short"]), "?? tools/\n?? top/\n");
}

/// A submodule at another commit than recorded can also be put back:
/// offered when its commit is held by a ref (nothing is lost), shown
/// blocked when it isn't or the submodule has changes of its own.
#[test]
fn a_moved_submodule_is_put_back_only_when_nothing_is_lost() {
    let sb = Sandbox::new();
    let (r, lib) = with_submodule(&sb);
    let recorded = git(&lib, &["rev-parse", "HEAD"]);
    let fix = |r: &std::path::Path| {
        let s = status(r, StatusOptions::default()).unwrap();
        plan(&s, &StashConfig::default())
            .into_iter()
            .find(|f| f.kind == FixKind::SubmoduleUpdate)
    };
    // the new commit on a branch in it: safe
    git(&lib, &["switch", "-q", "-c", "work"]);
    commit(&lib, "l2");
    let f = fix(&r).expect("offered");
    assert_eq!(f.blocked, None);
    assert_eq!(
        f.op,
        Op::SubmoduleUpdate {
            paths: vec!["lib".into()]
        }
    );
    assert_eq!(f.reason, "lib at another commit than recorded");
    // the preview: the submodule's entry goes, an ordinary change stays
    write(&r, "README", "changed\n");
    let after = simulate(&status(&r, StatusOptions::default()).unwrap(), &f);
    let c = &after.changes;
    assert_eq!((c.unstaged, c.unstaged_submodules), (1, 0));
    let left: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(left, ["README"]);
    assert!(after.moved_submodules.is_empty());
    git(&r, &["checkout", "-q", "--", "README"]);
    let out = argus_core::ops::run(&r, &f.op);
    assert!(out.success, "{}", out.output);
    assert_eq!(git(&lib, &["rev-parse", "HEAD"]), recorded);
    assert_eq!(fix(&r), None, "nothing left to put back");
    // the commit on no ref: it'd be lost
    git(&lib, &["switch", "-q", "--detach"]);
    commit(&lib, "only here");
    let f = fix(&r).expect("shown");
    assert_eq!(
        f.blocked.as_deref(),
        Some("lib: 1 commit on no branch would be lost")
    );
    assert_eq!(
        f.op,
        Op::SubmoduleUpdate {
            paths: vec!["lib".into()]
        },
        "shown for lib"
    );
    // changes of its own: they'd be in the way
    git(&lib, &["switch", "-q", "work"]);
    write(&lib, "README", "changed\n");
    let f = fix(&r).expect("shown");
    assert_eq!(f.blocked.as_deref(), Some("lib has changes of its own"));
    // inverse: a staged move is recorded, nothing to put back
    git(&lib, &["checkout", "-q", "--", "."]);
    git(&r, &["add", "lib"]);
    assert_eq!(fix(&r), None);
}

#[test]
fn a_fixup_commits_whats_staged_for_a_later_autosquash() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a", "1\n");
    commit(&r, "add a");
    let target = git(&r, &["rev-parse", "HEAD"]);
    write(&r, "b", "1\n");
    commit(&r, "add b");
    write(&r, "a", "2\n");
    git(&r, &["add", "a"]);
    write(&r, "unstaged", "x");
    let o = argus_core::ops::run(
        &r,
        &Op::Fixup {
            commit: target.trim().into(),
            squash: None,
            instant: false,
        },
    );
    assert!(o.success, "{}", o.output);
    assert_eq!(
        git(&r, &["log", "-1", "--format=%s"]).trim(),
        "fixup! add a"
    );
    assert_eq!(git(&r, &["show", "HEAD:a"]), "2\n");
    let s = status(&r, StatusOptions::default()).unwrap();
    assert_eq!(
        (s.changes.staged, s.changes.untracked),
        (0, 1),
        "only what was staged"
    );
}

/// A squash commit (`squash! …`, with a message to add), and the instant
/// kinds, folded in at once by a non-interactive autosquash rebase.
#[test]
fn squash_and_instant_fixups_fold_in() {
    let run = |r: &std::path::Path, commit: &str, squash: Option<&str>, instant| {
        argus_core::ops::run(
            r,
            &Op::Fixup {
                commit: commit.into(),
                squash: squash.map(String::from),
                instant,
            },
        )
    };
    let rev = |r: &std::path::Path, x: &str| git(r, &["rev-parse", x]).trim().to_string();
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a", "1\n");
    commit(&r, "add a");
    write(&r, "b", "1\n");
    commit(&r, "add b");
    let target = rev(&r, "HEAD~1");
    // squash: a commit to fold in later, with its message
    write(&r, "a", "2\n");
    git(&r, &["add", "a"]);
    let o = run(&r, &target, Some("also a"), false);
    assert!(o.success, "{}", o.output);
    assert_eq!(
        git(&r, &["log", "-1", "--format=%B"]).trim(),
        "squash! add a\n\nalso a"
    );
    git(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    // instant fixup: folded into the target, uncommitted work kept
    write(&r, "a", "3\n");
    git(&r, &["add", "a"]);
    write(&r, "b", "unstaged\n");
    let o = run(&r, &target, None, true);
    assert!(o.success, "{}", o.output);
    assert_eq!(
        git(&r, &["log", "-2", "--format=%s"]).trim(),
        "add b\nadd a",
        "no fixup! left"
    );
    assert_eq!(git(&r, &["rev-list", "--count", "HEAD"]).trim(), "3");
    assert_eq!(git(&r, &["show", "HEAD~1:a"]), "3\n");
    assert_eq!(std::fs::read_to_string(r.join("b")).unwrap(), "unstaged\n");
    git(&r, &["checkout", "-q", "--", "b"]);
    // instant squash: the messages combined
    let target = rev(&r, "HEAD~1");
    write(&r, "a", "4\n");
    git(&r, &["add", "a"]);
    let o = run(&r, &target, Some("and more"), true);
    assert!(o.success, "{}", o.output);
    assert_eq!(
        git(&r, &["log", "-1", "--format=%B", "HEAD~1"]).trim(),
        "add a\n\nand more"
    );
    // the first commit has no parent: --root
    let first = rev(&r, "HEAD~2");
    write(&r, "c", "new\n");
    git(&r, &["add", "c"]);
    let o = run(&r, &first, None, true);
    assert!(o.success, "{}", o.output);
    assert_eq!(git(&r, &["rev-list", "--count", "HEAD"]).trim(), "3");
    assert_eq!(git(&r, &["show", "HEAD~2:c"]), "new\n");
}

/// An instant fixup rewrites history: refused for a commit already on a
/// remote (a plain fixup still works).
#[test]
fn an_instant_fixup_of_a_pushed_commit_is_refused() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let pushed = git(&r, &["rev-parse", "HEAD"]).trim().to_string();
    write(&r, "README", "fix\n");
    git(&r, &["add", "README"]);
    let op = |instant| Op::Fixup {
        commit: pushed.clone(),
        squash: None,
        instant,
    };
    let o = argus_core::ops::run(&r, &op(true));
    assert!(
        !o.success && o.output.contains("on a remote"),
        "{}",
        o.output
    );
    assert_eq!(
        git(&r, &["rev-parse", "HEAD"]).trim(),
        pushed,
        "nothing committed"
    );
    assert!(argus_core::ops::run(&r, &op(false)).success);
}

/// A main repo with a linked worktree `wt` beside it.
fn with_worktree(sb: &Sandbox) -> (std::path::PathBuf, std::path::PathBuf) {
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let wt = sb.path("wt");
    git(
        &r,
        &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
    );
    (r, wt)
}

/// A worktree's folder deleted by hand leaves its record in the main
/// repo: flagged there, and pruned (its branch stays).
#[test]
fn stale_worktree_records_are_pruned() {
    let sb = Sandbox::new();
    let (r, wt) = with_worktree(&sb);
    let s = st(&r);
    assert!(s.stale_worktrees.is_empty() && s.worktrees == [wt.clone()]);
    assert!(
        !attention::issues(&s)
            .iter()
            .any(|i| matches!(i, Issue::StaleWorktrees(_)))
    );
    assert!(of_kind(&r, FixKind::PruneWorktrees).is_empty());

    std::fs::remove_dir_all(&wt).unwrap();
    let s = st(&r);
    assert_eq!(s.stale_worktrees, std::slice::from_ref(&wt));
    assert!(s.worktrees.is_empty(), "not listed as a worktree");
    assert!(attention::issues(&s).contains(&Issue::StaleWorktrees(1)));
    assert_eq!(
        of_kind(&r, FixKind::PruneWorktrees),
        [("prune stale worktree records".to_string(), None)]
    );
    assert!(!FixKind::PruneWorktrees.manual(), "nothing on disk is lost");
    assert_chain_matches_reality(&r, &[FixKind::PruneWorktrees], &[FixKind::PruneWorktrees]);
    let s = st(&r);
    assert!(s.stale_worktrees.is_empty());
    assert!(
        s.branches.iter().any(|b| b.name == "wt"),
        "the branch stays"
    );
}

/// A locked worktree (on a removable drive, say) isn't stale while its
/// folder is away; a living worktree doesn't report its siblings.
#[test]
fn locked_or_seen_from_a_worktree_is_not_stale() {
    let sb = Sandbox::new();
    let (r, wt) = with_worktree(&sb);
    let other = sb.path("other");
    git(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "other",
            other.to_str().unwrap(),
        ],
    );
    git(&r, &["worktree", "lock", wt.to_str().unwrap()]);
    std::fs::remove_dir_all(&wt).unwrap();
    assert!(st(&r).stale_worktrees.is_empty(), "locked");
    git(&r, &["worktree", "unlock", wt.to_str().unwrap()]);
    assert_eq!(st(&r).stale_worktrees, std::slice::from_ref(&wt));
    assert!(
        st(&other).stale_worktrees.is_empty(),
        "the main repo reports it"
    );
    // pruning one record (from its row) leaves the others, predicted too
    std::fs::remove_dir_all(&other).unwrap();
    let s = st(&r);
    assert_eq!(s.stale_worktrees.len(), 2);
    let one = argus_core::fix::Fix {
        kind: FixKind::PruneWorktrees,
        op: Op::PruneWorktree { path: wt.clone() },
        reason: String::new(),
        blocked: None,
    };
    assert_eq!(
        simulate(&s, &one).stale_worktrees,
        std::slice::from_ref(&other)
    );
    assert!(argus_core::ops::run(&r, &one.op).success);
    assert_eq!(st(&r).stale_worktrees, std::slice::from_ref(&other));
}

/// `wt`'s branch pushed, merged into main (pushed) and deleted on the
/// remote: the usual end of a pull request.
fn finished_worktree(sb: &Sandbox) -> (std::path::PathBuf, std::path::PathBuf) {
    let (r, wt) = with_worktree(sb);
    commit(&wt, "work");
    git(&wt, &["push", "-q", "-u", "origin", "wt"]);
    git(&r, &["merge", "-q", "--no-edit", "--no-ff", "wt"]);
    git(&r, &["push", "-q"]);
    git(&r, &["push", "-q", "origin", "--delete", "wt"]);
    git(&r, &["fetch", "-q", "--prune"]);
    (r, wt)
}

fn finished_issue(p: &std::path::Path) -> bool {
    attention::issues(&st(p)).contains(&Issue::FinishedWorktree)
}

/// A linked worktree whose branch is merged, upstream gone and nothing
/// uncommitted is finished: removed (confirmed first), its branch kept.
#[test]
fn a_finished_worktree_is_offered_for_removal() {
    let sb = Sandbox::new();
    let (r, wt) = finished_worktree(&sb);
    let issues = attention::issues(&st(&wt));
    assert!(issues.contains(&Issue::FinishedWorktree), "{issues:?}");
    assert!(!issues.contains(&Issue::NonDefaultBranch), "said instead");
    assert_eq!(
        of_kind(&wt, FixKind::RemoveWorktree),
        [("remove this worktree".to_string(), None)]
    );
    assert!(FixKind::RemoveWorktree.needs_confirmation());
    assert!(
        of_kind(&wt, FixKind::SwitchDefault).is_empty(),
        "removed instead"
    );
    let fix = plan(&st(&wt), &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::RemoveWorktree)
        .unwrap();
    assert!(argus_core::ops::run(&wt, &fix.op).success);
    assert!(!wt.exists());
    let s = st(&r);
    assert!(s.worktrees.is_empty() && s.stale_worktrees.is_empty());
    assert!(
        s.branches.iter().any(|b| b.name == "wt"),
        "the branch stays"
    );
}

/// Not finished: anything uncommitted, work not merged, an upstream still
/// there, no upstream (a new worktree looks merged too), or the main
/// checkout. Locked: offered, blocked.
#[test]
fn unfinished_worktrees_are_left_alone() {
    let sb = Sandbox::new();
    let (r, wt) = finished_worktree(&sb);
    write(&wt, "scratch", "x");
    assert!(!finished_issue(&wt), "untracked file");
    assert!(of_kind(&wt, FixKind::RemoveWorktree).is_empty());
    std::fs::remove_file(wt.join("scratch")).unwrap();
    assert!(finished_issue(&wt));

    git(&wt, &["worktree", "lock", "."]);
    assert_eq!(
        of_kind(&wt, FixKind::RemoveWorktree),
        [(
            "remove this worktree".to_string(),
            Some("it's locked (git worktree lock)".to_string())
        )]
    );
    git(&wt, &["worktree", "unlock", "."]);

    commit(&wt, "more");
    assert!(!finished_issue(&wt), "unmerged work");

    // the main checkout on such a branch: switch-default's
    git(&wt, &["switch", "-q", "--detach"]);
    git(&r, &["switch", "-q", "wt"]);
    git(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    assert!(!finished_issue(&r), "not a linked worktree");

    let sb = Sandbox::new();
    let (_r, wt) = with_worktree(&sb);
    assert!(!finished_issue(&wt), "new, no upstream");
    git(&wt, &["push", "-q", "-u", "origin", "wt"]);
    assert!(!finished_issue(&wt), "upstream still there");
}

/// An app with submodule `lib` (committed).
fn app_with_submodule(sb: &Sandbox) -> std::path::PathBuf {
    let lib = sb.init("libsrc");
    write(&lib, "f", "lib\n");
    commit(&lib, "lib");
    let app = sb.init("app");
    commit(&app, "app");
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
    app
}

/// A submodule whose folder was deleted is a submodule change (no stash
/// takes it), listed as missing, and put back: nothing can be lost.
#[test]
fn a_missing_submodule_folder_is_put_back() {
    let sb = Sandbox::new();
    let app = app_with_submodule(&sb);
    std::fs::remove_dir_all(app.join("lib")).unwrap();
    let s = st(&app);
    assert_eq!(s.missing_submodules, ["lib"]);
    assert_eq!(
        s.changes
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.worktree, f.kind))
            .collect::<Vec<_>>(),
        [("lib", 'D', PathKind::Submodule)]
    );
    assert_eq!(s.changes.unstaged_submodules, 1);
    assert_eq!(
        of_kind(&app, FixKind::SubmoduleUpdate),
        [("check out lib at its recorded commit".to_string(), None)]
    );
    let reason = plan(&s, &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::SubmoduleUpdate)
        .unwrap()
        .reason;
    assert_eq!(reason, "lib's folder is missing");
    let f = plan(&s, &StashConfig::default())
        .into_iter()
        .find(|f| f.kind == FixKind::SubmoduleUpdate)
        .unwrap();
    let after = simulate(&s, &f);
    assert!(
        after.missing_submodules.is_empty() && after.changes.is_clean(),
        "{after:?}"
    );
    assert_chain_matches_reality(
        &app,
        &[FixKind::SubmoduleUpdate],
        &[FixKind::SubmoduleUpdate],
    );
    assert!(app.join("lib/f").exists());
    let s = st(&app);
    assert!(s.missing_submodules.is_empty() && s.changes.is_clean());
}

/// An empty folder is a submodule not checked out (a clone without
/// `--recursive`): normal, not missing.
#[test]
fn an_uninitialised_submodule_is_not_missing() {
    let sb = Sandbox::new();
    let app = app_with_submodule(&sb);
    git(&app, &["submodule", "deinit", "-q", "-f", "lib"]);
    assert!(app.join("lib").is_dir());
    let s = st(&app);
    assert!(
        s.missing_submodules.is_empty(),
        "{:?}",
        s.missing_submodules
    );
    assert!(of_kind(&app, FixKind::SubmoduleUpdate).is_empty());
}
