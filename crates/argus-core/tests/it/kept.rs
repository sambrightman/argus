//! Repos kept open between status reads: reused while their config is
//! unchanged, and always agreeing with a fresh open.

use argus_core::status::{KeptRepos, RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};
use pretty_assertions::assert_eq;
use std::path::Path;

fn fresh(p: &Path) -> RepoStatus {
    status(p, StatusOptions::default()).unwrap()
}

fn kept(k: &KeptRepos, p: &Path) -> RepoStatus {
    k.status(p, StatusOptions::default()).unwrap()
}

fn url(k: &KeptRepos, p: &Path) -> Option<String> {
    kept(k, p).remote_url
}

#[test]
fn a_repo_is_opened_once_while_its_config_is_unchanged() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let k = KeptRepos::default();
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(k.opens(), 1);
}

#[test]
fn each_repo_is_kept_separately() {
    let sb = Sandbox::new();
    let (a, b) = (sb.init("a"), sb.init("b"));
    let k = KeptRepos::default();
    for _ in 0..2 {
        assert_eq!(kept(&k, &a), fresh(&a));
        assert_eq!(kept(&k, &b), fresh(&b));
    }
    assert_eq!(k.opens(), 2);
}

#[test]
fn work_in_a_kept_repo_shows_up_without_reopening() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let k = KeptRepos::default();
    let step = |what: &str| assert_eq!(kept(&k, &r), fresh(&r), "after {what}");
    step("cloning");
    write(&r, "a", "1");
    step("an untracked file");
    commit(&r, "a");
    step("a commit");
    git(&r, &["switch", "-q", "-c", "feat"]);
    step("a new branch");
    git(&r, &["tag", "v1"]);
    step("a tag");
    git(&r, &["switch", "-q", "--detach", "v1"]);
    step("detaching");
    git(&r, &["switch", "-q", "main"]);
    sb.advance_remote("acme", "w", "b");
    git(&r, &["-c", "fetch.unpackLimit=1", "fetch", "-q"]);
    step("a fetch into a new pack");
    git(&r, &["gc", "-q", "--prune=now"]);
    step("gc replacing the packs and packing the refs");
    git(&r, &["stash", "-q", "-u"]);
    step("stashing");
}

#[test]
fn work_that_leaves_config_index_and_packs_alone_does_not_reopen() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let k = KeptRepos::default();
    let step = |what: &str| assert_eq!(kept(&k, &r), fresh(&r), "after {what}");
    step("cloning");
    write(&r, "untracked", "1");
    step("an untracked file");
    write(&r, "README", "changed\n");
    step("a modified file");
    git(&r, &["tag", "v1"]);
    git(&r, &["branch", "feat"]);
    step("a tag and a branch");
    sb.advance_remote("acme", "w", "b");
    git(&r, &["fetch", "-q"]);
    step("a fetch of loose objects");
    std::fs::write(r.join(".gitignore"), "untracked\n").unwrap();
    step("a new .gitignore");
    std::fs::write(r.join(".git/info/exclude"), ".gitignore\n").unwrap();
    step("info/exclude");
    assert_eq!(k.opens(), 1);
}

#[test]
fn a_rewritten_index_reopens_even_with_the_same_time_and_size() {
    // as on a file system with whole-second times
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a", "1");
    git(&r, &["add", "a"]);
    let k = KeptRepos::default();
    assert_eq!(kept(&k, &r), fresh(&r));
    let index = r.join(".git/index");
    let before = std::fs::metadata(&index).unwrap();
    write(&r, "a", "2");
    git(&r, &["add", "a"]);
    let f = std::fs::File::options().write(true).open(&index).unwrap();
    f.set_modified(before.modified().unwrap()).unwrap();
    assert_eq!(f.metadata().unwrap().len(), before.len(), "same size");
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(k.opens(), 2);
}

#[test]
fn new_packs_reopen_so_replaced_ones_are_let_go() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let k = KeptRepos::default();
    kept(&k, &r);
    sb.advance_remote("acme", "w", "b");
    git(&r, &["-c", "fetch.unpackLimit=1", "fetch", "-q"]);
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(k.opens(), 2, "a fetch into a new pack");
    git(&r, &["gc", "-q", "--prune=now"]);
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(k.opens(), 3, "gc");
}

#[test]
fn a_kept_repo_is_checked_against_the_files_that_matter() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let k = KeptRepos::default();
    kept(&k, &r);
    let checked = k.checked(&r);
    for want in [
        r.join(".git/config"),
        r.join(".git/index"),
        r.join(".git/objects/pack"),
    ] {
        assert!(checked.contains(&want), "{want:?} in {checked:?}");
    }
    assert!(!checked.contains(&r.join(".git/HEAD")), "no branch config");
    if std::env::var_os("GIT_CONFIG_GLOBAL").is_none()
        && let Some(home) = std::env::var_os("HOME")
    {
        let global = std::path::PathBuf::from(home).join(".gitconfig");
        assert!(checked.contains(&global), "even if absent: {checked:?}");
    }
}

#[test]
fn a_change_to_the_repos_config_reopens_it() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "w");
    let k = KeptRepos::default();
    assert_eq!(
        url(&k, &r).as_deref(),
        Some("https://github.com/acme/w.git")
    );
    git(
        &r,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/x.git",
        ],
    );
    assert_eq!(
        url(&k, &r).as_deref(),
        Some("https://github.com/acme/x.git")
    );
    git(&r, &["branch", "--unset-upstream"]);
    assert_eq!(kept(&k, &r), fresh(&r));
    assert_eq!(kept(&k, &r).upstream, None);
    assert_eq!(k.opens(), 3);
}

#[test]
fn a_change_to_an_included_file_reopens_the_repo() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    std::fs::write(
        r.join(".git/extra"),
        "[remote \"origin\"]\n\turl = https://a/x\n",
    )
    .unwrap();
    git(&r, &["config", "include.path", "extra"]);
    let k = KeptRepos::default();
    assert_eq!(url(&k, &r).as_deref(), Some("https://a/x"));
    std::fs::write(
        r.join(".git/extra"),
        "[remote \"origin\"]\n\turl = https://b/longer\n",
    )
    .unwrap();
    assert_eq!(url(&k, &r).as_deref(), Some("https://b/longer"));
    assert_eq!(k.opens(), 2);
}

#[test]
fn switching_onto_a_branch_with_its_own_config_reopens_the_repo() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    std::fs::write(
        r.join(".git/feat"),
        "[remote \"origin\"]\n\turl = https://a/feat\n",
    )
    .unwrap();
    git(&r, &["config", "includeIf.onbranch:feat.path", "feat"]);
    git(&r, &["branch", "feat"]);
    let k = KeptRepos::default();
    assert_eq!(url(&k, &r), None, "no remote on main");
    git(&r, &["switch", "-q", "feat"]);
    assert_eq!(url(&k, &r).as_deref(), Some("https://a/feat"));
    assert_eq!(kept(&k, &r), fresh(&r));
    git(&r, &["switch", "-q", "main"]);
    assert_eq!(url(&k, &r), None, "and back");
}

#[test]
fn a_linked_worktree_follows_config_changed_from_its_main_repo() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    git(&r, &["remote", "add", "origin", "https://a/one"]);
    git(&r, &["worktree", "add", "-q", "../wt", "-b", "wt"]);
    let wt = sb.path("wt");
    let k = KeptRepos::default();
    assert_eq!(url(&k, &wt).as_deref(), Some("https://a/one"));
    git(&r, &["remote", "set-url", "origin", "https://a/two"]);
    assert_eq!(url(&k, &wt).as_deref(), Some("https://a/two"));
}

#[test]
fn a_deleted_repo_is_an_error_and_a_recreated_one_is_read_afresh() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let k = KeptRepos::default();
    kept(&k, &r);
    std::fs::remove_dir_all(&r).unwrap();
    assert!(k.status(&r, StatusOptions::default()).is_err());
    let r = sb.init("r");
    git(&r, &["remote", "add", "origin", "https://a/new"]);
    assert_eq!(kept(&k, &r), fresh(&r));
}

#[test]
fn retain_forgets_the_other_repos() {
    let sb = Sandbox::new();
    let (a, b) = (sb.init("a"), sb.init("b"));
    let k = KeptRepos::default();
    kept(&k, &a);
    kept(&k, &b);
    k.retain(|p| p == a);
    kept(&k, &a);
    assert_eq!(k.opens(), 2, "a still kept");
    kept(&k, &b);
    assert_eq!(k.opens(), 3, "b reopened");
}
