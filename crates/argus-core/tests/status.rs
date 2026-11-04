//! Status detection against real repositories. Every detector is paired with
//! its inverse (the clean case must *not* report it).

use argus_core::attention::{Issue, issues};
use argus_core::status::{Head, InProgress, RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};
use pretty_assertions::assert_eq;

fn st(p: &std::path::Path) -> RepoStatus {
    status(p, StatusOptions::default()).unwrap()
}

#[test]
fn clean_clone_has_no_issues() {
    let sb = Sandbox::new();
    sb.remote("acme", "widget");
    let r = sb.clone_as_github("acme", "widget", "dev/widget");
    let s = st(&r);
    assert_eq!(s.head, Head::Branch("main".into()));
    assert_eq!(s.default_branch.as_deref(), Some("main"));
    assert!(s.on_default());
    assert_eq!(s.remote.as_deref(), Some("origin"));
    assert_eq!(
        s.remote_url.as_deref(),
        Some("https://github.com/acme/widget.git"),
        "un-rewritten URL"
    );
    let up = s.upstream.as_ref().unwrap();
    assert_eq!(
        (up.name.as_str(), up.ahead, up.behind, up.gone),
        ("origin/main", 0, 0, false)
    );
    assert!(s.changes.is_clean());
    assert_eq!(s.stashes, 0);
    assert_eq!(s.in_progress, None);
    assert!(s.last_commit.is_some());
    assert_eq!(issues(&s), vec![]);
}

#[test]
fn counts_staged_unstaged_untracked_separately() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "changed\n"); // unstaged
    write(&r, "new.txt", "x"); // untracked
    write(&r, "dir/a", "x"); // untracked dir, collapsed
    write(&r, "dir/b", "x");
    write(&r, "staged.txt", "s");
    git(&r, &["add", "staged.txt"]);
    let s = st(&r);
    assert_eq!(
        (s.changes.staged, s.changes.unstaged, s.changes.untracked),
        (1, 1, 2)
    );
    let codes: Vec<_> = s
        .changes
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.index, f.worktree))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("README", ' ', 'M'),
            ("dir/", '?', '?'),
            ("new.txt", '?', '?'),
            ("staged.txt", 'A', ' ')
        ]
    );
    assert!(issues(&s).contains(&Issue::Dirty(4)));
}

#[test]
fn same_file_staged_and_modified_counts_in_both() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "README", "one\n");
    git(&r, &["add", "README"]);
    write(&r, "README", "two\n");
    let s = st(&r);
    assert_eq!((s.changes.staged, s.changes.unstaged), (1, 1));
    assert_eq!(s.changes.files.len(), 1);
    assert_eq!(
        (s.changes.files[0].index, s.changes.files[0].worktree),
        ('M', 'M')
    );
}

#[test]
fn deletions_staged_and_unstaged() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a", "a");
    write(&r, "b", "b");
    commit(&r, "ab");
    std::fs::remove_file(r.join("a")).unwrap();
    git(&r, &["rm", "-q", "b"]);
    let s = st(&r);
    let codes: Vec<_> = s
        .changes
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.index, f.worktree))
        .collect();
    assert_eq!(codes, vec![("a", ' ', 'D'), ("b", 'D', ' ')]);
}

#[test]
fn ignored_files_are_not_untracked() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, ".gitignore", "*.log\n");
    commit(&r, "ignore");
    write(&r, "x.log", "noise");
    assert!(st(&r).changes.is_clean());
}

#[test]
fn dir_with_only_ignored_files_and_empty_dirs_is_not_untracked() {
    // Found on a real Xcode project: git hides such a directory, gix's
    // collapsed walk reported it as untracked.
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, ".gitignore", "userdata/\n");
    commit(&r, "ignore");
    write(&r, "proj/ws/userdata/state", "x");
    std::fs::create_dir_all(r.join("proj/ws/shared/empty")).unwrap();
    let s = st(&r);
    assert!(s.changes.is_clean(), "{:?}", s.changes.files);
    // inverse: one real untracked file makes the outermost untracked dir show, as in git
    write(&r, "proj/ws/shared/empty/real", "x");
    let s = st(&r);
    let files: Vec<_> = s.changes.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["proj/"]);
}

#[test]
fn untracked_files_inside_tracked_dirs_are_listed_individually() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "src/main.rs", "fn main() {}");
    commit(&r, "src");
    write(&r, "src/new.rs", "x");
    write(&r, "src/sub/deep.rs", "x");
    let s = st(&r);
    let files: Vec<_> = s.changes.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, vec!["src/new.rs", "src/sub/"]);
}

#[test]
fn ahead_and_behind() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    write(&r, "local", "1");
    commit(&r, "local 1");
    write(&r, "local", "2");
    commit(&r, "local 2");
    sb.advance_remote("acme", "w", "remote");
    let s = st(&r);
    assert_eq!((s.ahead(), s.behind()), (2, 0), "not fetched yet");
    git(&r, &["fetch", "-q"]);
    let s = st(&r);
    assert_eq!((s.ahead(), s.behind()), (2, 1));
    let is = issues(&s);
    assert!(is.contains(&Issue::Unpushed(2)) && is.contains(&Issue::Behind(1)));
    assert!(s.last_fetch.is_some());
}

#[test]
fn feature_branch_pushed_vs_unpushed() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    write(&r, "f", "1");
    commit(&r, "feat");
    let s = st(&r);
    assert!(!s.on_default());
    assert_eq!(s.upstream, None);
    let is = issues(&s);
    assert!(is.contains(&Issue::NoUpstream), "{is:?}");
    assert!(is.contains(&Issue::OffDefault));
    let feat = s.branches.iter().find(|b| b.name == "feat").unwrap();
    assert!(feat.is_head && !feat.merged && feat.is_unpushed());
    let main = s.branches.iter().find(|b| b.name == "main").unwrap();
    assert!(!main.is_unpushed(), "main is merged into itself");

    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    let s = st(&r);
    let is = issues(&s);
    assert!(
        !is.contains(&Issue::NoUpstream) && !is.iter().any(|i| matches!(i, Issue::Unpushed(_)))
    );
    assert!(is.contains(&Issue::OffDefault));
}

#[test]
fn branch_without_upstream_but_merged_is_not_unpushed() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["branch", "old"]); // same commit as main
    let s = st(&r);
    let old = s.branches.iter().find(|b| b.name == "old").unwrap();
    assert!(old.merged && !old.is_unpushed());
    assert_eq!(issues(&s), vec![]);
}

#[test]
fn other_unpushed_branches_are_reported() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "wip"]);
    commit(&r, "wip");
    git(&r, &["switch", "-q", "main"]);
    assert_eq!(issues(&st(&r)), vec![Issue::UnpushedBranches(1)]);
}

#[test]
fn upstream_gone() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    git(&r, &["fetch", "-q", "--prune"]);
    let s = st(&r);
    assert!(s.upstream.as_ref().unwrap().gone);
    assert!(issues(&s).contains(&Issue::UpstreamGone));
}

#[test]
fn upstream_gone_but_merged_is_fine() {
    // The usual "PR merged, branch deleted" case: safe to leave.
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["switch", "-q", "-c", "feat"]);
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    git(&r, &["fetch", "-q", "--prune"]);
    let is = issues(&st(&r));
    assert!(!is.contains(&Issue::UpstreamGone), "{is:?}");
    assert!(is.contains(&Issue::OffDefault));
}

#[test]
fn detached_head() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "second");
    git(&r, &["checkout", "-q", "HEAD~1"]);
    let s = st(&r);
    assert!(matches!(s.head, Head::Detached(ref id) if id.len() >= 7));
    assert!(issues(&s).contains(&Issue::Detached));
}

#[test]
fn unborn_repo() {
    let sb = Sandbox::new();
    let r = sb.path("empty");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    let s = st(&r);
    assert_eq!(s.head, Head::Unborn("main".into()));
    assert!(s.branches.is_empty());
    assert_eq!(s.last_commit, None);
    assert_eq!(issues(&s), vec![Issue::NoRemote]);
}

#[test]
fn local_only_repo() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let s = st(&r);
    assert_eq!(s.remote, None);
    assert_eq!(s.default_branch.as_deref(), Some("main"));
    assert_eq!(
        issues(&s),
        vec![Issue::NoRemote],
        "no NoUpstream without a remote"
    );
}

#[test]
fn default_branch_follows_remote_head() {
    let sb = Sandbox::new();
    let bare = sb.remote("acme", "w");
    git(&bare, &["branch", "trunk", "main"]);
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/trunk"]);
    let r = sb.clone_as_github("acme", "w", "w");
    let s = st(&r);
    assert_eq!(s.default_branch.as_deref(), Some("trunk"));
    git(&r, &["switch", "-q", "main"]);
    let s = st(&r);
    assert!(!s.on_default());
}

#[test]
fn stashes_counted() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    for i in 0..3 {
        write(&r, "README", &format!("{i}"));
        git(&r, &["stash", "-q"]);
    }
    let s = st(&r);
    assert_eq!(s.stashes, 3);
    assert!(s.changes.is_clean());
    assert_eq!(issues(&s), vec![Issue::Stashes(3), Issue::NoRemote]);
}

#[test]
fn merge_conflict() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["switch", "-q", "-c", "other"]);
    write(&r, "README", "other\n");
    commit(&r, "other");
    git(&r, &["switch", "-q", "main"]);
    write(&r, "README", "main\n");
    commit(&r, "main");
    let out = std::process::Command::new("git")
        .current_dir(&r)
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let s = st(&r);
    assert_eq!(s.in_progress, Some(InProgress::Merge));
    assert_eq!(s.changes.conflicted, 1);
    assert_eq!(
        s.changes.staged + s.changes.unstaged,
        0,
        "conflicted paths counted once"
    );
    let is = issues(&s);
    assert_eq!(&is[..2], &[Issue::Conflicts(1), Issue::InProgress]);
}

#[test]
fn rebase_in_progress() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["switch", "-q", "-c", "other"]);
    write(&r, "README", "other\n");
    commit(&r, "other");
    git(&r, &["switch", "-q", "main"]);
    write(&r, "README", "main\n");
    commit(&r, "main");
    let _ = std::process::Command::new("git")
        .current_dir(&r)
        .args(["rebase", "other"])
        .output()
        .unwrap();
    assert_eq!(st(&r).in_progress, Some(InProgress::Rebase));
}

#[test]
fn worktrees_and_submodules_listed() {
    let sb = Sandbox::new();
    let lib = sb.init("lib");
    let r = sb.init("r");
    git(
        &r,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "vendor/lib",
        ],
    );
    commit(&r, "add sub");
    let wt = sb.path("r-feature");
    git(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    let s = st(&r);
    assert_eq!(s.worktrees, vec![wt.clone()]);
    assert_eq!(s.submodules, vec![r.join("vendor/lib")]);
    assert!(s.changes.is_clean());

    // dirty submodule makes the parent dirty
    write(&r.join("vendor/lib"), "README", "changed");
    let s = st(&r);
    assert_eq!(s.changes.unstaged, 1, "{:?}", s.changes.files);

    // worktree status is independent
    let ws = st(&wt);
    assert_eq!(ws.head, Head::Branch("feature".into()));
    assert!(ws.changes.is_clean());
    // regression: a linked worktree listed itself as a worktree, making it its own parent
    assert_eq!(ws.worktrees, Vec::<std::path::PathBuf>::new());
}

#[test]
fn bare_repo() {
    let sb = Sandbox::new();
    let bare = sb.remote("acme", "w");
    let s = st(&bare);
    assert!(s.bare);
    assert_eq!(s.head, Head::Branch("main".into()));
    assert!(s.changes.is_clean());
    assert_eq!(issues(&s), vec![], "bare repos don't nag about remotes");
}

#[test]
fn submodule_status_can_be_skipped() {
    let sb = Sandbox::new();
    let lib = sb.init("lib");
    let r = sb.init("r");
    git(
        &r,
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
    commit(&r, "add sub");
    write(&r.join("lib"), "README", "changed");
    let s = status(&r, StatusOptions { submodules: false }).unwrap();
    assert!(s.changes.is_clean());
}

/// Fork workflow seen in the wild: the branch tracks `upstream/master` but
/// was pushed to `origin/<branch>`. That work is safe on a remote.
fn fork_setup(sb: &Sandbox) -> std::path::PathBuf {
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
    r
}

#[test]
fn ahead_of_upstream_but_on_another_remote_is_not_unpushed() {
    let sb = Sandbox::new();
    let r = fork_setup(&sb);
    // inverse first: not pushed anywhere yet
    let s = st(&r);
    assert_eq!(s.ahead(), 1);
    assert!(issues(&s).contains(&Issue::Unpushed(1)), "{:?}", issues(&s));
    git(&r, &["push", "-q", "origin", "topic"]);
    let s = st(&r);
    assert_eq!(s.ahead(), 1, "still ahead of its upstream, as git reports");
    let topic = s.branches.iter().find(|b| b.name == "topic").unwrap();
    assert!(!topic.is_unpushed());
    let is = issues(&s);
    assert!(
        !is.iter().any(|i| matches!(i, Issue::Unpushed(_))),
        "{is:?}"
    );
    assert!(is.contains(&Issue::OffDefault));
}

#[test]
fn other_branch_pushed_to_any_remote_is_not_unpushed() {
    let sb = Sandbox::new();
    let r = fork_setup(&sb);
    git(&r, &["push", "-q", "origin", "topic"]);
    git(&r, &["branch", "--unset-upstream"]);
    git(&r, &["switch", "-q", "main"]);
    let is = issues(&st(&r));
    assert!(
        !is.iter().any(|i| matches!(i, Issue::UnpushedBranches(_))),
        "{is:?}"
    );
}
