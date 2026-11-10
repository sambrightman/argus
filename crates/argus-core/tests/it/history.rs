//! Drilling down: the commits behind a status, and one commit in full.

use argus_core::history::{Range, commit_detail, commits};
use argus_core::testkit::{Sandbox, commit, git, write};

fn summaries(r: &std::path::Path, range: &Range, limit: usize) -> (Vec<String>, bool) {
    let (v, more) = commits(r, range, limit).unwrap();
    (v.into_iter().map(|c| c.summary).collect(), more)
}

#[test]
fn a_branchs_commits_on_no_remote_newest_first() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "pushed");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    commit(&r, "first local");
    commit(&r, "second local");
    let range = Range::NotOnRemote {
        tip: "refs/heads/feat".into(),
    };
    assert_eq!(
        summaries(&r, &range, 10),
        (vec!["second local".into(), "first local".into()], false)
    );
    // a limit, and whether there's more
    assert_eq!(
        summaries(&r, &range, 1),
        (vec!["second local".into()], true)
    );
    let (v, _) = commits(&r, &range, 10).unwrap();
    assert_eq!(v[0].id, git(&r, &["rev-parse", "HEAD"]).trim());
    assert!(v[0].time.is_some() && !v[0].author.is_empty());
    // inverse: once pushed, none
    git(&r, &["push", "-q"]);
    assert_eq!(summaries(&r, &range, 10), (vec![], false));
}

#[test]
fn what_a_pull_would_bring_in() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    sb.advance_remote("acme", "w", "theirs.txt");
    git(&r, &["fetch", "-q"]);
    let range = Range::Missing {
        tip: "refs/remotes/origin/main".into(),
        from: "HEAD".into(),
    };
    assert_eq!(
        summaries(&r, &range, 10),
        (vec!["remote theirs.txt".into()], false)
    );
}

#[test]
fn commits_only_a_detached_head_holds() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["switch", "-q", "--detach"]);
    commit(&r, "adrift");
    assert_eq!(
        summaries(&r, &Range::Orphans, 10),
        (vec!["adrift".into()], false)
    );
    git(&r, &["tag", "kept"]);
    assert_eq!(
        summaries(&r, &Range::Orphans, 10),
        (vec![], false),
        "a tag holds it"
    );
}

#[test]
fn a_commit_in_full() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "gone.txt", "gone\n");
    write(&r, "dir/kept.txt", "kept\n");
    git(&r, &["add", "."]);
    git(&r, &["commit", "-q", "-m", "setup"]);
    git(&r, &["rm", "-q", "gone.txt"]);
    write(&r, "README", "changed\n");
    write(&r, "dir/new.txt", "new\n");
    git(&r, &["add", "."]);
    git(
        &r,
        &[
            "commit",
            "-q",
            "-m",
            "Change things\n\nWhy, at length.\nSecond line.",
        ],
    );
    let d = commit_detail(&r, "HEAD").unwrap();
    assert_eq!(d.message, "Change things\n\nWhy, at length.\nSecond line.");
    assert_eq!(
        d.files,
        [
            ('M', "README".to_string()),
            ('A', "dir/new.txt".to_string()),
            ('D', "gone.txt".to_string())
        ]
    );
    assert_eq!(
        d.parents,
        [git(&r, &["rev-parse", "HEAD~1"]).trim().to_string()]
    );
    assert!(!d.author.is_empty() && !d.email.is_empty() && d.time.is_some());
    // a move, as git sees it
    git(&r, &["mv", "dir/kept.txt", "moved.txt"]);
    git(&r, &["commit", "-q", "-m", "move"]);
    let d = commit_detail(&r, "HEAD").unwrap();
    assert_eq!(d.files, [('R', "dir/kept.txt -> moved.txt".to_string())]);
    // the root commit: everything added
    let root = git(&r, &["rev-list", "--max-parents=0", "HEAD"]);
    let d = commit_detail(&r, root.trim()).unwrap();
    assert!(d.parents.is_empty());
    assert_eq!(d.files, [('A', "README".to_string())]);
    // inverse: no such commit
    assert!(commit_detail(&r, "0000000000000000000000000000000000000001").is_err());
}

#[test]
fn a_branchs_history_newest_first() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "second");
    commit(&r, "third");
    let range = Range::Ancestors { tip: "HEAD".into() };
    assert_eq!(
        summaries(&r, &range, 10),
        (
            vec!["third".into(), "second".into(), "initial".into()],
            false
        )
    );
    assert_eq!(
        summaries(&r, &range, 2),
        (vec!["third".into(), "second".into()], true)
    );
}
