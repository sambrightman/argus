//! The demo must show every state argus can display.

use argus_core::attention::Issue;
use argus_core::discover::RepoKind;
use argus_core::{Config, State, Workspace, demo};

#[test]
fn demo_covers_every_state() {
    let d = demo::build();
    let cfg = format!("roots = ['{}']", d.root.display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    d.apply_marks(&mut ws);
    let has = |f: &dyn Fn(&argus_core::Entry) -> bool| ws.entries.iter().any(f);
    let issue = |i: fn(&Issue) -> bool| has(&|e| e.issues.iter().any(i));
    let checks: Vec<(&str, bool)> = vec![
        (
            "clean",
            has(&|e| e.status().is_some() && !e.needs_attention()),
        ),
        (
            "staged",
            has(&|e| e.status().is_some_and(|s| s.changes.staged > 0)),
        ),
        (
            "unstaged",
            has(&|e| e.status().is_some_and(|s| s.changes.unstaged > 0)),
        ),
        (
            "untracked",
            has(&|e| e.status().is_some_and(|s| s.changes.untracked > 0)),
        ),
        ("ahead", issue(|i| matches!(i, Issue::Unpushed(_)))),
        ("behind", issue(|i| matches!(i, Issue::Behind(_)))),
        (
            "diverged",
            has(&|e| e.status().is_some_and(|s| s.integration.is_some())),
        ),
        (
            "non-default branch",
            issue(|i| matches!(i, Issue::NonDefaultBranch)),
        ),
        ("no upstream", issue(|i| matches!(i, Issue::NoUpstream))),
        ("gone", issue(|i| matches!(i, Issue::UpstreamGone))),
        ("conflicts", issue(|i| matches!(i, Issue::Conflicts(_)))),
        ("in progress", issue(|i| matches!(i, Issue::InProgress))),
        ("detached", issue(|i| matches!(i, Issue::Detached))),
        (
            "detached commits",
            issue(|i| matches!(i, Issue::DetachedCommits(_))),
        ),
        (
            "tags",
            has(&|e| e.status().is_some_and(|s| s.tags.len() > 1)),
        ),
        ("stashes", issue(|i| matches!(i, Issue::Stashes(_)))),
        (
            "ignored stashes",
            has(&|e| {
                e.ignored_issues
                    .iter()
                    .any(|i| matches!(i, Issue::Stashes(_)))
            }),
        ),
        (
            "other branches",
            issue(|i| matches!(i, Issue::UnpushedBranches(_))),
        ),
        ("no remote", issue(|i| matches!(i, Issue::NoRemote))),
        ("worktree", has(&|e| e.kind == RepoKind::Worktree)),
        ("submodule", has(&|e| e.kind == RepoKind::Submodule)),
        (
            "nested repo",
            has(&|e| {
                e.status().is_some_and(|s| {
                    s.changes
                        .files
                        .iter()
                        .any(|f| f.kind == argus_core::status::PathKind::Repo)
                })
            }),
        ),
        ("bare", has(&|e| e.kind == RepoKind::Bare)),
        ("error", has(&|e| e.error().is_some())),
        ("favourite", has(&|e| e.flags.favourite)),
        ("new", has(&|e| e.flags.new)),
        ("ignored", has(&|e| e.flags.ignored)),
        ("snoozed", has(&|e| e.flags.snoozed)),
    ];
    let missing: Vec<&str> = checks.iter().filter(|c| !c.1).map(|c| c.0).collect();
    assert!(missing.is_empty(), "demo lacks: {missing:?}");
}
