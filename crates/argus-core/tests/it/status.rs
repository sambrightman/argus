//! Status detection against real repositories. Every detector is paired with
//! its inverse (the clean case must *not* report it).

use argus_core::attention::{Issue, issues};
use argus_core::status::{Head, InProgress, RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, git_may_fail, write};
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
    assert!(s.stashes.is_empty());
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
fn a_linked_worktree_sees_the_fetch_made_from_its_main_repo() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["worktree", "add", "-q", "../wt", "-b", "wt"]);
    let wt = sb.path("wt");
    std::fs::remove_file(r.join(".git/FETCH_HEAD")).ok(); // the clone's own
    assert!(st(&wt).last_fetch.is_none(), "no fetch yet");
    git(&r, &["fetch", "-q"]);
    assert!(
        st(&wt).last_fetch.is_some(),
        "FETCH_HEAD is in the common dir"
    );
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
    assert!(is.contains(&Issue::NonDefaultBranch));
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
    assert!(is.contains(&Issue::NonDefaultBranch));
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
    assert!(is.contains(&Issue::NonDefaultBranch));
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
fn stashes_read_newest_first_by_message_without_the_branch() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let wip = git(&r, &["log", "-1", "--format=%h %s"]);
    for (i, args) in [
        &["stash", "-q"][..],
        &["stash", "push", "-q", "-m", "note: later"],
        &["stash", "push", "-q", "-m", "mail me@host"],
    ]
    .into_iter()
    .enumerate()
    {
        write(&r, "README", &format!("{i}"));
        git(&r, args);
    }
    git(&r, &["switch", "-q", "--detach"]);
    write(&r, "README", "detached");
    git(&r, &["stash", "push", "-q", "-m", "from nowhere"]);
    let s = st(&r);
    assert_eq!(
        s.stashes,
        ["from nowhere", "mail me@host", "note: later", wip.trim()]
    );
    assert!(s.changes.is_clean());
    assert_eq!(
        issues(&s),
        vec![Issue::Detached, Issue::Stashes(4), Issue::NoRemote]
    );
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
    assert!(!git_may_fail(&r, &["merge", "other"]));
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
    assert!(!git_may_fail(&r, &["rebase", "other"]));
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

    // an untracked file inside a submodule is the submodule's too: not a
    // file staged or untracked here
    write(&r.join("vendor/lib"), "new-file", "x");
    let c = st(&r).changes;
    assert_eq!(
        (c.staged, c.untracked, c.submodule_content, c.total()),
        (0, 0, 1, 1),
        "{:?}",
        c.files
    );
    std::fs::remove_file(r.join("vendor/lib/new-file")).unwrap();

    // changes inside a submodule are listed in the parent, but are the
    // submodule's to fix
    write(&r.join("vendor/lib"), "README", "changed");
    let s = st(&r);
    assert_eq!(
        (
            s.changes.unstaged,
            s.changes.submodule_content,
            s.changes.total()
        ),
        (0, 1, 1),
        "{:?}",
        s.changes.files
    );
    assert!(!s.changes.is_clean());

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
    let s = status(
        &r,
        StatusOptions {
            submodules: false,
            ..Default::default()
        },
    )
    .unwrap();
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
    assert!(is.contains(&Issue::NonDefaultBranch));
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

#[test]
fn linked_worktree_does_not_repeat_other_branch_issues() {
    // Seen in the wild: main ahead of origin, plus a detached worktree.
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    commit(&r, "unpushed on main");
    let wt = sb.path("wt");
    git(
        &r,
        &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
    );
    let is = issues(&st(&wt));
    assert!(
        !is.iter().any(|i| matches!(i, Issue::UnpushedBranches(_))),
        "{is:?}"
    );
    assert!(is.contains(&Issue::Detached));
    // inverse: the main checkout still reports its own other unpushed branches
    git(&r, &["switch", "-q", "-c", "other"]);
    commit(&r, "other");
    git(&r, &["switch", "-q", "main"]);
    assert!(issues(&st(&r)).contains(&Issue::UnpushedBranches(1)));
}

#[test]
fn detached_commits_on_no_branch_are_flagged() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    git(&r, &["checkout", "-q", "--detach"]);
    // inverse first: detached on a pushed commit is merely "detached"
    let is = issues(&st(&r));
    assert!(
        is.contains(&Issue::Detached) && !is.iter().any(|i| matches!(i, Issue::DetachedCommits(_))),
        "{is:?}"
    );
    commit(&r, "orphan 1");
    commit(&r, "orphan 2");
    let is = issues(&st(&r));
    assert_eq!(is[0], Issue::DetachedCommits(2), "{is:?}");
    // once a branch holds them they're no longer at risk
    git(&r, &["branch", "keep"]);
    assert!(
        !issues(&st(&r))
            .iter()
            .any(|i| matches!(i, Issue::DetachedCommits(_)))
    );
}

#[test]
fn submodule_detached_head_is_normal() {
    // Seen in the wild: a submodule pinned to a commit its stale remote refs
    // don't contain was reported as 218 orphaned commits.
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
    commit(&r, "sub");
    // advance the submodule source, then pin the submodule to the new commit by id
    commit(&lib, "new upstream work");
    let id = git(&lib, &["rev-parse", "HEAD"]);
    let sub = r.join("lib");
    git(&sub, &["fetch", "-q", "origin", id.trim()]);
    git(&sub, &["checkout", "-q", id.trim()]);
    let s = st(&sub);
    assert!(matches!(s.head, Head::Detached(_)));
    let is = issues(&s);
    assert!(
        !is.iter()
            .any(|i| matches!(i, Issue::Detached | Issue::DetachedCommits(_))),
        "{is:?}"
    );
    // inverse: the parent sees the submodule moved
    assert_eq!(st(&r).changes.unstaged, 1);
    // a branch at the pinned commit isn't switched to: that's not normal
    git(&sub, &["branch", "here"]);
    let s = st(&sub);
    assert_eq!(s.at_head, ["here"]);
    let fixes = argus_core::fix::plan(&s, &Default::default());
    assert!(
        !fixes
            .iter()
            .any(|f| f.kind == argus_core::fix::FixKind::Reattach),
        "{fixes:?}"
    );
}

#[test]
fn tags_on_head_annotated_first_then_newest_version() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "second");
    git(&r, &["tag", "v1.9"]);
    git(&r, &["tag", "v1.10"]);
    git(&r, &["tag", "-a", "-m", "release", "release-1"]);
    git(&r, &["tag", "-a", "-m", "release", "release-2"]);
    git(&r, &["tag", "old", "HEAD~1"]); // elsewhere: not listed
    let check = |what: &str| {
        let s = st(&r);
        assert_eq!(
            s.tags,
            ["release-2", "release-1", "v1.10", "v1.9"],
            "{what}"
        );
        let id = git(&r, &["rev-parse", "HEAD"]);
        assert_eq!(s.head_commit.as_deref(), Some(id.trim()), "{what}: full id");
    };
    check("loose refs, on a branch");
    git(&r, &["pack-refs", "--all"]);
    check("packed refs");
    git(&r, &["switch", "-q", "--detach", "release-1"]);
    check("detached on the tag");
    // inverse: no tags on HEAD
    git(&r, &["switch", "-q", "--detach", "HEAD~1"]);
    git(&r, &["tag", "-d", "old"]);
    let s = st(&r);
    assert!(s.tags.is_empty(), "{:?}", s.tags);
    // unborn: no commit, no tags
    let u = sb.path("u");
    std::fs::create_dir_all(&u).unwrap();
    git(&u, &["init", "-q"]);
    let s = st(&u);
    assert!(s.tags.is_empty() && s.head_commit.is_none());
}

#[test]
fn a_broken_tag_is_skipped_but_a_broken_branch_fails_status() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["tag", "v1"]);
    std::fs::write(r.join(".git/refs/tags/bad"), "garbage\n").unwrap();
    let s = st(&r);
    assert_eq!(s.tags, ["v1"]);
    assert_eq!(s.head, Head::Branch("main".into()));
    // inverse: a broken branch can't be reported on, so status says so
    std::fs::write(r.join(".git/refs/heads/bad"), "garbage\n").unwrap();
    assert!(status(&r, StatusOptions::default()).is_err());
}

#[test]
fn a_detached_head_after_a_tag_is_described_as_git_does() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["tag", "-a", "-m", "one", "v1"]);
    commit(&r, "a");
    commit(&r, "b");
    git(&r, &["switch", "-q", "--detach"]);
    let want = git(&r, &["describe", "--tags"]);
    assert_eq!(st(&r).describe.as_deref(), Some(want.trim()));
    assert!(want.starts_with("v1-2-g"), "{want}");
    // a nearer tag, same HEAD: the remembered answer is replaced
    git(&r, &["tag", "v2", "HEAD~1"]);
    let want = git(&r, &["describe", "--tags"]);
    assert!(want.starts_with("v2-1-g"), "{want}");
    assert_eq!(st(&r).describe.as_deref(), Some(want.trim()));
    // renamed, same commit: the name changes too
    git(&r, &["tag", "v2-final", "v2"]);
    git(&r, &["tag", "-d", "v2"]);
    let want = git(&r, &["describe", "--tags"]);
    assert!(want.starts_with("v2-final-1-g"), "{want}");
    assert_eq!(st(&r).describe.as_deref(), Some(want.trim()));
    git(&r, &["tag", "-d", "v2-final"]);
    // inverses: on the tag (the tags say it), on a branch, no tags at all
    git(&r, &["switch", "-q", "--detach", "v1"]);
    assert_eq!(st(&r).describe, None, "on a tag");
    git(&r, &["switch", "-q", "main"]);
    assert_eq!(st(&r).describe, None, "on a branch");
    git(&r, &["switch", "-q", "--detach"]);
    git(&r, &["tag", "-d", "v1"]);
    assert_eq!(st(&r).describe, None, "no tags");
    // a tag only on another branch isn't in HEAD's history
    git(&r, &["switch", "-q", "--orphan", "other"]);
    commit(&r, "other");
    git(&r, &["tag", "elsewhere"]);
    git(&r, &["switch", "-q", "--detach", "main"]);
    assert_eq!(st(&r).describe, None, "unreachable tag");
}

#[test]
fn describe_gives_up_on_a_tag_more_than_1000_commits_back() {
    use std::io::Write;
    let sb = Sandbox::new();
    let r = sb.path("r");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    // `n` more commits on main (timestamps from `t`), continuing it if `from`
    let commits = |n: usize, t: usize, from: bool| {
        let mut script = String::new();
        for i in 0..n {
            script.push_str(&format!(
                "commit refs/heads/main\ncommitter a <a@b> {} +0000\ndata 1\nc\n",
                1_700_000_000 + t + i
            ));
            if i == 0 && from {
                script.push_str("from refs/heads/main^0\n");
            }
        }
        script
    };
    let import = |script: String| {
        let mut p = std::process::Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(&r)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        p.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(p.wait().unwrap().success());
    };
    import(commits(1, 0, false));
    git(&r, &["tag", "v1"]);
    import(commits(1000, 1, true));
    git(&r, &["switch", "-q", "--detach", "main"]);
    let depth: usize = git(&r, &["rev-list", "--count", "v1..HEAD"])
        .trim()
        .parse()
        .unwrap();
    assert_eq!(depth, 1000);
    assert!(st(&r).describe.is_some(), "1000 back: found");
    import(commits(1, 1001, true));
    git(&r, &["switch", "-q", "--detach", "main"]);
    assert_eq!(st(&r).describe, None, "1001 back: given up");
}

/// Status with this `describe_limit`.
fn st_limit(p: &std::path::Path, limit: usize) -> RepoStatus {
    let opts = StatusOptions {
        describe_limit: limit,
        ..Default::default()
    };
    status(p, opts).unwrap()
}

#[test]
fn the_describe_limit_is_configurable_and_says_when_it_gave_up() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["tag", "v1"]);
    for i in 0..4 {
        commit(&r, &format!("c{i}"));
    }
    git(&r, &["switch", "-q", "--detach"]);
    // the tag is 4 back
    let s = st_limit(&r, 3);
    assert_eq!((s.describe, s.describe_gave_up), (None, true));
    // a longer walk isn't answered by the shorter one that gave up
    let s = st_limit(&r, 4);
    assert!(
        s.describe
            .as_deref()
            .is_some_and(|d| d.starts_with("v1-4-g")),
        "{s:?}"
    );
    assert!(!s.describe_gave_up);
    // 0: no walk at all, left to the details
    let r2 = sb.init("r2");
    git(&r2, &["tag", "v1"]);
    commit(&r2, "after");
    git(&r2, &["switch", "-q", "--detach"]);
    let s = st_limit(&r2, 0);
    assert_eq!((s.describe, s.describe_gave_up), (None, true));
}

#[test]
fn running_out_of_history_before_the_limit_is_a_final_answer() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "b");
    git(&r, &["switch", "-q", "--orphan", "other"]);
    commit(&r, "other");
    git(&r, &["tag", "elsewhere"]);
    git(&r, &["switch", "-q", "--detach", "main"]);
    let s = st_limit(&r, 100);
    assert_eq!(
        (s.describe, s.describe_gave_up),
        (None, false),
        "2 commits, no tag"
    );
}

#[test]
fn a_history_exactly_the_limit_long_with_no_tag_is_a_final_answer() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "b");
    commit(&r, "c"); // 3 commits
    git(&r, &["switch", "-q", "--orphan", "other"]);
    commit(&r, "other");
    git(&r, &["tag", "elsewhere"]);
    git(&r, &["switch", "-q", "--detach", "main"]);
    let s = st_limit(&r, 3);
    assert_eq!((s.describe, s.describe_gave_up), (None, false));
    // one short of the history: it might be further back
    let r2 = sb.init("r2");
    commit(&r2, "b");
    commit(&r2, "c");
    git(&r2, &["tag", "elsewhere", "HEAD~2"]);
    git(&r2, &["switch", "-q", "--detach"]);
    // the tag is 2 back: a limit of 1 gives up
    assert!(st_limit(&r2, 1).describe_gave_up);
}

#[test]
fn describing_fully_finds_a_tag_past_the_limit_and_is_remembered() {
    use argus_core::status::describe_fully;
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["tag", "v1"]);
    for i in 0..4 {
        commit(&r, &format!("c{i}"));
    }
    git(&r, &["switch", "-q", "--detach"]);
    assert!(st_limit(&r, 2).describe_gave_up);
    let want = git(&r, &["describe", "--tags"]);
    assert_eq!(describe_fully(&r).as_deref(), Some(want.trim()));
    // the next status uses it, without walking again
    let s = st_limit(&r, 2);
    assert_eq!(s.describe.as_deref(), Some(want.trim()));
    assert!(!s.describe_gave_up);
}

#[test]
fn version_order_compares_numbers_as_numbers() {
    use argus_core::status::version_cmp;
    use std::cmp::Ordering::*;
    for (a, b, o) in [
        ("v1.10", "v1.9", Greater),
        ("v2", "v10", Less),
        ("1.0.1", "1.0", Greater),
        ("v1.01", "v1.1", Less), // equal numerically; tie broken by text
        ("v1.0-rc1", "v1.0-rc2", Less),
        ("", "a", Less),
        ("abc", "abc", Equal),
    ] {
        assert_eq!(version_cmp(a, b), o, "{a} vs {b}");
        assert_eq!(version_cmp(b, a), o.reverse(), "{b} vs {a}");
    }
}

#[test]
fn a_local_master_is_the_default_branch_when_nothing_says_otherwise() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["branch", "-q", "-m", "master"]);
    git(&r, &["config", "init.defaultBranch", "trunk"]);
    assert_eq!(st(&r).default_branch.as_deref(), Some("master"));
    // inverse: neither main nor master, so the configured name
    git(&r, &["branch", "-q", "-m", "dev"]);
    assert_eq!(st(&r).default_branch.as_deref(), Some("trunk"));
}

#[test]
fn a_detached_submodule_isnt_checked_for_orphaned_commits() {
    // detached is how submodules are checked out; the parent records the commit
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
    let sub = r.join("lib");
    git(&sub, &["switch", "-q", "--detach"]);
    commit(&sub, "only here");
    let s = st(&sub);
    assert!(s.submodule && matches!(s.head, Head::Detached(_)));
    assert_eq!(s.detached_orphans, 0);
    // inverse: the same in a repo of its own counts
    git(&lib, &["switch", "-q", "--detach"]);
    commit(&lib, "only here");
    assert_eq!(st(&lib).detached_orphans, 1);
}

#[test]
fn untracked_listings_say_whether_there_are_more() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    for f in ["a", "b", "c"] {
        write(&r, &format!("d/{f}"), "x");
    }
    let list = |limit| {
        let (v, more) = argus_core::status::untracked_in(&r, "d/", limit).unwrap();
        (v.into_iter().map(|(p, _)| p).collect::<Vec<String>>(), more)
    };
    assert_eq!(
        list(3),
        (["d/a", "d/b", "d/c"].map(String::from).to_vec(), false)
    );
    assert_eq!(list(2), (["d/a", "d/b"].map(String::from).to_vec(), true));
}

#[test]
fn nested_repos_are_told_from_files() {
    use argus_core::status::PathKind;
    let sb = Sandbox::new();
    let r = sb.init("r");
    commit(&r, "r");
    write(&r, "d/file", "x");
    sb.init("r/d/inner");
    sb.init("r/top");
    write(&r, "loose", "x");
    let (v, _) = argus_core::status::untracked_in(&r, "d/", 10).unwrap();
    assert_eq!(
        v,
        [
            ("d/file".into(), PathKind::File),
            ("d/inner".into(), PathKind::Repo),
        ]
    );
    let kinds: Vec<_> = st(&r)
        .changes
        .files
        .into_iter()
        .map(|f| (f.path, f.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("d/".into(), PathKind::File),
            ("loose".into(), PathKind::File),
            ("top".into(), PathKind::Repo),
        ]
    );
}

#[test]
fn files_are_listed_up_to_the_cap_and_all_counted() {
    use argus_core::status::MAX_FILES;
    let sb = Sandbox::new();
    let r = sb.init("r");
    for i in 0..=MAX_FILES {
        write(&r, &format!("f{i:04}"), "x");
    }
    let c = st(&r).changes;
    assert_eq!((c.files.len(), c.untracked), (MAX_FILES, MAX_FILES + 1));
}

/// A clone with `f` (20 lines) on main, and a pushed branch `feat` that
/// changes line 10.
fn feature_changing_line_10(sb: &Sandbox) -> std::path::PathBuf {
    sb.remote("acme", "sq");
    let r = sb.clone_as_github("acme", "sq", "dev/sq");
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    write(&r, "f", &lines);
    commit(&r, "f");
    git(&r, &["push", "-q"]);
    git(&r, &["switch", "-q", "-c", "feat"]);
    write(&r, "f", &lines.replace("line 10\n", "line ten\n"));
    commit(&r, "ten");
    write(&r, "g", "new file\n");
    commit(&r, "g");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    git(&r, &["switch", "-q", "main"]);
    r
}

/// As a forge does it: main gets the branch's whole change as one new
/// commit, and the remote branch is deleted.
fn squash_merge(r: &std::path::Path) {
    git(r, &["merge", "-q", "--squash", "feat"]);
    git(r, &["commit", "-q", "-m", "feat (#1)"]);
    git(r, &["push", "-q"]);
    git(r, &["push", "-q", "origin", "--delete", "feat"]);
    git(r, &["fetch", "-q", "--prune"]);
}

fn replace_in(r: &std::path::Path, file: &str, from: &str, to: &str) {
    let text = std::fs::read_to_string(r.join(file)).unwrap();
    assert!(text.contains(from));
    write(r, file, &text.replace(from, to));
}

fn feat(r: &std::path::Path) -> argus_core::status::BranchInfo {
    st(r)
        .branches
        .into_iter()
        .find(|b| b.name == "feat")
        .unwrap()
}

/// A branch is absorbed when merging it into the default branch would
/// change nothing: squash-merged (even with main changing nearby lines
/// first, which makes the patches differ) or rebase-merged. Its commits
/// are on no remote, but nothing of it is unpushed.
#[test]
fn a_squash_or_rebase_merged_branch_is_absorbed() {
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    let b = feat(&r);
    assert!(!b.merged && !b.absorbed && !b.landed(), "not yet: {b:?}");
    // main changes a line near the branch's, then squash-merges it
    replace_in(&r, "f", "line 12\n", "line twelve\n");
    commit(&r, "twelve");
    squash_merge(&r);
    let b = feat(&r);
    assert!(!b.merged, "its commits aren't in main's history");
    assert!(b.absorbed && b.landed(), "{b:?}");
    assert!(b.upstream.as_ref().unwrap().gone);
    assert_eq!(b.unpushed, Some(0), "its changes are on the remote's main");
    assert!(!b.is_unpushed());

    // rebase-merged: the same changes as new commits
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    replace_in(&r, "f", "line 2\n", "line two\n");
    commit(&r, "two");
    git(&r, &["cherry-pick", "main..feat"]);
    git(&r, &["push", "-q"]);
    let b = feat(&r);
    assert!(!b.merged && b.absorbed, "{b:?}");
}

/// Not absorbed: main reverted the change (merging would bring it back),
/// or changed the same lines again (can't tell: left alone), or never had
/// it. A branch in main's history is merged, which isn't checked again.
#[test]
fn a_branch_main_does_not_hold_is_not_absorbed() {
    // reverted on main
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    squash_merge(&r);
    assert!(feat(&r).absorbed);
    git(&r, &["revert", "--no-edit", "HEAD"]);
    git(&r, &["push", "-q"]);
    let b = feat(&r);
    assert!(!b.absorbed && !b.landed(), "{b:?}");
    assert!(b.is_unpushed(), "its work is on no remote");

    // the same line changed again on main
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    squash_merge(&r);
    replace_in(&r, "f", "line ten\n", "line X\n");
    commit(&r, "again");
    git(&r, &["push", "-q"]);
    assert!(!feat(&r).absorbed);

    // part of it only: one of its two commits
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    git(&r, &["cherry-pick", "feat~1"]);
    git(&r, &["push", "-q"]);
    assert!(!feat(&r).absorbed, "g isn't in main");

    // no net change of its own (empty commits, or a change and its
    // revert): nothing was merged anywhere; its commits are still unpushed
    let sb = Sandbox::new();
    sb.remote("acme", "e");
    let r = sb.clone_as_github("acme", "e", "dev/e");
    git(&r, &["switch", "-q", "-c", "feat"]);
    git(&r, &["commit", "-q", "--allow-empty", "-m", "note"]);
    write(&r, "f", "x\n");
    commit(&r, "x");
    git(&r, &["revert", "--no-edit", "HEAD"]);
    let b = feat(&r);
    assert!(!b.absorbed && b.is_unpushed(), "{b:?}");

    // in main's history: merged, nothing more to ask
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    git(&r, &["merge", "-q", "--no-edit", "--no-ff", "feat"]);
    git(&r, &["push", "-q"]);
    let b = feat(&r);
    assert!(b.merged && !b.absorbed && b.landed(), "{b:?}");
}

/// Absorbed into a main that's only local: nothing of it is on a remote,
/// so it's still unpushed. And status writes nothing to the repo.
#[test]
fn absorbed_into_a_local_main_is_still_unpushed_and_nothing_is_written() {
    let sb = Sandbox::new();
    let r = feature_changing_line_10(&sb);
    // feat's own remote branch gone before the local squash
    git(&r, &["push", "-q", "origin", "--delete", "feat"]);
    git(&r, &["fetch", "-q", "--prune"]);
    replace_in(&r, "f", "line 12\n", "line twelve\n");
    commit(&r, "twelve");
    git(&r, &["merge", "-q", "--squash", "feat"]);
    git(&r, &["commit", "-q", "-m", "squashed, not pushed"]);
    let objects = || git(&r, &["count-objects", "-v"]);
    let before = objects();
    let b = feat(&r);
    assert!(
        !b.absorbed,
        "origin/main, what merged means, doesn't have it: {b:?}"
    );
    assert!(b.is_unpushed());
    assert_eq!(objects(), before, "the trial merge stayed in memory");
    git(&r, &["push", "-q"]);
    let b = feat(&r);
    assert!(b.absorbed && !b.is_unpushed(), "{b:?}");
    assert_eq!(objects(), before);
}

#[test]
fn other_remotes_are_listed_by_url_and_their_owners_may_head_pull_requests() {
    use argus_core::identity::RepoId;
    let sb = Sandbox::new();
    sb.remote("up", "w");
    let r = sb.clone_as_github("up", "w", "dev/w");
    assert!(st(&r).other_remotes.is_empty(), "only origin");
    git(&r, &["remote", "add", "fork", "git@github.com:Me/w.git"]);
    git(
        &r,
        &["remote", "add", "mirror", "https://gitlab.com/them/w.git"],
    );
    git(&r, &["remote", "add", "again", "https://github.com/me/w"]);
    git(&r, &["remote", "add", "local", "/srv/git/w"]);
    let s = st(&r);
    assert_eq!(
        s.other_remotes,
        [
            "https://github.com/me/w",
            "git@github.com:Me/w.git",
            "/srv/git/w",
            "https://gitlab.com/them/w.git"
        ],
        "by remote name, as configured; not origin's"
    );
    // whose pull requests from this clone's branches may be: the owners
    // of its other remotes on the same host, each once, not origin's own
    let id = RepoId::from_url(s.remote_url.as_deref().unwrap()).unwrap();
    assert_eq!(argus_core::forge::head_owners(&id, &s), ["me"]);
    // inverse: with origin the fork, the upstream's owner is the other
    let me = RepoId::new("github.com", "me", "w");
    let mut t = s.clone();
    t.other_remotes = vec!["git@github.com:up/w.git".into()];
    assert_eq!(argus_core::forge::head_owners(&me, &t), ["up"]);
    t.other_remotes = vec!["git@github.com:ME/other.git".into()];
    assert!(argus_core::forge::head_owners(&me, &t).is_empty());
}

/// The same in a folder with nothing else tracked: git shows the folder
/// as untracked. Counted once as that, once as the deletion; and a
/// commit of everything is expected to leave nothing.
#[test]
fn a_folders_last_tracked_file_taken_out_of_the_index_counts_once_each_way() {
    use argus_core::fix::{Fix, FixKind, simulate};
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "dir/f", "f\n");
    write(&r, "other", "o\n");
    git(&r, &["add", "."]);
    commit(&r, "files");
    git(&r, &["rm", "-q", "--cached", "dir/f"]);
    assert_eq!(git(&r, &["status", "--short"]), "D  dir/f\n?? dir/\n");
    let s = st(&r);
    let c = &s.changes;
    assert_eq!(
        (c.staged, c.unstaged, c.untracked),
        (1, 0, 1),
        "{:?}",
        c.files
    );
    // a plain file likewise, for what a commit of everything leaves
    for path in ["dir/f", "other"] {
        git(&r, &["add", "."]);
        git(&r, &["rm", "-q", "--cached", path]);
        let s = st(&r);
        let commit_all = Fix {
            kind: FixKind::Commit,
            op: argus_core::ops::Op::CommitAll {
                message: "m".into(),
            },
            reason: String::new(),
            blocked: None,
        };
        let after = simulate(&s, &commit_all);
        assert!(after.changes.is_clean(), "{path}: {:?}", after.changes);
    }
}

#[test]
fn a_file_taken_out_of_the_index_but_kept_is_a_staged_deletion_and_an_untracked_file() {
    use argus_core::ops;
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "keep", "k\n");
    write(&r, "other", "o\n");
    git(&r, &["add", "."]);
    commit(&r, "files");
    // as git has it: `D  keep` and `?? keep`
    git(&r, &["rm", "-q", "--cached", "keep"]);
    let short = git(&r, &["status", "--short"]);
    assert_eq!(short, "D  keep\n?? keep\n");
    for _ in 0..5 {
        // (whichever of the two the walk reports first)
        let s = st(&r);
        let f = s.changes.entry("keep").unwrap();
        assert_eq!((f.index, f.worktree), ('D', '?'));
        assert!(f.untracked());
        let c = &s.changes;
        assert_eq!((c.staged, c.unstaged, c.untracked), (1, 0, 1));
    }
    let s = st(&r);
    let keep = vec!["keep".to_string()];
    // staging the file that's there adds it; discarding deletes it, as
    // any untracked file; unstaging undoes the deletion
    assert_eq!(
        ops::stage_ops(&s, &keep),
        [ops::Op::Add {
            paths: keep.clone()
        }]
    );
    assert_eq!(
        ops::discard_ops(&s, &keep),
        [ops::Op::Clean {
            paths: keep.clone()
        }]
    );
    for op in ops::unstage_ops(&s, &keep) {
        assert!(ops::run(&r, &op).success);
    }
    assert_eq!(git(&r, &["status", "--short"]), "");
    // inverse: an ordinary untracked file, and a staged deletion alone
    write(&r, "new", "n\n");
    git(&r, &["rm", "-q", "other"]);
    let s = st(&r);
    let (new, other) = (
        s.changes.entry("new").unwrap(),
        s.changes.entry("other").unwrap(),
    );
    assert_eq!((new.index, new.worktree, new.untracked()), ('?', '?', true));
    assert_eq!(
        (other.index, other.worktree, other.untracked()),
        ('D', ' ', false)
    );
    assert_eq!(
        (s.changes.staged, s.changes.unstaged, s.changes.untracked),
        (1, 0, 1)
    );
}

/// What each status's `files` step says, while `f` runs, without its repo.
fn files_said(f: impl FnOnce()) -> Vec<String> {
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let into = got.clone();
    argus_core::trace::set_sink(Some((
        argus_core::trace::Form::Each,
        Arc::new(move |s: argus_core::trace::Step| {
            // (said with its repo: `status ~/r: files: …`)
            if let Some(at) = s.what.find(": files:") {
                into.lock().unwrap().push(s.what[at + 2..].to_string());
            }
        }),
    )));
    f();
    argus_core::trace::set_sink(None);
    got.lock().unwrap().clone()
}

/// The `files` step says what the status read: files whose details on
/// disk no longer match the index's are read again, those unchanged after
/// all counted (they stay so, as argus doesn't write the index), as are
/// those the index can't vouch for, being as new as it; and how far the
/// walk for untracked files went.
#[test]
fn the_files_step_says_what_was_read_to_find_changes() {
    let sb = Sandbox::new();
    let r = sb.path("r");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    write(&r, "a", "aaaa\n");
    write(&r, "b", "bb\n");
    write(&r, "c", "c\n");
    commit(&r, "three");
    let at = |f: &str, year: u64| {
        let t =
            std::time::UNIX_EPOCH + std::time::Duration::from_secs((year - 1970) * 365 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(r.join(f))
            .unwrap()
            .set_modified(t)
            .unwrap();
    };
    // all older than the index, which then says so: nothing to read
    for f in ["a", "b", "c"] {
        at(f, 2001);
    }
    git(&r, &["update-index", "-q", "--refresh"]);
    let said = || files_said(|| drop(st(&r)));
    assert_eq!(
        said(),
        [
            "files: 3 tracked, 0 read (0 bytes): 0 unchanged, 0 too new to trust; 1 folders walked, 4 entries"
        ]
    );
    // a touched: read again, unchanged, and so at every status
    at("a", 2002);
    let touched = "files: 3 tracked, 1 read (5 bytes): 1 unchanged, 0 too new to trust; 1 folders walked, 4 entries";
    assert_eq!(said(), [touched]);
    assert_eq!(said(), [touched]);
    // c as the index has it, but newer than the index: read, untrusted
    at("c", 2099);
    git(&r, &["update-index", "-q", "--refresh"]);
    assert_eq!(
        said(),
        [
            "files: 3 tracked, 1 read (2 bytes): 1 unchanged, 1 too new to trust; 1 folders walked, 4 entries"
        ]
    );
    // untracked folders walked
    at("c", 2001);
    git(&r, &["update-index", "-q", "--refresh"]);
    write(&r.join("u/v"), "x", "");
    assert_eq!(
        said(),
        [
            "files: 3 tracked, 0 read (0 bytes): 0 unchanged, 0 too new to trust; 3 folders walked, 5 entries"
        ]
    );
}
