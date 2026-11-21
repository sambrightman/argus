//! Mutating operations, executed with the `git` CLI so that the user's
//! credentials, hooks and configuration (`pull.rebase`, `rebase.autoStash`,
//! `commit.gpgsign`, …) apply exactly as they would by hand.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::status::InProgress;

/// The first word of the stash fix's messages (`argus 2026-11-08 12:00`),
/// which `[tags.ignore.issues] stashes` doesn't cover by default.
pub const STASH_PREFIX: &str = "argus";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    Fetch,
    /// `git pull`: integration strategy comes from the user's config.
    Pull,
    Push,
    /// First push of a branch without upstream.
    PushSetUpstream {
        remote: String,
        branch: String,
    },
    /// Push `branch` to the same-named branch on `remote`, whatever the
    /// branch's upstream and `push.default` say (fast-forward only).
    PushTo {
        remote: String,
        branch: String,
    },
    Switch {
        branch: String,
    },
    /// `git switch --track`: a new local branch for a remote one (`origin/x`
    /// gives `x`), its upstream set, checked out.
    SwitchTrack {
        remote_branch: String,
    },
    /// `git switch -c`: a new branch at HEAD, checked out.
    CreateBranch {
        name: String,
    },
    /// `git remote add`.
    AddRemote {
        name: String,
        url: String,
    },
    /// `git remote set-url`: for a repo renamed or moved on its forge.
    SetRemoteUrl {
        remote: String,
        url: String,
    },
    /// End the operation in progress (`git merge --abort`, `git bisect
    /// reset`…), losing its progress. Permanently destructive: callers
    /// confirm first.
    Abort {
        what: InProgress,
    },
    /// `git branch -d`: delete a branch git agrees is merged.
    /// `git branch -d`, which refuses a branch git doesn't see as merged.
    /// With `force_at`, `-D`, only while the branch is still at that
    /// commit: for a branch whose changes are in the default branch by
    /// content (squashed or rebased), which git can't see, or one chosen
    /// by name (`b k`).
    DeleteBranch {
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force_at: Option<String>,
    },
    /// `git worktree prune`: delete the records of linked worktrees whose
    /// folder is gone (not locked ones).
    PruneWorktrees,
    /// `git worktree remove .`, run in the linked worktree: refuses one
    /// with untracked or changed files, deletes ignored ones.
    RemoveWorktree,
    /// `git worktree remove <path>` for a worktree whose folder is gone,
    /// run in its main repo: deletes just that record.
    PruneWorktree {
        path: PathBuf,
    },
    /// `git stash pop --index`, only when a trial merge says it applies
    /// without conflicts.
    StashPop,
    /// [`Op::StashPop`] of `stash@{index}`, only if it's still the stash
    /// `id`.
    StashPopAt {
        index: usize,
        id: String,
    },
    /// `git stash pop` of `stash@{index}` (if it's still the stash `id`)
    /// without `--index`: what the stash had staged comes back unstaged,
    /// and anything staged and then changed again before stashing isn't
    /// brought back at all. For when git can't restore the stash's index
    /// (it says to try without): callers ask first.
    StashPopPlain {
        index: usize,
        id: String,
    },
    /// `git stash drop`, only if `stash@{index}` is still the stash `id`.
    /// Permanently destructive: callers confirm first.
    StashDrop {
        index: usize,
        id: String,
    },
    /// `git stash push`. `staged` stashes only staged changes (untracked
    /// files never, as git refuses the pair; nor is the index kept, which
    /// would take them out of the work tree only); `paths` limits it to
    /// those.
    Stash {
        message: String,
        untracked: bool,
        keep_index: bool,
        staged: bool,
        paths: Vec<String>,
    },
    /// Stash `lines` added or removed lines of files by themselves, which
    /// `git stash` only does when asked line by line: `stage` is the patch
    /// that stages them, `discard` the one that takes them out of the work
    /// tree (see [`crate::diff::patch_hunks`]). What's staged stays
    /// staged. All or nothing: if the lines can't be taken out, the stash
    /// isn't kept.
    StashLines {
        message: String,
        stage: String,
        discard: String,
        lines: usize,
    },
    /// Move the current branch to `commit`.
    Reset {
        mode: ResetMode,
        commit: String,
    },
    /// Replace the last commit with one that adds what's staged, with the
    /// same message or (`Some`) this one.
    Amend {
        message: Option<String>,
    },
    /// Commit what's staged.
    Commit {
        message: String,
    },
    /// `git apply` a patch (a hunk or lines of one, see [`crate::diff::patch`])
    /// to the index (`index`) or the work tree, backwards with `reverse`.
    /// `what` says what it does, for its label.
    Apply {
        patch: String,
        index: bool,
        reverse: bool,
        what: String,
    },
    /// Spin-off (as magit's): a new branch `name` at the current one, with
    /// the commits its upstream doesn't have; the current branch goes back
    /// to the last commit it shares with its upstream. Checked out, or with
    /// `stay` (spin-out) the current branch stays checked out, unless
    /// uncommitted changes would be lost.
    SpinOff {
        name: String,
        stay: bool,
    },
    /// Commit what's staged as a fixup of `commit` (`fixup! <its subject>`),
    /// or with `squash` a squash commit adding that message (`squash!`),
    /// for `git rebase --autosquash` to fold in. `instant` folds it in at
    /// once (a non-interactive autosquash rebase from `commit`'s parent,
    /// uncommitted changes stashed around it); refused when `commit` is
    /// on a remote, as that rewrites pushed history.
    Fixup {
        commit: String,
        squash: Option<String>,
        instant: bool,
    },
    /// Stage everything (including untracked) and commit.
    CommitAll {
        message: String,
    },
    /// Fast-forward a local branch that is not checked out to its upstream.
    FastForward {
        branch: String,
        upstream: String,
    },
    Clone {
        url: String,
        dest: PathBuf,
    },
    /// `git add`: stage untracked files.
    Add {
        paths: Vec<String>,
    },
    /// `git add -u`: stage tracked files' changes, deletions too. Plain
    /// `add` stages one under a gitignored directory but fails, refusing
    /// the ignored path.
    AddTracked {
        paths: Vec<String>,
    },
    /// `git restore`: from the index, or HEAD with `from_head`.
    Restore {
        staged: bool,
        worktree: bool,
        from_head: bool,
        paths: Vec<String>,
    },
    /// `git rm`: from the index only with `cached`, else from the work tree
    /// too (forced).
    Remove {
        cached: bool,
        paths: Vec<String>,
    },
    /// `git clean -f`: delete untracked files.
    Clean {
        paths: Vec<String>,
    },
    /// `git submodule update`: check submodules out at the commits the
    /// repo records (theirs too, recursively).
    SubmoduleUpdate {
        paths: Vec<String>,
    },
    /// Add ignore rules for untracked `paths` to `file` (see
    /// [`crate::gitignore`]): a file edit, not a git command.
    Ignore {
        file: crate::gitignore::IgnoreFile,
        paths: Vec<String>,
        anchored: bool,
    },
}

/// How `git reset` treats the index and work tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetMode {
    /// Changes since the commit stay staged.
    Soft,
    /// They stay in the work tree, unstaged.
    Mixed,
    /// The work tree matches the commit; uncommitted changes the reset
    /// would touch make it refuse.
    Keep,
    /// The work tree matches the commit; uncommitted changes are lost.
    Hard,
}

impl ResetMode {
    pub fn flag(self) -> &'static str {
        match self {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Keep => "--keep",
            ResetMode::Hard => "--hard",
        }
    }
}

impl Op {
    /// A stash of everything (`staged` off, no paths) with these defaults
    /// and an `argus <date> <time>` message.
    pub fn stash(defaults: &crate::config::StashConfig) -> Op {
        Op::Stash {
            message: format!(
                "{STASH_PREFIX} {}",
                jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M")
            ),
            untracked: defaults.untracked,
            keep_index: defaults.keep_index,
            staged: false,
            paths: vec![],
        }
    }

    /// Argument vector for `git`.
    pub fn args(&self) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            Op::Fetch => v(&["fetch"]),
            // Submodules follow: a pull or clone leaves them checked out at
            // the commits the repo records, whatever `submodule.recurse`
            // says. (So does a switch: see `switch`.)
            Op::Pull => v(&["pull", "--recurse-submodules"]),
            Op::Push => v(&["push"]),
            Op::PushSetUpstream { remote, branch } => {
                v(&["push", "--set-upstream", remote, branch])
            }
            Op::PushTo { remote, branch } => {
                let r = format!("refs/heads/{branch}");
                vec!["push".into(), remote.clone(), format!("{r}:{r}")]
            }
            Op::Switch { branch } => v(&["switch", branch]),
            Op::SwitchTrack { remote_branch } => v(&["switch", "--track", remote_branch]),
            Op::Abort { what } => match what {
                InProgress::Bisect => v(&["bisect", "reset"]),
                w => v(&[w.label(), "--abort"]),
            },
            Op::DeleteBranch { branch, force_at } => {
                let how = if force_at.is_some() { "-D" } else { "-d" };
                v(&["branch", how, "--", branch])
            }
            Op::PruneWorktrees => v(&["worktree", "prune"]),
            Op::RemoveWorktree => v(&["worktree", "remove", "."]),
            Op::PruneWorktree { path } => {
                vec![
                    "worktree".into(),
                    "remove".into(),
                    path.display().to_string(),
                ]
            }
            Op::StashPop => v(&["stash", "pop", "--index"]),
            Op::StashPopAt { index, .. } => {
                v(&["stash", "pop", "--index", &format!("stash@{{{index}}}")])
            }
            Op::StashPopPlain { index, .. } => v(&["stash", "pop", &format!("stash@{{{index}}}")]),
            Op::StashDrop { index, .. } => {
                vec!["stash".into(), "drop".into(), format!("stash@{{{index}}}")]
            }
            Op::CreateBranch { name } => v(&["switch", "-c", name]),
            Op::AddRemote { name, url } => v(&["remote", "add", "--", name, url]),
            Op::SetRemoteUrl { remote, url } => v(&["remote", "set-url", "--", remote, url]),
            Op::Stash {
                message,
                untracked,
                keep_index,
                staged,
                paths,
            } => {
                let mut a = v(&["stash", "push"]);
                if *untracked && !*staged {
                    a.push("--include-untracked".into());
                }
                if *keep_index && !*staged {
                    a.push("--keep-index".into());
                }
                if *staged {
                    a.push("--staged".into());
                }
                a.extend(v(&["--message", message]));
                match paths.is_empty() {
                    true => a,
                    false => with_paths(a, paths),
                }
            }
            Op::Reset { mode, commit } => {
                v(&["reset", "-q", mode.flag(), "--end-of-options", commit])
            }
            Op::Amend { message: None } => v(&["commit", "--amend", "--no-edit"]),
            Op::Amend { message: Some(m) } => v(&["commit", "--amend", "--message", m]),
            // run in steps by stash_lines; this is its last
            Op::StashLines { .. } => v(&["stash", "store"]),
            // the patch goes on stdin
            Op::Apply { index, reverse, .. } => {
                let mut a = v(&["apply", "--recount", "--whitespace=nowarn"]);
                if *index {
                    a.push("--cached".into());
                }
                if *reverse {
                    a.push("--reverse".into());
                }
                a.push("-".into());
                a
            }
            // run in steps by spin_off; this is its first
            Op::SpinOff { name, .. } => v(&["switch", "-c", name]),
            Op::Fixup { commit, squash, .. } => {
                let mut a = vec!["commit".to_string(), "--quiet".to_string()];
                match squash.as_deref().map(str::trim) {
                    None => a.push(format!("--fixup={commit}")),
                    Some(m) => {
                        a.push(format!("--squash={commit}"));
                        // no message: no editor either
                        a.extend(if m.is_empty() {
                            v(&["--no-edit"])
                        } else {
                            v(&["--message", m])
                        });
                    }
                }
                a
            }
            Op::Commit { message } | Op::CommitAll { message } => {
                v(&["commit", "--message", message])
            }
            Op::FastForward { branch, upstream } => {
                // `git fetch . upstream:branch` refuses non-fast-forward updates
                // and never touches the work tree.
                vec!["fetch".into(), ".".into(), format!("{upstream}:{branch}")]
            }
            Op::Clone { url, dest } => {
                vec![
                    "clone".into(),
                    "--recurse-submodules".into(),
                    "--".into(),
                    url.clone(),
                    dest.display().to_string(),
                ]
            }
            Op::Add { paths } => with_paths(v(&["add"]), paths),
            Op::AddTracked { paths } => with_paths(v(&["add", "--update"]), paths),
            Op::Restore {
                staged,
                worktree,
                from_head,
                paths,
            } => {
                let mut a = v(&["restore"]);
                if *from_head {
                    a.push("--source=HEAD".into());
                }
                if *staged {
                    a.push("--staged".into());
                }
                if *worktree {
                    a.push("--worktree".into());
                }
                with_paths(a, paths)
            }
            Op::Remove { cached, paths } => {
                let how = if *cached { "--cached" } else { "-f" };
                with_paths(v(&["rm", "-q", how]), paths)
            }
            Op::Clean { paths } => with_paths(v(&["clean", "-f", "-q"]), paths),
            Op::SubmoduleUpdate { paths } => {
                // (submodule paths, as its listing gives them)
                let mut a = v(&["submodule", "update", "--recursive", "--"]);
                a.extend(paths.iter().cloned());
                a
            }
            // not a git command (see `run`)
            Op::Ignore { .. } => vec![],
        }
    }

    /// Network operations go through the bounded worker pool.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Op::Fetch
                | Op::Pull
                | Op::Push
                | Op::PushSetUpstream { .. }
                | Op::PushTo { .. }
                | Op::Clone { .. }
        )
    }

    pub fn label(&self) -> String {
        match self {
            Op::Fetch => "fetch".into(),
            Op::Pull => "pull".into(),
            Op::Push => "push".into(),
            Op::PushSetUpstream { remote, branch } => format!("push -u {remote} {branch}"),
            Op::PushTo { remote, branch } => format!("push {branch} to {remote}/{branch}"),
            Op::Switch { branch } if branch == "-" => "switch to the previous branch".into(),
            Op::Switch { branch } => format!("switch {branch}"),
            Op::SwitchTrack { remote_branch } => format!("switch to {remote_branch}, tracking it"),
            Op::Abort {
                what: InProgress::Bisect,
            } => "end bisect".into(),
            Op::Abort { what } => format!("abort {}", what.label()),
            Op::DeleteBranch { branch, .. } => format!("delete branch {branch}"),
            Op::PruneWorktrees => "prune stale worktree records".into(),
            Op::RemoveWorktree => "remove this worktree".into(),
            Op::PruneWorktree { path } => format!(
                "prune the record of {}",
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            ),
            Op::StashPop => "pop stash".into(),
            Op::StashPopAt { index, .. } => format!("pop stash@{{{index}}}"),
            Op::StashPopPlain { index, .. } => format!("pop stash@{{{index}}} without its index"),
            Op::StashDrop { index, .. } => format!("drop stash@{{{index}}}"),
            Op::CreateBranch { name } => format!("create branch {name}"),
            Op::AddRemote { name, url } => format!("add remote {name} {url}"),
            Op::SetRemoteUrl { remote, url } => format!("point {remote} at {url}"),
            Op::Stash {
                untracked,
                keep_index,
                staged,
                paths,
                ..
            } => {
                let mut l = "stash".to_string();
                if *staged {
                    l.push_str(" staged");
                }
                if !paths.is_empty() {
                    l.push_str(&format!(" {} path(s)", paths.len()));
                }
                let mut how = vec![];
                if *keep_index && !*staged {
                    how.push("keeping index");
                }
                if !*untracked && !*staged {
                    how.push("no untracked");
                }
                if !how.is_empty() {
                    l.push_str(&format!(" ({})", how.join(", ")));
                }
                l
            }
            Op::Reset { mode, commit } => {
                format!("reset {} {}", mode.flag(), &commit[..commit.len().min(7)])
            }
            Op::Amend { .. } => "amend".into(),
            Op::Commit { .. } => "commit staged".into(),
            Op::Fixup {
                commit,
                squash,
                instant,
            } => format!(
                "{}{} {}",
                if *instant { "instant " } else { "" },
                if squash.is_some() { "squash" } else { "fixup" },
                &commit[..commit.len().min(7)]
            ),
            Op::StashLines { lines, .. } => {
                format!("stash {lines} {}", crate::plural(*lines, "line", "lines"))
            }
            Op::Apply { what, .. } => what.clone(),
            Op::SpinOff { name, stay: false } => format!("spin off {name}"),
            Op::SpinOff { name, stay: true } => format!("spin out {name}"),
            Op::CommitAll { .. } => "commit".into(),
            Op::FastForward { branch, .. } => format!("fast-forward {branch}"),
            Op::Clone { .. } => "clone".into(),
            Op::Add { paths } | Op::AddTracked { paths } => format!("stage {}", files(paths)),
            Op::Restore {
                worktree: false,
                paths,
                ..
            }
            | Op::Remove {
                cached: true,
                paths,
            } => format!("unstage {}", files(paths)),
            Op::Restore { paths, .. } | Op::Remove { paths, .. } => {
                format!("discard {}", files(paths))
            }
            Op::Clean { paths } => format!("delete untracked {}", files(paths)),
            Op::SubmoduleUpdate { paths } => {
                format!("check out {} at its recorded commit", files(paths))
            }
            Op::Ignore { file, paths, .. } => {
                use crate::gitignore::IgnoreFile::*;
                let to = match file {
                    TopLevel => ".gitignore",
                    Directory => "their .gitignore",
                    Private => ".git/info/exclude",
                    Global => "the global excludes",
                };
                format!("ignore {} in {to}", files(paths))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpOutcome {
    pub success: bool,
    pub code: Option<i32>,
    /// Combined stdout+stderr, trimmed.
    pub output: String,
    #[serde(with = "duration_ms")]
    pub duration: Duration,
}

mod duration_ms {
    use serde::Serializer;
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }
}

/// Run `op` in `dir` (for clone: the parent of `dest` is created and used).
/// `args` then `--` and the paths, so none can be taken as an option.
fn with_paths(mut args: Vec<String>, paths: &[String]) -> Vec<String> {
    args.push("--".into());
    // each exactly the path it names: git would otherwise read `*`, `?`
    // and `[…]` in a file's name as a pattern, and act on every match
    args.extend(paths.iter().map(|p| format!(":(literal){p}")));
    args
}

/// `a`, or `3 files`, for labels.
fn files(paths: &[String]) -> String {
    match paths {
        [one] => one.clone(),
        _ => format!("{} files", paths.len()),
    }
}

/// The index code of each of `files` (`?` untracked, `A` added, ` `
/// unstaged only…), skipping files with no changes.
fn codes<'a>(
    s: &'a crate::status::RepoStatus,
    files: &'a [String],
) -> impl Iterator<Item = (&'a String, char)> {
    files
        .iter()
        .filter_map(|f| s.changes.entry(f).map(|c| (f, c.index)))
}

/// Stage `files` (changes, new files and deletions): tracked ones, then
/// untracked ones. Never a nested repo: git would add it as an embedded
/// repo (see [`nested_repos_in`]).
pub fn stage_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (untracked, tracked): (Vec<_>, Vec<_>) = files
        .iter()
        .filter_map(|f| Some((f, s.changes.entry(f)?.untracked())))
        .filter(|(f, _)| nested_repos_in(s, std::slice::from_ref(f)).is_empty())
        .filter(|(f, _)| !is_conflicted(s, f))
        .partition(|(_, untracked)| *untracked);
    let paths = |v: Vec<(&String, bool)>| v.into_iter().map(|(f, _)| f.clone()).collect();
    let mut ops = vec![];
    if !tracked.is_empty() {
        ops.push(Op::AddTracked {
            paths: paths(tracked),
        });
    }
    if !untracked.is_empty() {
        ops.push(Op::Add {
            paths: paths(untracked),
        });
    }
    ops
}

/// Whether `file` is conflicted (status gives every kind of conflict `U`).
fn is_conflicted(s: &crate::status::RepoStatus, file: &str) -> bool {
    s.changes.entry(file).is_some_and(|c| c.index == 'U')
}

/// Which of `files` are conflicted, which staging and unstaging leave
/// out: staging would mark one resolved with its conflict markers in;
/// unstaging would leave the markers in the work tree as an ordinary
/// change.
pub fn conflicted_in<'f>(s: &crate::status::RepoStatus, files: &'f [String]) -> Vec<&'f String> {
    files.iter().filter(|f| is_conflicted(s, f)).collect()
}

/// Which of `files` are repos nested in the work tree (or directories of
/// nothing else), which staging leaves out.
pub fn nested_repos_in<'f>(s: &crate::status::RepoStatus, files: &'f [String]) -> Vec<&'f String> {
    files
        .iter()
        .filter(|f| {
            s.changes
                .entry(f)
                .is_some_and(|c| c.kind == crate::status::PathKind::Repo)
        })
        .collect()
}

/// Unstage `files`, keeping the work tree: `git restore --staged` for files
/// in HEAD, `git rm --cached` for newly added ones (which also works before
/// the first commit). Files with nothing staged are left alone.
pub fn unstage_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (mut restore, mut added) = (vec![], vec![]);
    for (f, index) in codes(s, files) {
        match index {
            // (nor a conflicted one: see `conflicted_in`)
            ' ' | '?' | 'U' => {}
            'A' => added.push(f.clone()),
            _ => restore.push(f.clone()),
        }
    }
    let mut ops = vec![];
    if !restore.is_empty() {
        ops.push(Op::Restore {
            staged: true,
            worktree: false,
            from_head: false,
            paths: restore,
        });
    }
    if !added.is_empty() {
        ops.push(Op::Remove {
            cached: true,
            paths: added,
        });
    }
    ops
}

/// Discard the unstaged changes to `files`: the work tree put back as the
/// index has it, untracked files deleted. What's staged is never touched
/// (it's unstaged first, by its own key), so a file with nothing unstaged
/// gets nothing. A conflicted file goes back to HEAD, index and work tree.
/// Permanently destructive: callers confirm first.
pub fn discard_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (mut restore, mut conflicted, mut untracked) = (vec![], vec![], vec![]);
    for f in files {
        let Some(c) = s.changes.entry(f) else {
            continue;
        };
        match (c.index, c.worktree) {
            _ if c.untracked() => untracked.push(f.clone()),
            ('U', _) => conflicted.push(f.clone()),
            // nothing unstaged: what's staged is never discarded
            (_, ' ') => {}
            _ => restore.push(f.clone()),
        }
    }
    let mut ops = vec![];
    if !restore.is_empty() {
        // the work tree as the index has it: staged changes stay
        ops.push(Op::Restore {
            staged: false,
            worktree: true,
            from_head: false,
            paths: restore,
        });
    }
    if !conflicted.is_empty() {
        ops.push(Op::Restore {
            staged: true,
            worktree: true,
            from_head: true,
            paths: conflicted,
        });
    }
    if !untracked.is_empty() {
        ops.push(Op::Clean { paths: untracked });
    }
    ops
}

/// [`stash_problem`]'s answer for a repo with nothing to stash: a reason to
/// skip it, not an error.
pub const NOTHING_TO_STASH: &str = "nothing to stash";

/// Why a stash with these options can't work in this state, if it can't:
/// no commit yet, nothing to stash, or a staged-only stash (`--staged`)
/// git would save but then fail to take out of the work tree, leaving the
/// changes and a stash. That happens when a file's unstaged changes overlap
/// its staged ones; it's checked exactly, as git does it (see
/// [`staged_removal_fails`]), for files with both. `paths` limits what's
/// considered, and untracked files count only with `untracked`.
pub fn stash_problem(
    repo: &Path,
    s: &crate::status::RepoStatus,
    untracked: bool,
    staged: bool,
    paths: &[String],
) -> Option<String> {
    let files: Vec<&crate::status::FileChange> = s
        .changes
        .files
        .iter()
        .filter(|f| untracked || f.index != '?')
        .filter(|f| {
            paths.is_empty()
                || paths.iter().any(|p| {
                    f.path == *p || f.path.starts_with(&format!("{}/", p.trim_end_matches('/')))
                })
        })
        .collect();
    if matches!(s.head, crate::status::Head::Unborn(_)) {
        return Some("no commit yet to stash against".into());
    }
    if files.is_empty() {
        return Some(NOTHING_TO_STASH.into());
    }
    if !staged {
        return None;
    }
    let is_staged = |f: &&&crate::status::FileChange| f.index != ' ' && f.index != '?';
    if !files.iter().any(|f| is_staged(&f)) {
        return Some("nothing staged to stash".into());
    }
    let failing: Vec<&str> = files
        .iter()
        .filter(|f| is_staged(f) && f.worktree != ' ')
        .map(|f| f.path.as_str())
        .filter(|f| staged_removal_fails(repo, f))
        .collect();
    (!failing.is_empty()).then(|| {
        format!(
            "git can't stash only the staged changes of {}: unstaged changes overlap them \
             (stash everything, or keep the index instead)",
            failing.join(", ")
        )
    })
}

/// Whether `file`'s mode in the work tree differs from the index's (made
/// executable, say): a change no patch of its lines carries.
pub fn mode_changed(repo: &Path, file: &str) -> bool {
    let path = format!(":(literal){file}");
    let raw = git_in(repo, &["diff-files", "--raw", "--", &path]);
    // `:100644 100755 <id> <id> M\t<path>`
    let mut modes = raw.output.trim_start_matches(':').split(' ');
    matches!((modes.next(), modes.next()), (Some(old), Some(new)) if raw.success && old != new)
}

/// Whether git's staged-only stash would fail to take `file`'s staged
/// changes out of the work tree. git reverse-applies the HEAD → index patch
/// (one line of context) to the work tree; this tries that with `--check`,
/// changing nothing. Plumbing (`diff-index`) rather than `git diff`, whose
/// output follows settings such as `diff.noprefix` that `apply` can't read.
/// Any failure to check counts as failing.
pub fn staged_removal_fails(repo: &Path, file: &str) -> bool {
    let cmd = |args: &[&str]| {
        let mut c = Command::new("git");
        c.current_dir(repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .envs(crate::isolation::git_env().iter().map(|(k, v)| (k, v)));
        c
    };
    let ran = Instant::now();
    let diff = [
        "diff-index",
        "--cached",
        "-p",
        "--binary",
        "-U1",
        "HEAD",
        "--",
        file,
    ];
    let patch = cmd(&diff).stdin(Stdio::null()).output();
    traced(
        repo,
        &diff,
        patch.as_ref().is_ok_and(|o| o.status.success()),
        ran,
    );
    let Ok(patch) = patch else {
        return true;
    };
    if !patch.status.success() {
        return true;
    }
    let Ok(mut apply) = cmd(&["apply", "-R", "--check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return true;
    };
    let ran = Instant::now();
    let wrote = apply
        .stdin
        .take()
        .is_some_and(|mut i| std::io::Write::write_all(&mut i, &patch.stdout).is_ok());
    let applies = apply.wait().is_ok_and(|s| s.success());
    traced(repo, &["apply", "-R", "--check"], applies, ran);
    !(applies && wrote)
}

pub fn run(dir: &Path, op: &Op) -> OpOutcome {
    let started = Instant::now();
    let cwd = match op {
        Op::Clone { dest, .. } => {
            let parent = dest.parent().unwrap_or(Path::new("."));
            if let Err(e) = std::fs::create_dir_all(parent) {
                return OpOutcome {
                    success: false,
                    code: None,
                    output: format!("creating {}: {e}", parent.display()),
                    duration: started.elapsed(),
                };
            }
            parent.to_path_buf()
        }
        _ => dir.to_path_buf(),
    };
    match op {
        Op::Pull => return pull(&cwd, started),
        Op::Switch { .. } | Op::SwitchTrack { .. } => return switch(&cwd, op, started),
        Op::SpinOff { name, stay } => return spin_off(&cwd, name, *stay, started),
        Op::Apply { patch, .. } => return git_stdin(&cwd, &op.args(), patch, None, started),
        Op::StashLines {
            message,
            stage,
            discard,
            ..
        } => return stash_lines(&cwd, message, stage, discard, started),
        Op::Fixup {
            commit,
            instant: true,
            ..
        } => return instant_fixup(&cwd, op, commit, started),
        Op::Ignore {
            file,
            paths,
            anchored,
        } => {
            let r = crate::gitignore::add(&cwd, *file, paths, *anchored);
            return OpOutcome {
                success: r.is_ok(),
                code: None,
                output: match r {
                    Ok(added) => added
                        .iter()
                        .map(|(f, rule)| format!("{rule} in {}", f.display()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => e,
                },
                duration: started.elapsed(),
            };
        }
        Op::StashPop => return pop(&cwd, 0, None, true, started),
        Op::StashPopAt { index, id } => return pop(&cwd, *index, Some(id), true, started),
        Op::StashPopPlain { index, id } => return pop(&cwd, *index, Some(id), false, started),
        Op::DeleteBranch {
            branch,
            force_at: Some(seen),
        } => {
            let at = git(
                &cwd,
                &[
                    "rev-parse".into(),
                    "-q".into(),
                    "--verify".into(),
                    format!("refs/heads/{branch}"),
                ],
                started,
            );
            if !at.success || at.output != *seen {
                return refused(
                    format!(
                        "not deleting: {branch} has moved since it was checked (refresh and try again)"
                    ),
                    started,
                );
            }
        }
        Op::StashDrop { index, id } if !is_stash(&cwd, *index, id) => {
            return refused(
                format!(
                    "not dropping: stash@{{{index}}} isn't the stash it was (refresh and try again)"
                ),
                started,
            );
        }
        _ => {}
    }
    if let Op::CommitAll { .. } = op {
        // everything but nested repos, which git would add as embedded ones
        let add = vec!["add".into(), "--all".into(), "--".into(), ".".into()];
        let add = git(&cwd, &without_nested(&cwd, add), started);
        if !add.success {
            return add;
        }
    }
    if let Op::Add { .. } = op {
        // a directory holding a nested repo: its files only
        return git(&cwd, &without_nested(&cwd, op.args()), started);
    }
    git(&cwd, &op.args(), started)
}

/// `args` (ending in pathspecs) leaving out the repos nested in the work
/// tree, which `git add` would add as embedded repos.
fn without_nested(cwd: &Path, mut args: Vec<String>) -> Vec<String> {
    let nested = crate::status::nested_repos(cwd).unwrap_or_default();
    args.extend(nested.iter().map(|p| format!(":(exclude,literal){p}")));
    args
}

fn refused(output: String, started: Instant) -> OpOutcome {
    OpOutcome {
        success: false,
        code: None,
        output,
        duration: started.elapsed(),
    }
}

/// Whether `stash@{index}` is the stash `id`.
fn is_stash(cwd: &Path, index: usize, id: &str) -> bool {
    let stash = format!("stash@{{{index}}}");
    let at = git_in(cwd, &["rev-parse", "-q", "--verify", &stash]);
    at.success && at.output.trim() == id
}

/// How a refused pop's output starts when the files could be restored
/// but not the staging: what the stash had staged and what's staged now
/// change the same lines differently. The files alone can still be
/// popped ([`Op::StashPopPlain`]), which callers ask about.
pub const POP_STAGING_CONFLICTS: &str =
    "not popping: what it had staged conflicts with what's staged now";

/// Pop `stash@{index}` (if it's still the stash `id`, when one is given).
///
/// Not `git stash pop --index`: to restore what the stash had staged,
/// that resets the index to HEAD before merging the files, so with
/// anything staged the merge finds every staged file in its way, fails,
/// and leaves the index reset: what was staged is unstaged, and a
/// version staged and then changed again is gone. The same steps work in
/// another order, which is this:
///
/// 1. A trial merge of the stash's files onto what's staged (what the
///    real one merges onto): a conflict is refused here, nothing touched.
/// 2. The untracked files it holds must have nothing in their way, or
///    git restores the rest and stops at them.
/// 3. With `staging`, the staging to end with: a merge of what the stash
///    had staged with what's staged now. If those conflict there's none
///    to restore both ([`POP_STAGING_CONFLICTS`]).
/// 4. The files, by git's own merge, which leaves the index alone and
///    refuses (nothing changed) if a file it would rewrite has changes
///    of its own.
/// 5. The index set to 3's, and the stash dropped.
fn pop(cwd: &Path, index: usize, id: Option<&str>, staging: bool, started: Instant) -> OpOutcome {
    let stash = format!("stash@{{{index}}}");
    let not = |why: &str| refused(format!("not popping: {why}"), started);
    // (what git prints, or why it failed)
    let run = |args: &[&str]| {
        let out = git_in(cwd, args);
        let said = out.output.trim().to_string();
        if out.success { Ok(said) } else { Err(said) }
    };
    let Ok(is) = run(&["rev-parse", "-q", "--verify", &stash]) else {
        return not(&format!("there's no {stash}"));
    };
    if id.is_some_and(|id| id != is) {
        return not(&format!(
            "{stash} isn't the stash it was (refresh and try again)"
        ));
    }
    // (an index with conflicts in it has no tree, a locked one can't be
    // read for one: git says which)
    let staged = match run(&["write-tree"]) {
        Ok(tree) => tree,
        Err(why) => return not(&why),
    };
    let base = format!("{stash}^1");
    let merged = |theirs: &str| {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "--merge-base",
            &base,
            &staged,
            theirs,
        ];
        let o = git_in(cwd, &args);
        let mut lines = o.output.lines();
        let tree = lines.next().unwrap_or_default().to_string();
        let files: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
        match o.code {
            Some(0) => Ok(tree),
            Some(1) => Err(Some(files.join(", "))),
            _ => Err(None),
        }
    };
    let unchecked = || not("can't check it for conflicts (git 2.40 or later is needed for that)");
    let result = match merged(&stash) {
        Ok(tree) => tree,
        Err(Some(files)) => return not(&format!("it would conflict in {files}")),
        Err(None) => return unchecked(),
    };
    // what git would refuse to write over: the files the pop changes
    // (where the merge differs from what's staged) that have changes of
    // their own, and those it adds that are here untracked
    let join = |files: &[&String]| {
        let files: Vec<&str> = files.iter().map(|f| f.as_str()).collect();
        files.join(", ")
    };
    let names = |args: &[&str]| -> Option<Vec<String>> {
        let o = git_in(cwd, args);
        o.success.then(|| {
            o.output
                .split('\0')
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect()
        })
    };
    let changes = |only: Option<&str>| {
        let only = only.map(|f| format!("--diff-filter={f}"));
        let mut args = vec!["diff-tree", "-r", "--name-only", "-z"];
        args.extend(only.as_deref());
        args.extend([staged.as_str(), result.as_str()]);
        names(&args)
    };
    let own = names(&["diff", "--no-ext-diff", "--no-renames", "--name-only", "-z"]);
    if let (Some(written), Some(own)) = (changes(None), own) {
        let over: Vec<&String> = written.iter().filter(|w| own.contains(w)).collect();
        if !over.is_empty() {
            return not(&match &over[..] {
                [one] => format!(
                    "{one} has changes of its own the stash would overwrite \
                     (commit or stash them first)"
                ),
                many => format!(
                    "{} have changes of their own the stash would overwrite \
                     (commit or stash them first)",
                    join(many)
                ),
            });
        }
    }
    if let Some(added) = changes(Some("A")) {
        let here: Vec<&String> = added
            .iter()
            .filter(|a| cwd.join(a).symlink_metadata().is_ok())
            .collect();
        if !here.is_empty() {
            return not(&match &here[..] {
                [one] => {
                    format!("it adds {one}, which is here already, untracked: move it away first")
                }
                many => format!(
                    "it adds {}, which are here already, untracked: move them away first",
                    join(many)
                ),
            });
        }
    }
    // (its third parent, when it has one, holds what was untracked: each
    // needs its place free, and its folders to be folders)
    let ran = Instant::now();
    let args = ["ls-tree", "-r", "--name-only", "-z", &format!("{stash}^3")];
    let held = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .envs(crate::isolation::git_env().iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    traced(
        cwd,
        &args,
        held.as_ref().is_ok_and(|o| o.status.success()),
        ran,
    );
    let held = held.map(|o| o.stdout).unwrap_or_default();
    let in_the_way = |file: &&str| {
        let path = Path::new(file);
        let taken = cwd.join(path).symlink_metadata().is_ok();
        let folders = path
            .ancestors()
            .skip(1)
            .filter(|a| !a.as_os_str().is_empty());
        let blocked = |a: &Path| {
            let is = cwd.join(a).symlink_metadata();
            is.is_ok_and(|m| !m.is_dir())
        };
        taken || folders.into_iter().any(blocked)
    };
    let held = String::from_utf8_lossy(&held);
    let there: Vec<&str> = held
        .split('\0')
        .filter(|f| !f.is_empty())
        .filter(in_the_way)
        .collect();
    if !there.is_empty() {
        return not(&format!(
            "it holds the untracked {}, which {} in the way: move {} away first",
            there.join(", "),
            if there.len() == 1 { "is" } else { "are" },
            if there.len() == 1 { "it" } else { "them" },
        ));
    }
    // the staging to end with, when the stash has any of its own
    let had_staged = format!("{stash}^2");
    let trees = (
        run(&["rev-parse", &format!("{base}^{{tree}}")]),
        run(&["rev-parse", &format!("{had_staged}^{{tree}}")]),
    );
    let restores = matches!(&trees, (Ok(b), Ok(i)) if staging && i != b && *i != staged);
    let to_stage = match restores.then(|| merged(&had_staged)) {
        None => None,
        Some(Ok(tree)) => Some(tree),
        Some(Err(Some(files))) => {
            return refused(format!("{POP_STAGING_CONFLICTS}: {files}"), started);
        }
        Some(Err(None)) => return unchecked(),
    };
    // (`--no-index` said: the user's `stash.index` mustn't make this the
    // pop that resets)
    let applied = git_in(cwd, &["stash", "apply", "--no-index", "--quiet", &stash]);
    if !applied.success {
        return applied;
    }
    if let Some(tree) = to_stage {
        let set = git_in(cwd, &["read-tree", "--reset", &tree]);
        if !set.success {
            return refused(
                format!(
                    "its files are back, but what it had staged isn't staged again \
                     (the stash is kept):\n{}",
                    set.output
                ),
                started,
            );
        }
    }
    // (still the one that was applied: nothing else ran here meanwhile)
    let dropped = match is_stash(cwd, index, &is) {
        true => git_in(cwd, &["stash", "drop", "--quiet", &stash]),
        false => refused(format!("{stash} moved: it's applied, and kept"), started),
    };
    OpOutcome {
        output: match dropped.success {
            true => format!("popped {stash}"),
            false => dropped.output.clone(),
        },
        ..dropped
    }
}

fn in_progress(dir: &Path) -> Option<InProgress> {
    crate::isolation::open(dir)
        .ok()?
        .state()
        .map(crate::status::map_state)
}

/// `git pull`, but never started during an operation in progress, and never
/// leaving one behind: a pull that stops with conflicts is aborted, which
/// also restores anything `rebase.autoStash`/`merge.autoStash` stashed.
/// A pull git calls a success can still leave conflicts: when re-applying
/// the autostash conflicts, git keeps the changes in a stash (which it only
/// does when it couldn't re-apply them) and leaves the conflicted files.
/// That's undone too, back to the commit and changes from before the pull,
/// and reported as a failure.
fn pull(cwd: &Path, started: Instant) -> OpOutcome {
    if let Some(p) = in_progress(cwd) {
        return OpOutcome {
            success: false,
            code: None,
            output: format!("{} in progress; not pulling", p.label()),
            duration: started.elapsed(),
        };
    }
    let rev = |r: &str| {
        let o = git_in(cwd, &["rev-parse", "-q", "--verify", r]);
        o.success.then_some(o.output)
    };
    let (head_before, stash_before) = (rev("HEAD"), rev("refs/stash"));
    let subs_before = submodule_heads(cwd);
    let mut o = git(cwd, &Op::Pull.args(), started);
    if o.success {
        if let Some(head) = head_before
            && rev("refs/stash") != stash_before
            && git_in(cwd, &["log", "-g", "-1", "--format=%gs", "refs/stash"]).output == "autostash"
        {
            let conflicted = git_in(cwd, &["diff", "--name-only", "--diff-filter=U"]).output;
            o.success = false;
            o.output.push_str(&undo_autostash_conflict(
                cwd,
                &head,
                &subs_before,
                &conflicted,
            ));
        }
        o.duration = started.elapsed();
        return o;
    }
    if let Some(p) = in_progress(cwd) {
        let cmd = match p {
            InProgress::Merge => "merge",
            InProgress::Rebase => "rebase",
            _ => return o,
        };
        let a = git(cwd, &[cmd.into(), "--abort".into()], started);
        o.output.push_str(&if a.success {
            format!("\nargus: pull stopped with conflicts; aborted (git {cmd} --abort), repo left as it was")
        } else {
            format!("\nargus: pull stopped with conflicts and `git {cmd} --abort` failed:\n{}", a.output)
        });
        o.duration = started.elapsed();
    }
    o
}

/// `git switch`, then the submodules checked out at the commits the branch
/// records. Not `switch --recurse-submodules`: that silently overwrites
/// untracked files in a submodule, where `submodule update` (as `pull
/// --recurse-submodules` uses) refuses, leaving the submodule as it was.
/// A submodule put at another commit than was recorded stays there (and
/// is said to have); the rest follow.
fn switch(cwd: &Path, op: &Op, started: Instant) -> OpOutcome {
    let before = submodule_states(cwd);
    let mut o = git(cwd, &op.args(), started);
    if !o.success {
        return o;
    }
    // the branch's submodules now, each following unless it had been
    // put somewhere else before (one the branch left didn't have, too)
    let now = submodule_states(cwd);
    if now.is_empty() {
        return o;
    }
    let was = |path: &String| before.iter().find(|(p, _)| p == path).map(|(_, s)| *s);
    // (one not checked out is passed with the rest: git leaves it so)
    let (stay, follow): (Vec<&String>, Vec<&String>) = now
        .iter()
        .map(|(p, _)| p)
        .partition(|p| was(p) == Some('+'));
    if !follow.is_empty() {
        let mut args = vec!["submodule", "update", "--recursive", "--"];
        args.extend(follow.iter().map(|p| p.as_str()));
        let u = git_in(cwd, &args);
        if !u.success {
            o.success = false;
            o.output.push_str(&format!(
                "\nargus: switched, but checking out the submodules failed:\n{}",
                u.output
            ));
        }
    }
    for path in stay {
        o.output.push_str(&format!(
            "\nargus: {path} left where it was: it wasn't at the commit recorded for it"
        ));
    }
    // one that came with the branch and was never set up isn't checked
    // out by that, nor does git count it a failure: said
    let not_out = |states: &[(String, char)]| -> Vec<String> {
        let out = states.iter().filter(|(_, s)| *s == '-');
        out.map(|(p, _)| p.clone()).collect()
    };
    let came: Vec<String> = not_out(&now)
        .into_iter()
        .filter(|p| was(p).is_none())
        .collect();
    let absent: Vec<String> = match came.is_empty() {
        true => vec![],
        false => not_out(&submodule_states(cwd)),
    }
    .into_iter()
    .filter(|p| came.contains(p))
    .collect();
    for path in absent {
        o.output.push_str(&format!(
            "\nargus: {path} isn't checked out (git submodule update --init -- {path})"
        ));
    }
    o.duration = started.elapsed();
    o
}

/// The repo's own submodules by path, each with its state as `git
/// submodule status` marks it: ` ` at the recorded commit, `+` at
/// another, `-` not checked out, `U` conflicted.
fn submodule_states(cwd: &Path) -> Vec<(String, char)> {
    if !cwd.join(".gitmodules").exists() {
        return vec![];
    }
    let listed = git_in(cwd, &["submodule", "status"]);
    // (not listed, an entry with no mapping say: none to follow)
    if !listed.success {
        return vec![];
    }
    // (the mark, the commit, the path; the first line's blank mark may
    // have been trimmed away with the output's ends)
    let line = |l: &str| {
        let state = l.chars().next().filter(|c| "+-U".contains(*c));
        let state = state.unwrap_or(' ');
        let (_, rest) = l.trim_start_matches(['+', '-', 'U', ' ']).split_once(' ')?;
        // (a checked-out one's commit is described after its path, in
        // brackets; the rest have their path alone, brackets and all)
        let path = match rest.rsplit_once(" (") {
            Some((path, _)) if matches!(state, ' ' | '+') && rest.ends_with(')') => path,
            _ => rest,
        };
        Some((path.to_string(), state))
    };
    listed.output.lines().filter_map(line).collect()
}

/// `git` with `input` on its stdin.
/// A file removed when it's no longer needed, however that comes about.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// [`Op::StashLines`]: the stash is made as `git stash` makes one, a
/// commit of the index with the lines staged into it (a copy: the index
/// itself isn't touched) on one of the index as it is, and stored; then
/// the lines are taken out of the work tree. If that fails the stash is
/// dropped again, so it's all or nothing.
fn stash_lines(
    cwd: &Path,
    message: &str,
    stage: &str,
    discard: &str,
    started: Instant,
) -> OpOutcome {
    let not = |why: &str| refused(format!("not stashed: {why}"), started);
    // (what git prints, or why it failed)
    let run = |args: &[&str]| {
        let out = git_in(cwd, args);
        let said = out.output.trim().to_string();
        if out.success { Ok(said) } else { Err(said) }
    };
    let Ok(head) = run(&["rev-parse", "-q", "--verify", "HEAD^{commit}"]) else {
        return not("no commit yet to stash against");
    };
    let index = match run(&["rev-parse", "--git-path", "index"]) {
        Ok(index) => index,
        Err(why) => return not(&why),
    };
    let index = cwd.join(index);
    let scratch = Scratch(index.with_file_name(format!("argus-index.{}", std::process::id())));
    if let Err(e) = std::fs::copy(&index, &scratch.0) {
        return not(&format!("copying the index: {e}"));
    }
    let apply = |patch: &str, to: Option<&Path>, reverse: bool| {
        let op = Op::Apply {
            patch: String::new(),
            index: to.is_some(),
            reverse,
            what: String::new(),
        };
        git_stdin(cwd, &op.args(), patch, to, started)
    };
    let staged = apply(stage, Some(&scratch.0), false);
    if !staged.success {
        return not(&format!(
            "the lines no longer apply (refresh and mark them again)\n{}",
            staged.output
        ));
    }
    let scratch_index = scratch.0.to_string_lossy().into_owned();
    let ran = Instant::now();
    let with_lines = Command::new("git")
        .current_dir(cwd)
        .arg("write-tree")
        .env("GIT_INDEX_FILE", &scratch_index)
        .envs(crate::isolation::git_env().iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .output();
    let with_lines = match with_lines {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("failed to run git: {e}")),
    };
    let ok = with_lines.is_ok();
    traced(cwd, &["write-tree", "(of a scratch index)"], ok, ran);
    let branch = run(&["symbolic-ref", "--short", "-q", "HEAD"]);
    let branch = branch.unwrap_or_else(|_| "(no branch)".into());
    let title = run(&["log", "-1", "--format=%h %s", "HEAD"]).unwrap_or_default();
    let on_index = format!("index on {branch}: {title}");
    let on_branch = format!("On {branch}: {message}");
    // (an index with conflicts in it has no tree to write, a locked one
    // can't be read for one, no identity makes no commit: git says which)
    let stash = with_lines.and_then(|with_lines| {
        let as_is = run(&["write-tree"])?;
        let base = run(&["commit-tree", &as_is, "-p", &head, "-m", &on_index])?;
        let parents = ["-p", &head, "-p", &base];
        run(&[
            &["commit-tree", &with_lines],
            &parents[..],
            &["-m", &on_branch],
        ]
        .concat())
    });
    let stash = match stash {
        Ok(stash) => stash,
        Err(why) => return not(&format!("git couldn't make the stash's commits\n{why}")),
    };
    let stored = git_in(cwd, &["stash", "store", "-m", &on_branch, &stash]);
    if !stored.success {
        return not(&stored.output);
    }
    let taken = apply(discard, None, true);
    if taken.success {
        return OpOutcome {
            output: on_branch,
            ..taken
        };
    }
    // (still the newest: nothing else ran in this repo meanwhile)
    if is_stash(cwd, 0, &stash) {
        git_in(cwd, &["stash", "drop", "-q", "stash@{0}"]);
    }
    not(&format!(
        "the lines couldn't be taken out of the work tree\n{}",
        taken.output
    ))
}

fn git_stdin(
    cwd: &Path,
    args: &[String],
    input: &str,
    index: Option<&Path>,
    started: Instant,
) -> OpOutcome {
    use std::io::Write;
    let ran = Instant::now();
    let child = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(index.map(|i| ("GIT_INDEX_FILE", i)))
        .envs(crate::isolation::git_env().iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let out = child.and_then(|mut c| {
        c.stdin.take().expect("piped").write_all(input.as_bytes())?;
        c.wait_with_output()
    });
    traced(
        cwd,
        args,
        out.as_ref().is_ok_and(|o| o.status.success()),
        ran,
    );
    match out {
        Ok(o) => OpOutcome {
            success: o.status.success(),
            code: o.status.code(),
            output: format!(
                "{}\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
            .trim()
            .to_string(),
            duration: started.elapsed(),
        },
        Err(e) => refused(format!("failed to run git: {e}"), started),
    }
}

/// An instant [`Op::Fixup`]: refused for a commit on a remote, then the
/// commit, then a non-interactive autosquash rebase folding it in. A
/// conflict leaves the rebase in progress, as git does, for the user (or
/// the abort fix) to finish.
fn instant_fixup(cwd: &Path, op: &Op, commit: &str, started: Instant) -> OpOutcome {
    let on_remote = git_in(cwd, &["branch", "--remotes", "--contains", commit]);
    if !on_remote.success || !on_remote.output.trim().is_empty() {
        return refused(
            format!(
                "{} is on a remote: folding a fixup in would rewrite pushed history \
                 (a plain fixup commit doesn't)",
                &commit[..commit.len().min(7)]
            ),
            started,
        );
    }
    if let Some(merge) = merge_made_by_hand(cwd, commit) {
        return refused(
            format!(
                "a merge since {} has changes of its own, which folding a fixup in                  (a rebase) can't make again: {merge} (a plain fixup commit works)",
                &commit[..commit.len().min(7)]
            ),
            started,
        );
    }
    let c = git(cwd, &op.args(), started);
    if !c.success {
        return c;
    }
    let parent = format!("{commit}^");
    let has_parent = git_in(cwd, &["rev-parse", "--verify", "--quiet", &parent]).success;
    // the todo list and messages taken as they are
    let mut rebase: Vec<String> = [
        "-c",
        "sequence.editor=true",
        "-c",
        "core.editor=true",
        "rebase",
        "--quiet",
        "--interactive",
        "--autosquash",
        "--autostash",
        // (merges made since the commit stay merges)
        "--rebase-merges",
    ]
    .map(String::from)
    .to_vec();
    rebase.push(if has_parent { parent } else { "--root".into() });
    let mut r = git(cwd, &rebase, started);
    r.output = format!("{}\n{}", c.output, r.output).trim().to_string();
    r
}

/// A merge after `commit` that a rebase can't make again as it is, by its
/// summary: one whose tree isn't what merging its two parents gives (a
/// conflict resolved by hand, something added), or of more than two.
fn merge_made_by_hand(cwd: &Path, commit: &str) -> Option<String> {
    let range = format!("{commit}..HEAD");
    let merges = git_in(cwd, &["rev-list", "--merges", "--parents", &range]);
    for line in merges.output.lines() {
        let ids: Vec<&str> = line.split(' ').collect();
        let summary = || {
            let s = git_in(cwd, &["log", "-1", "--format=%h %s", ids[0]]);
            s.output.trim().to_string()
        };
        let [id, first, second] = ids[..] else {
            return Some(summary());
        };
        let tree = git_in(cwd, &["rev-parse", &format!("{id}^{{tree}}")]);
        let again = git_in(cwd, &["merge-tree", "--write-tree", first, second]);
        if !again.success || again.output.trim() != tree.output.trim() {
            return Some(summary());
        }
    }
    None
}

/// [`Op::SpinOff`], in steps, as magit's `magit--branch-spinoff`.
fn spin_off(cwd: &Path, name: &str, stay: bool, started: Instant) -> OpOutcome {
    let out = |args: &[&str]| {
        let o = git_in(cwd, args);
        o.success.then(|| o.output.trim().to_string())
    };
    let Some(current) = out(&["symbolic-ref", "--short", "--quiet", "HEAD"]) else {
        return refused("not on a branch: nothing to spin off".into(), started);
    };
    if out(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{name}"),
    ])
    .is_some()
    {
        return refused(format!("a branch {name} already exists"), started);
    }
    let head = out(&["rev-parse", "HEAD"]).unwrap_or_default();
    // where the branch goes back to: what it shares with its upstream
    let base = out(&[
        "rev-parse",
        "--abbrev-ref",
        &format!("{current}@{{upstream}}"),
    ])
    .and_then(|up| out(&["merge-base", &current, &up]))
    .filter(|b| *b != head);
    let mut notes = vec![];
    let dirty =
        out(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let stay = if stay && dirty && base.is_some() {
        notes.push("uncommitted changes: switched to the new branch to keep them".to_string());
        false
    } else {
        stay
    };
    let step = |args: &[&str]| -> Result<(), OpOutcome> {
        let o = git_in(cwd, args);
        if o.success { Ok(()) } else { Err(o) }
    };
    let run = || -> Result<(), OpOutcome> {
        if stay {
            step(&["branch", "--no-track", name, &current])?;
        } else {
            step(&["switch", "--no-track", "-c", name, &current])?;
        }
        if let Some(base) = &base {
            if stay {
                // (`--keep`, not `--hard`: an untracked file where the
                // older commit has one stops it instead of being lost)
                step(&["reset", "--keep", base]).inspect_err(|_| {
                    git_in(cwd, &["branch", "-D", name]);
                })?;
            } else {
                let refname = format!("refs/heads/{current}");
                let msg = format!("spin off {name}: moving to {base}");
                step(&["update-ref", "-m", &msg, &refname, base])?;
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => {
            notes.push(match &base {
                Some(b) => format!("{current} reset to {}", &b[..b.len().min(7)]),
                None => format!("{current} unchanged: nothing it has that its upstream hasn't"),
            });
            OpOutcome {
                success: true,
                code: Some(0),
                output: notes.join("\n"),
                duration: started.elapsed(),
            }
        }
        Err(mut o) => {
            o.duration = started.elapsed();
            o
        }
    }
}

/// Each checked-out submodule's HEAD, nested ones after their parent.
fn submodule_heads(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut v = vec![];
    let Ok(repo) = crate::isolation::open(dir) else {
        return v;
    };
    let Ok(Some(sms)) = repo.submodules() else {
        return v;
    };
    for sm in sms {
        if !sm.state().is_ok_and(|s| s.worktree_checkout) {
            continue;
        }
        let (Ok(path), Ok(Some(r))) = (sm.work_dir(), sm.open()) else {
            continue;
        };
        if let Ok(id) = r.head_id() {
            v.push((path.clone(), id.to_string()));
            v.extend(submodule_heads(&path));
        }
    }
    v
}

/// Back to `head`, and each submodule to the commit in `subs` (the pull
/// checked them out too), with the autostash (`stash@{0}`) re-applied
/// there, where it was made, so it applies cleanly. Says what happened.
fn undo_autostash_conflict(
    cwd: &Path,
    head: &str,
    subs: &[(PathBuf, String)],
    conflicted: &str,
) -> String {
    let files = conflicted_files(conflicted);
    // each step only once the last succeeded; the stash back last
    let last = std::iter::once_with(|| git_in(cwd, &["reset", "-q", "--hard", head]))
        .chain(
            subs.iter()
                .filter(|(path, id)| git_in(path, &["rev-parse", "HEAD"]).output != *id)
                .map(|(path, id)| git_in(path, &["checkout", "-q", "--detach", id])),
        )
        .find(|o| !o.success)
        .unwrap_or_else(|| git_in(cwd, &["stash", "pop", "-q", "--index"]));
    if last.success {
        format!(
            "\nargus: the pull conflicts with your uncommitted changes in {files}; \
             undone, repo left as it was"
        )
    } else {
        format!(
            "\nargus: the pull conflicts with your uncommitted changes in {files}, and undoing \
             it failed: your changes are in stash@{{0}} (autostash):\n{}",
            last.output
        )
    }
}

/// `git diff --name-only`'s lines as a list, or just "files".
fn conflicted_files(names: &str) -> String {
    match names.lines().collect::<Vec<_>>().join(", ") {
        f if f.is_empty() => "files".to_string(),
        f => f,
    }
}

/// `git <args>` in `cwd`, started at `started`, as a step.
fn traced<S: AsRef<str>>(cwd: &Path, args: &[S], ok: bool, started: Instant) {
    crate::trace::took(
        "git",
        || {
            let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
            let failed = if ok { "" } else { ", failed" };
            format!(
                "git {} in {}{failed}",
                args.join(" "),
                crate::paths::contract_tilde(cwd)
            )
        },
        started.elapsed(),
    );
}

/// Run a read-only git command for status (no network, no prompts).
pub(crate) fn git_in(cwd: &Path, args: &[&str]) -> OpOutcome {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    git(cwd, &args, Instant::now())
}

fn git(cwd: &Path, args: &[String], started: Instant) -> OpOutcome {
    let ran = Instant::now();
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        // Never block on a prompt in a background job; fail fast instead.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("GCM_INTERACTIVE", "never")
        .envs(crate::isolation::git_env().iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .output();
    traced(
        cwd,
        args,
        out.as_ref().is_ok_and(|o| o.status.success()),
        ran,
    );
    match out {
        Ok(o) => {
            // stdout, then stderr
            let (out, err) = (
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
            );
            let output: Vec<&str> = [out.trim(), err.trim()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            OpOutcome {
                success: o.status.success(),
                code: o.status.code(),
                output: output.join("\n"),
                duration: started.elapsed(),
            }
        }
        Err(e) => OpOutcome {
            success: false,
            code: None,
            output: format!("failed to run git: {e}"),
            duration: started.elapsed(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_one_file_and_count_more() {
        let add = |paths: &[&str]| Op::Add {
            paths: paths.iter().map(|p| p.to_string()).collect(),
        };
        assert_eq!(add(&["a"]).label(), "stage a");
        assert_eq!(add(&["a", "b"]).label(), "stage 2 files");
    }

    #[test]
    fn conflicted_files_are_listed_or_just_files() {
        assert_eq!(conflicted_files("a\nb/c\n"), "a, b/c");
        assert_eq!(conflicted_files(""), "files");
    }

    #[test]
    fn args_never_put_user_text_where_it_could_be_an_option() {
        let o = Op::Clone {
            url: "--upload-pack=evil".into(),
            dest: "/x".into(),
        };
        let a = o.args();
        let dd = a.iter().position(|s| s == "--").unwrap();
        assert!(a.iter().position(|s| s == "--upload-pack=evil").unwrap() > dd);
        let a = Op::Reset {
            mode: ResetMode::Hard,
            commit: "--evil".into(),
        }
        .args();
        let end = a.iter().position(|s| s == "--end-of-options").unwrap();
        assert!(a.iter().position(|s| s == "--evil").unwrap() > end);
        // stash/commit messages are passed as option *values*
        let a = Op::Stash {
            message: "-rf".into(),
            untracked: true,
            keep_index: false,
            staged: false,
            paths: vec![],
        }
        .args();
        assert_eq!(
            &a[a.len() - 2..],
            &["--message".to_string(), "-rf".to_string()]
        );
    }

    #[test]
    fn network_classification() {
        assert!(Op::Pull.is_network());
        assert!(
            Op::Clone {
                url: String::new(),
                dest: PathBuf::new()
            }
            .is_network()
        );
        assert!(
            !Op::Switch {
                branch: "main".into()
            }
            .is_network()
        );
        assert!(
            !Op::FastForward {
                branch: "a".into(),
                upstream: "b".into()
            }
            .is_network()
        );
    }

    #[test]
    fn failure_is_reported_not_panicked() {
        let d = tempfile::tempdir().unwrap();
        let o = run(d.path(), &Op::Pull); // not a repo
        assert!(!o.success);
        assert!(o.output.contains("not a git repository"), "{}", o.output);
    }
}
