//! Mutating operations, executed with the `git` CLI so that the user's
//! credentials, hooks and configuration (`pull.rebase`, `rebase.autoStash`,
//! `commit.gpgsign`, …) apply exactly as they would by hand.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

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
    Stash {
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
}

impl Op {
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
            Op::Stash { message } => {
                v(&["stash", "push", "--include-untracked", "--message", message])
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
            Op::Stash { .. } => "stash".into(),
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
    if let Op::CommitAll { .. } = op {
        let add = git(&cwd, &["add".into(), "--all".into()], started);
        if !add.success {
            return add;
        }
    }
    git(&cwd, &op.args(), started)
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
