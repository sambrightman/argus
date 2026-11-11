//! Mutating operations, executed with the `git` CLI so that the user's
//! credentials, hooks and configuration (`pull.rebase`, `rebase.autoStash`,
//! `commit.gpgsign`, …) apply exactly as they would by hand.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::status::InProgress;

/// The first word of the stash fix's messages (`argus 2026-11-08 12:00`),
/// which `[ignore] stashes` doesn't cover by default.
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
    /// `git switch -c`: a new branch at HEAD, checked out.
    CreateBranch {
        name: String,
    },
    /// `git remote add`.
    AddRemote {
        name: String,
        url: String,
    },
    /// End the operation in progress (`git merge --abort`, `git bisect
    /// reset`…), losing its progress. Permanently destructive: callers
    /// confirm first.
    Abort {
        what: InProgress,
    },
    /// `git branch -d`: delete a branch git agrees is merged.
    DeleteBranch {
        branch: String,
    },
    /// `git stash pop --index`, only when a trial merge says it applies
    /// without conflicts.
    StashPop,
    /// `git stash drop`, only if `stash@{index}` is still the stash `id`.
    /// Permanently destructive: callers confirm first.
    StashDrop {
        index: usize,
        id: String,
    },
    /// `git stash push`. `staged` stashes only staged changes (untracked
    /// files never, as git refuses the pair); `paths` limits it to those.
    Stash {
        message: String,
        untracked: bool,
        keep_index: bool,
        staged: bool,
        paths: Vec<String>,
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
            Op::Abort { what } => match what {
                InProgress::Bisect => v(&["bisect", "reset"]),
                w => v(&[w.label(), "--abort"]),
            },
            Op::DeleteBranch { branch } => v(&["branch", "-d", "--", branch]),
            Op::StashPop => v(&["stash", "pop", "--index"]),
            Op::StashDrop { index, .. } => {
                vec!["stash".into(), "drop".into(), format!("stash@{{{index}}}")]
            }
            Op::CreateBranch { name } => v(&["switch", "-c", name]),
            Op::AddRemote { name, url } => v(&["remote", "add", "--", name, url]),
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
                if *keep_index {
                    a.push("--keep-index".into());
                }
                if *staged {
                    a.push("--staged".into());
                }
                a.extend(v(&["--message", message]));
                if !paths.is_empty() {
                    a.push("--".into());
                    a.extend(paths.iter().cloned());
                }
                a
            }
            Op::Reset { mode, commit } => {
                v(&["reset", "-q", mode.flag(), "--end-of-options", commit])
            }
            Op::Amend { message: None } => v(&["commit", "--amend", "--no-edit"]),
            Op::Amend { message: Some(m) } => v(&["commit", "--amend", "--message", m]),
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
            Op::Switch { branch } => format!("switch {branch}"),
            Op::Abort {
                what: InProgress::Bisect,
            } => "end bisect".into(),
            Op::Abort { what } => format!("abort {}", what.label()),
            Op::DeleteBranch { branch } => format!("delete branch {branch}"),
            Op::StashPop => "pop stash".into(),
            Op::StashDrop { index, .. } => format!("drop stash@{{{index}}}"),
            Op::CreateBranch { name } => format!("create branch {name}"),
            Op::AddRemote { name, url } => format!("add remote {name} {url}"),
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
                if *keep_index {
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
    args.extend(paths.iter().cloned());
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
/// untracked ones.
pub fn stage_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (untracked, tracked): (Vec<_>, Vec<_>) = codes(s, files).partition(|(_, c)| *c == '?');
    let paths = |v: Vec<(&String, char)>| v.into_iter().map(|(f, _)| f.clone()).collect();
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

/// Unstage `files`, keeping the work tree: `git restore --staged` for files
/// in HEAD, `git rm --cached` for newly added ones (which also works before
/// the first commit). Files with nothing staged are left alone.
pub fn unstage_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (mut restore, mut added) = (vec![], vec![]);
    for (f, index) in codes(s, files) {
        match index {
            ' ' | '?' => {}
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

/// Discard every change to `files`, back to HEAD: restore files in HEAD
/// (index and work tree), remove newly added ones, delete untracked ones.
/// Permanently destructive: callers confirm first.
pub fn discard_ops(s: &crate::status::RepoStatus, files: &[String]) -> Vec<Op> {
    let (mut restore, mut added, mut untracked) = (vec![], vec![], vec![]);
    for (f, index) in codes(s, files) {
        match index {
            '?' => untracked.push(f.clone()),
            'A' => added.push(f.clone()),
            _ => restore.push(f.clone()),
        }
    }
    let mut ops = vec![];
    if !restore.is_empty() {
        ops.push(Op::Restore {
            staged: true,
            worktree: true,
            from_head: true,
            paths: restore,
        });
    }
    if !added.is_empty() {
        ops.push(Op::Remove {
            cached: false,
            paths: added,
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
    let Ok(patch) = cmd(&[
        "diff-index",
        "--cached",
        "-p",
        "--binary",
        "-U1",
        "HEAD",
        "--",
        file,
    ])
    .stdin(Stdio::null())
    .output() else {
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
    let wrote = apply
        .stdin
        .take()
        .is_some_and(|mut i| std::io::Write::write_all(&mut i, &patch.stdout).is_ok());
    !(apply.wait().is_ok_and(|s| s.success()) && wrote)
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
        Op::Switch { .. } => return switch(&cwd, op, started),
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
        Op::StashPop => {
            if let Some(why) = pop_problem(&cwd) {
                return refused(format!("not popping: {why}"), started);
            }
        }
        Op::StashDrop { index, id } => {
            let at = git(
                &cwd,
                &[
                    "rev-parse".into(),
                    "-q".into(),
                    "--verify".into(),
                    format!("stash@{{{index}}}"),
                ],
                started,
            );
            if !at.success || at.output != *id {
                return refused(
                    format!(
                        "not dropping: stash@{{{index}}} isn't the stash it was (refresh and try again)"
                    ),
                    started,
                );
            }
        }
        _ => {}
    }
    if let Op::CommitAll { .. } = op {
        // everything but nested repos, which git would add as embedded ones
        let mut args: Vec<String> = vec!["add".into(), "--all".into()];
        let nested = crate::status::nested_repos(&cwd).unwrap_or_default();
        if !nested.is_empty() {
            args.extend(["--".into(), ".".into()]);
            args.extend(nested.iter().map(|p| format!(":(exclude,literal){p}")));
        }
        let add = git(&cwd, &args, started);
        if !add.success {
            return add;
        }
    }
    git(&cwd, &op.args(), started)
}

fn refused(output: String, started: Instant) -> OpOutcome {
    OpOutcome {
        success: false,
        code: None,
        output,
        duration: started.elapsed(),
    }
}

/// Why popping `stash@{0}` would stop with conflicts, if it would: a trial
/// merge (`git merge-tree`) of the stash into HEAD, changing nothing. Their
/// merge base is the commit the stash was made on, as for `git stash pop`.
fn pop_problem(cwd: &Path) -> Option<String> {
    let o = git_in(
        cwd,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "HEAD",
            "stash@{0}",
        ],
    );
    match o.code {
        Some(0) => None,
        Some(1) => {
            let files: Vec<&str> = o.output.lines().skip(1).filter(|l| !l.is_empty()).collect();
            Some(format!("it would conflict in {}", files.join(", ")))
        }
        _ => Some(format!(
            "can't check it for conflicts: {}",
            o.output.lines().next().unwrap_or("git failed")
        )),
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
fn switch(cwd: &Path, op: &Op, started: Instant) -> OpOutcome {
    let mut o = git(cwd, &op.args(), started);
    if o.success && cwd.join(".gitmodules").exists() {
        let u = git_in(cwd, &["submodule", "update", "--recursive"]);
        if !u.success {
            o.success = false;
            o.output.push_str(&format!(
                "\nargus: switched, but checking out the submodules failed:\n{}",
                u.output
            ));
        }
        o.duration = started.elapsed();
    }
    o
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

/// Run a read-only git command for status (no network, no prompts).
pub(crate) fn git_in(cwd: &Path, args: &[&str]) -> OpOutcome {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    git(cwd, &args, Instant::now())
}

fn git(cwd: &Path, args: &[String], started: Instant) -> OpOutcome {
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
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                if !text.trim().is_empty() {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            OpOutcome {
                success: o.status.success(),
                code: o.status.code(),
                output: text.trim().to_string(),
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
