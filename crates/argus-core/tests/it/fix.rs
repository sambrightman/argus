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
