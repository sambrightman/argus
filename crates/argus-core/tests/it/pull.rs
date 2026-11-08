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
        let unpushed = |s: &RepoStatus| s.branches.iter().find(|b| b.is_head).unwrap().unpushed;
        assert_eq!(
            unpushed(&predicted),
            unpushed(&actual),
            "pull.rebase={rebase}"
        );
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
