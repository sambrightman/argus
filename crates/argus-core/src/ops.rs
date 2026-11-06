//! Mutating operations, executed with the `git` CLI so that the user's
//! credentials, hooks and configuration (`pull.rebase`, `rebase.autoStash`,
//! `commit.gpgsign`, …) apply exactly as they would by hand.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::status::InProgress;

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
    Switch {
        branch: String,
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
}

impl Op {
    /// A stash of everything (`staged` off, no paths) with these defaults
    /// and an `argus <date> <time>` message.
    pub fn stash(defaults: &crate::config::StashConfig) -> Op {
        Op::Stash {
            message: format!(
                "argus {}",
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
            Op::Pull => v(&["pull"]),
            Op::Push => v(&["push"]),
            Op::PushSetUpstream { remote, branch } => {
                v(&["push", "--set-upstream", remote, branch])
            }
            Op::Switch { branch } => v(&["switch", branch]),
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
            Op::CommitAll { message } => v(&["commit", "--message", message]),
            Op::FastForward { branch, upstream } => {
                // `git fetch . upstream:branch` refuses non-fast-forward updates
                // and never touches the work tree.
                vec!["fetch".into(), ".".into(), format!("{upstream}:{branch}")]
            }
            Op::Clone { url, dest } => {
                vec![
                    "clone".into(),
                    "--".into(),
                    url.clone(),
                    dest.display().to_string(),
                ]
            }
        }
    }

    /// Network operations go through the bounded worker pool.
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Op::Fetch | Op::Pull | Op::Push | Op::PushSetUpstream { .. } | Op::Clone { .. }
        )
    }

    pub fn label(&self) -> String {
        match self {
            Op::Fetch => "fetch".into(),
            Op::Pull => "pull".into(),
            Op::Push => "push".into(),
            Op::PushSetUpstream { remote, branch } => format!("push -u {remote} {branch}"),
            Op::Switch { branch } => format!("switch {branch}"),
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
            Op::CommitAll { .. } => "commit".into(),
            Op::FastForward { branch, .. } => format!("fast-forward {branch}"),
            Op::Clone { .. } => "clone".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpOutcome {
    pub success: bool,
    pub code: Option<i32>,
    /// Combined stdout+stderr, trimmed.
    pub output: String,
    #[serde(with = "duration_ms")]
    pub duration: Duration,
}

mod duration_ms {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_millis() as u64)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(d)?))
    }
}

/// Run `op` in `dir` (for clone: the parent of `dest` is created and used).
/// [`stash_problem`]'s answer for a repo with nothing to stash: a reason to
/// skip it, not an error.
pub const NOTHING_TO_STASH: &str = "nothing to stash";

/// Why a stash with these options can't work in this state, if it can't:
/// nothing to stash, or a staged-only stash (`--staged`) of a file that
/// also has unstaged changes, which git saves but then fails to remove,
/// leaving the changes and a stash. `paths` limits what's considered, and
/// untracked files count only with `untracked`.
pub fn stash_problem(
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
    let mixed: Vec<&str> = files
        .iter()
        .filter(|f| is_staged(f) && f.worktree != ' ')
        .map(|f| f.path.as_str())
        .collect();
    (!mixed.is_empty()).then(|| {
        format!(
            "git can't stash only staged changes of files that also have unstaged ones ({}): \
             stash everything, or keep the index instead",
            mixed.join(", ")
        )
    })
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
    if let Op::Pull = op {
        return pull(&cwd, started);
    }
    if let Op::CommitAll { .. } = op {
        let add = git(&cwd, &["add".into(), "--all".into()], started);
        if !add.success {
            return add;
        }
    }
    git(&cwd, &op.args(), started)
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
fn pull(cwd: &Path, started: Instant) -> OpOutcome {
    if let Some(p) = in_progress(cwd) {
        return OpOutcome {
            success: false,
            code: None,
            output: format!("{} in progress; not pulling", p.label()),
            duration: started.elapsed(),
        };
    }
    let mut o = git(cwd, &Op::Pull.args(), started);
    if !o.success
        && let Some(p) = in_progress(cwd)
    {
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
    fn args_never_put_user_text_where_it_could_be_an_option() {
        let o = Op::Clone {
            url: "--upload-pack=evil".into(),
            dest: "/x".into(),
        };
        let a = o.args();
        let dd = a.iter().position(|s| s == "--").unwrap();
        assert!(a.iter().position(|s| s == "--upload-pack=evil").unwrap() > dd);
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
