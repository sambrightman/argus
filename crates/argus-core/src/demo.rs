//! Sample repositories in every state argus shows, for `argus demo`: a way
//! to see the whole UI, and to try themes and icon sets, without touching
//! real repos. Built with the test fixtures, so git runs with a private
//! config (no signing, hooks or user settings).

use std::path::PathBuf;

use crate::rules::Mark;
use crate::testkit::{Sandbox, commit, git, git_may_fail, write};
use crate::workspace::Workspace;

pub struct Demo {
    /// Owns the temporary directory; dropping it deletes everything.
    pub sandbox: Sandbox,
    /// The directory to scan.
    pub root: PathBuf,
}

/// Build the sample repos: many git commands, so the repos (independent of
/// each other) are built in parallel.
pub fn build() -> Demo {
    let sb = Sandbox::new();
    let clone = |name: &str| {
        sb.remote("demo", name);
        sb.clone_as_github("demo", name, &format!("repos/{name}"))
    };
    type Step<'a> = Box<dyn Fn() + Send + Sync + 'a>;
    let steps: Vec<Step> = vec![
        // and a bare mirror of it
        Box::new(|| {
            clone("clean");
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
        }),
        Box::new(|| {
            let r = clone("changes");
            write(&r, "new.txt", "staged\n");
            git(&r, &["add", "new.txt"]);
            write(&r, "README", "edited\n");
            write(&r, "scratch-1.txt", "untracked\n");
            write(&r, "scratch-2.txt", "untracked\n");
        }),
        Box::new(|| {
            let r = clone("ahead");
            commit(&r, "local work");
            commit(&r, "more local work");
        }),
        Box::new(|| {
            let r = clone("behind");
            sb.advance_remote("demo", "behind", "news.txt");
            sb.advance_remote("demo", "behind", "more-news.txt");
            git(&r, &["fetch", "-q"]);
        }),
        // diverged, and a trial merge predicts a conflict: pull isn't automatic
        Box::new(|| {
            let r = sb.diverged("demo", "diverged", "repos/diverged", true);
            git(&r, &["config", "pull.rebase", "false"]);
        }),
        Box::new(|| {
            let r = clone("feature");
            git(&r, &["switch", "-q", "-c", "feat"]);
            commit(&r, "feature work");
            git(&r, &["push", "-q", "-u", "origin", "feat"]);
        }),
        Box::new(|| {
            let r = clone("unpublished");
            git(&r, &["switch", "-q", "-c", "wip"]);
            commit(&r, "not pushed anywhere");
        }),
        // upstream deleted on the remote, with unmerged commits locally
        Box::new(|| {
            let r = clone("gone");
            git(&r, &["switch", "-q", "-c", "old-feature"]);
            commit(&r, "unmerged");
            git(&r, &["push", "-q", "-u", "origin", "old-feature"]);
            git(&r, &["push", "-q", "origin", "--delete", "old-feature"]);
            git(&r, &["fetch", "-q", "--prune"]);
        }),
        Box::new(|| {
            let r = clone("merging");
            conflicting_branch(&r);
            git_may_fail(&r, &["merge", "--no-edit", "other"]);
        }),
        Box::new(|| {
            let r = clone("rebasing");
            conflicting_branch(&r);
            git_may_fail(&r, &["rebase", "other"]);
        }),
        Box::new(|| {
            let r = clone("tagged");
            git(&r, &["tag", "v1.0"]);
            git(&r, &["tag", "-a", "-m", "release", "release-1.0"]);
            git(&r, &["switch", "-q", "--detach", "v1.0"]);
        }),
        // detached, with a commit no branch or tag holds
        Box::new(|| {
            let r = clone("detached");
            git(&r, &["switch", "-q", "--detach"]);
            commit(&r, "experiment");
        }),
        Box::new(|| {
            // two of your own (ignored by default) and one the stash fix made
            let r = clone("stashed");
            for i in 1..=2 {
                write(&r, "README", &format!("edit {i}\n"));
                git(&r, &["stash", "-q"]);
            }
            write(&r, "README", "edit 3\n");
            let message = format!("{} 2026-01-02 09:30", crate::ops::STASH_PREFIX);
            git(&r, &["stash", "push", "-q", "-m", &message]);
        }),
        Box::new(|| {
            let r = clone("branches");
            git(&r, &["switch", "-q", "-c", "topic"]);
            commit(&r, "unpushed topic work");
            git(&r, &["switch", "-q", "main"]);
        }),
        Box::new(|| {
            sb.init("repos/local-only");
        }),
        // a repo nested in another's work tree, untracked, with a change of
        // its own: it unfolds there into its own details
        Box::new(|| {
            clone("with-nested");
            let n = sb.init("repos/with-nested/tools/vendored");
            write(&n, "README", "patched\n");
        }),
        Box::new(|| {
            let r = clone("worktrees");
            let wt = sb.path("repos/worktrees-feat");
            let wt = wt.to_str().unwrap();
            git(&r, &["worktree", "add", "-q", "-b", "wt-feat", wt]);
            // one whose folder was deleted by hand: its record stays
            let gone = sb.path("repos/worktrees-gone");
            git(
                &r,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "wt-gone",
                    gone.to_str().unwrap(),
                ],
            );
            std::fs::remove_dir_all(&gone).unwrap();
            // one whose branch was pushed, merged and deleted on the remote
            let done = sb.path("repos/worktrees-done");
            git(
                &r,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "wt-done",
                    done.to_str().unwrap(),
                ],
            );
            commit(&done, "done");
            git(&done, &["push", "-q", "-u", "origin", "wt-done"]);
            git(&r, &["merge", "-q", "--no-edit", "--no-ff", "wt-done"]);
            git(&r, &["push", "-q"]);
            git(&r, &["push", "-q", "origin", "--delete", "wt-done"]);
            git(&r, &["fetch", "-q", "--prune"]);
        }),
        Box::new(|| {
            let r = clone("with-submodule");
            let lib = sb.remote("demo", "lib");
            git(
                &r,
                &["submodule", "add", "-q", lib.to_str().unwrap(), "lib"],
            );
            commit(&r, "add submodule");
            git(&r, &["push", "-q"]);
        }),
        Box::new(|| {
            for name in ["favourite", "new", "ignored", "snoozed"] {
                clone(name);
            }
        }),
        // unreadable: status reports an error
        Box::new(|| {
            let broken = sb.path("repos/broken/.git");
            std::fs::create_dir_all(&broken).unwrap();
            std::fs::write(broken.join("HEAD"), "not a ref\n").unwrap();
        }),
    ];
    rayon::scope(|s| {
        for step in &steps {
            s.spawn(move |_| step());
        }
    });
    drop(steps);
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
    /// Set the marks the sample repos are named after: favourite and
    /// ignored in `ws.config`, new and snoozed in `ws.state`; and a label of
    /// its own, `work`. Call after a scan, so repo keys are known.
    pub fn apply_marks(&self, ws: &mut Workspace) {
        let index = |ws: &Workspace, name: &str| {
            ws.entries
                .iter()
                .position(|e| e.display_name() == format!("demo/{name}"))
        };
        let key = |ws: &Workspace, name: &str| index(ws, name).map(|i| ws.entries[i].key.clone());
        let favourites: Vec<usize> = ["favourite", "clean"]
            .iter()
            .filter_map(|n| index(ws, n))
            .collect();
        let fav = Mark::label(&ws.config, "favourite").expect("built in");
        ws.set_mark(&favourites, fav, true).unwrap();
        ws.config
            .marks
            .entry("work".into())
            .or_insert(crate::config::MarkDef {
                key: Some('w'),
                icon: Some(crate::config::MarkIcon::One("W".into())),
                color: Some("cyan".into()),
                ..Default::default()
            });
        let work: Vec<usize> = ["clean", "worktrees"]
            .iter()
            .filter_map(|n| index(ws, n))
            .collect();
        let label = Mark::label(&ws.config, "work").expect("just added");
        ws.set_mark(&work, label, true).unwrap();
        if let Some(i) = index(ws, "ignored") {
            ws.set_mark(&[i], Mark::Ignore, true).unwrap();
        }
        if let Some(k) = key(ws, "new") {
            ws.state.new.insert(k);
        }
        if let Some(k) = key(ws, "snoozed") {
            let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24);
            ws.state.snoozed.insert(k, until);
        }
        ws.resolve_flags();
    }
}
