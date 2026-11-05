//! Sample repositories in every state argus shows, for `argus demo`: a way
//! to see the whole UI, and to try themes and icon sets, without touching
//! real repos. Built with the test fixtures, so git runs with a private
//! config (no signing, hooks or user settings).

use std::path::PathBuf;

use crate::testkit::{Sandbox, commit, git, git_may_fail, write};
use crate::workspace::Workspace;

pub struct Demo {
    /// Owns the temporary directory; dropping it deletes everything.
    pub sandbox: Sandbox,
    /// The directory to scan.
    pub root: PathBuf,
}

/// Build the sample repos (takes a second or two: many git commands).
pub fn build() -> Demo {
    let sb = Sandbox::new();
    let clone = |name: &str| {
        sb.remote("demo", name);
        sb.clone_as_github("demo", name, &format!("repos/{name}"))
    };

    clone("clean");

    let r = clone("changes");
    write(&r, "new.txt", "staged\n");
    git(&r, &["add", "new.txt"]);
    write(&r, "README", "edited\n");
    write(&r, "scratch-1.txt", "untracked\n");
    write(&r, "scratch-2.txt", "untracked\n");

    let r = clone("ahead");
    commit(&r, "local work");
    commit(&r, "more local work");

    let r = clone("behind");
    sb.advance_remote("demo", "behind", "news.txt");
    sb.advance_remote("demo", "behind", "more-news.txt");
    git(&r, &["fetch", "-q"]);

    // diverged, and a trial merge predicts a conflict: pull isn't automatic
    let r = sb.diverged("demo", "diverged", "repos/diverged", true);
    git(&r, &["config", "pull.rebase", "false"]);

    let r = clone("feature");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feature work");
    git(&r, &["push", "-q", "-u", "origin", "feat"]);

    let r = clone("unpublished");
    git(&r, &["switch", "-q", "-c", "wip"]);
    commit(&r, "not pushed anywhere");

    // upstream deleted on the remote, with unmerged commits locally
    let r = clone("gone");
    git(&r, &["switch", "-q", "-c", "old-feature"]);
    commit(&r, "unmerged");
    git(&r, &["push", "-q", "-u", "origin", "old-feature"]);
    git(&r, &["push", "-q", "origin", "--delete", "old-feature"]);
    git(&r, &["fetch", "-q", "--prune"]);

    let r = clone("merging");
    conflicting_branch(&r);
    git_may_fail(&r, &["merge", "--no-edit", "other"]);

    let r = clone("rebasing");
    conflicting_branch(&r);
    git_may_fail(&r, &["rebase", "other"]);

    let r = clone("tagged");
    git(&r, &["tag", "v1.0"]);
    git(&r, &["tag", "-a", "-m", "release", "release-1.0"]);
    git(&r, &["switch", "-q", "--detach", "v1.0"]);

    // detached, with a commit no branch or tag holds
    let r = clone("detached");
    git(&r, &["switch", "-q", "--detach"]);
    commit(&r, "experiment");

    let r = clone("stashed");
    for i in 1..=2 {
        write(&r, "README", &format!("edit {i}\n"));
        git(&r, &["stash", "-q"]);
    }

    let r = clone("branches");
    git(&r, &["switch", "-q", "-c", "topic"]);
    commit(&r, "unpushed topic work");
    git(&r, &["switch", "-q", "main"]);

    sb.init("repos/local-only");

    let r = clone("worktrees");
    let wt = sb.path("repos/worktrees-feat");
    git(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-feat",
            wt.to_str().unwrap(),
        ],
    );

    let r = clone("with-submodule");
    let lib = sb.remote("demo", "lib");
    git(
        &r,
        &["submodule", "add", "-q", lib.to_str().unwrap(), "lib"],
    );
    commit(&r, "add submodule");
    git(&r, &["push", "-q"]);

    git(
        &sb.root(),
        &[
            "clone",
            "-q",
            "--bare",
            sb.path("forge/demo/clean.git").to_str().unwrap(),
            sb.path("repos/mirror.git").to_str().unwrap(),
        ],
    );

    for name in ["favourite", "new", "excluded", "snoozed"] {
        clone(name);
    }

    // unreadable: status reports an error
    let broken = sb.path("repos/broken/.git");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("HEAD"), "not a ref\n").unwrap();

    let root = sb.path("repos");
    Demo { sandbox: sb, root }
}

/// A local branch `other` and a `main` commit, both changing `README`.
fn conflicting_branch(r: &std::path::Path) {
    git(r, &["switch", "-q", "-c", "other"]);
    write(r, "README", "theirs\n");
    commit(r, "theirs");
    git(r, &["switch", "-q", "main"]);
    write(r, "README", "ours\n");
    commit(r, "ours");
}

impl Demo {
    /// Set the marks the sample repos are named after (favourite, new,
    /// excluded, snoozed) in `ws.state`. Call after a scan, so repo keys
    /// are known.
    pub fn apply_marks(&self, ws: &mut Workspace) {
        let key = |ws: &Workspace, name: &str| {
            ws.entries
                .iter()
                .find(|e| e.display_name() == format!("demo/{name}"))
                .map(|e| e.key.clone())
        };
        for name in ["favourite", "clean"] {
            if let Some(k) = key(ws, name) {
                ws.state.mark(&k, |m| m.favourite = Some(true));
            }
        }
        if let Some(k) = key(ws, "new") {
            ws.state.new.insert(k);
        }
        if let Some(k) = key(ws, "excluded") {
            ws.state.mark(&k, |m| m.exclude = Some(true));
        }
        if let Some(k) = key(ws, "snoozed") {
            let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24);
            ws.state.snoozed.insert(k, until);
        }
        ws.resolve_flags();
    }
}
