//! Git's installation config, as argus opens repos outside tests: so no
//! testkit `Sandbox`, which isolates git for the whole process. Each scan
//! is one run of argus: this test binary run again as a child process,
//! with the environment set for it, as git is asked once per process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use argus_core::{Config, State, Workspace, trace};

/// Set in the child process: the roots to scan.
const CHILD: &str = "ARGUS_TEST_SCAN_ROOT";

/// In a child process: a scan of the root (with repos kept open, as the
/// TUI), printing the repo's untracked count and the steps said. Run by
/// itself, nothing.
#[test]
fn child_scan() {
    let Some(root) = std::env::var_os(CHILD) else {
        return;
    };
    let said: Arc<Mutex<Vec<String>>> = Arc::default();
    let into = said.clone();
    trace::set_sink(Some(Arc::new(move |s: trace::Step| {
        into.lock().unwrap().push(s.what)
    })));
    let config = Config {
        roots: vec![PathBuf::from(&root)],
        ..Config::default()
    };
    let mut ws = Workspace::new(config, State::default()).unwrap();
    ws.keep_repos_open();
    ws.scan();
    trace::set_sink(None);
    let e = ws.get(&PathBuf::from(&root).join("r")).unwrap();
    println!("untracked {}", e.status().unwrap().changes.untracked);
    for s in said.lock().unwrap().iter() {
        println!("said {s}");
    }
}

struct Run {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ignoring: PathBuf,
    plain: PathBuf,
    env: Vec<(&'static str, OsString)>,
}

impl Run {
    fn kept(&self) -> PathBuf {
        self.root.join("state/git-installation")
    }

    /// git's answer: the installation config file, if any, in `scope`.
    fn answer(&self, file: Option<&Path>, scope: &str) {
        let answer = file.map_or(String::new(), |f| f.display().to_string());
        std::fs::write(self.root.join("answer"), answer).unwrap();
        std::fs::write(self.root.join("scope"), scope).unwrap();
    }

    /// git takes this long to answer.
    fn slow(&self, seconds: &str) {
        std::fs::write(self.root.join("delay"), seconds).unwrap();
    }

    fn keep(&self, line: &str) {
        std::fs::create_dir_all(self.kept().parent().unwrap()).unwrap();
        std::fs::write(self.kept(), line).unwrap();
    }

    /// One run of argus scanning: the repo's untracked count, and whether
    /// it said it read the statuses again.
    fn scan(&self, extra: &[(&str, &Path)]) -> (usize, bool, String) {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "child_scan",
                "--exact",
                "--nocapture",
                "--quiet",
                "--test-threads=1",
            ])
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .envs(extra.iter().map(|(k, v)| (k, v)))
            .env(CHILD, self.root.join("dev"))
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            out.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let n = text
            .lines()
            .find_map(|l| l.strip_prefix("untracked "))
            .unwrap_or_else(|| panic!("{text}"))
            .parse()
            .unwrap();
        let again = text.lines().any(|l| l == format!("said {AGAIN}"));
        (n, again, text)
    }
}

fn git(dir: &Path, args: &[&str], env: &[(&'static str, OsString)]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A repo with an untracked `junk`; an installation config ignoring it
/// and one that doesn't; and, first on `PATH`, a fake `git` answering
/// gix's question with the file and scope the run says (all else goes to
/// git).
fn run() -> Run {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let empty = root.join("empty.gitconfig");
    std::fs::write(&empty, "").unwrap();
    let bin = root.join("bin");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let env: Vec<(&'static str, OsString)> = vec![
        ("PATH", path.into()),
        ("HOME", root.clone().into()),
        ("GIT_CONFIG_GLOBAL", empty.into()),
        ("ARGUS_STATE", root.join("state/state.toml").into()),
        ("GIT_AUTHOR_NAME", "t".into()),
        ("GIT_AUTHOR_EMAIL", "t@t".into()),
        ("GIT_COMMITTER_NAME", "t".into()),
        ("GIT_COMMITTER_EMAIL", "t@t".into()),
    ];
    let repo = root.join("dev/r");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"], &env);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "first"],
        &env,
    );
    std::fs::write(repo.join("junk"), "x\n").unwrap();
    std::fs::write(root.join("ignores"), "junk\n").unwrap();
    let ignoring = root.join("ignoring.gitconfig");
    let excludes = format!(
        "[core]\n\texcludesFile = {}\n",
        root.join("ignores").display()
    );
    std::fs::write(&ignoring, excludes).unwrap();
    let plain = root.join("plain.gitconfig");
    std::fs::write(&plain, "").unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let a = root.join("answer");
    let s = root.join("scope");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = config ] && [ \"$2\" = -lz ]; then\n\
         \t[ -f {d} ] && sleep \"$(cat {d})\"\n\
         \tanswer=$(cat {a})\n\
         \t[ -n \"$answer\" ] && printf '%s\\0file:%s\\0core.excludesfile\\0' \"$(cat {s})\" \"$answer\"\n\
         \texit 0\n\
         fi\n\
         exec {real} \"$@\"\n",
        a = a.display(),
        s = s.display(),
        d = root.join("delay").display(),
    );
    let fake = bin.join("git");
    std::fs::write(&fake, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    Run {
        _dir: dir,
        root,
        ignoring,
        plain,
        env,
    }
}

const AGAIN: &str = "git's installation config moved: statuses read again";

#[test]
fn with_nothing_kept_git_is_asked_and_its_answer_kept() {
    let r = run();
    r.answer(Some(&r.ignoring), "system");
    let (n, again, text) = r.scan(&[]);
    assert_eq!(n, 0, "junk ignored by the installation config\n{text}");
    assert!(!again, "{text}");
    let line = format!("system\t{}\n", r.ignoring.display());
    assert_eq!(std::fs::read_to_string(r.kept()).unwrap(), line);
}

#[test]
fn a_kept_answer_git_still_gives_is_used() {
    let r = run();
    r.answer(Some(&r.ignoring), "system");
    r.keep(&format!("system\t{}\n", r.ignoring.display()));
    let (n, again, text) = r.scan(&[]);
    assert_eq!(n, 0, "{text}");
    assert!(!again, "{text}");
}

/// Opened with the kept answer, then read again with git's (kept repos
/// opened afresh, not reused with the old file).
#[test]
fn a_kept_answer_git_no_longer_gives_is_replaced_and_read_again() {
    let r = run();
    r.answer(Some(&r.plain), "system");
    r.keep(&format!("system\t{}\n", r.ignoring.display()));
    let (n, again, text) = r.scan(&[]);
    assert_eq!(n, 1, "{text}");
    assert!(again, "{text}");
    let line = format!("system\t{}\n", r.plain.display());
    assert_eq!(std::fs::read_to_string(r.kept()).unwrap(), line);
}

/// No installation config is an answer too: repos are opened without
/// asking git for one.
#[test]
fn none_kept_is_used_and_none_given_is_kept() {
    let r = run();
    r.answer(None, "system");
    r.keep("none\n");
    let (n, again, text) = r.scan(&[]);
    assert_eq!(n, 1, "{text}");
    assert!(!again, "{text}");
    assert_eq!(std::fs::read_to_string(r.kept()).unwrap(), "none\n");
}

#[test]
fn none_given_replaces_one_kept() {
    let r = run();
    r.answer(None, "system");
    r.keep(&format!("system\t{}\n", r.ignoring.display()));
    let (n, again, text) = r.scan(&[]);
    assert_eq!(n, 1, "{text}");
    assert!(again, "{text}");
    assert_eq!(std::fs::read_to_string(r.kept()).unwrap(), "none\n");
}

/// As git: `GIT_CONFIG_SYSTEM` replaces a kept installation config in the
/// system scope.
#[test]
fn git_config_system_replaces_a_kept_system_scoped_one() {
    let r = run();
    r.answer(Some(&r.ignoring), "system");
    r.keep(&format!("system\t{}\n", r.ignoring.display()));
    let (n, _, text) = r.scan(&[("GIT_CONFIG_SYSTEM", &r.plain)]);
    assert_eq!(n, 1, "{text}");
}

/// But not one in another scope (Apple's git's).
#[test]
fn git_config_system_leaves_a_kept_one_of_another_scope() {
    let r = run();
    r.answer(Some(&r.ignoring), "unknown");
    r.keep(&format!("other\t{}\n", r.ignoring.display()));
    let (n, again, text) = r.scan(&[("GIT_CONFIG_SYSTEM", &r.plain)]);
    assert_eq!(n, 0, "{text}");
    assert!(!again, "{text}");
}

/// With an answer kept, opening a repo doesn't wait for git: the repo's
/// status says how long its open took.
#[test]
fn a_kept_answer_spares_opening_a_repo_the_wait_for_git() {
    let r = run();
    r.answer(Some(&r.ignoring), "system");
    r.slow("1");
    let waited = |text: &str| {
        let line = text
            .lines()
            .find(|l| l.starts_with("said status "))
            .unwrap();
        line.contains("open 1.") || line.contains("open 2.")
    };
    // nothing kept: the open waits for git
    let (_, _, text) = r.scan(&[]);
    assert!(waited(&text), "{text}");
    // kept, a file or none: it doesn't
    let (n, _, text) = r.scan(&[]);
    assert!(!waited(&text), "{text}");
    assert_eq!(n, 0);
    r.answer(None, "system");
    r.keep("none\n");
    let (_, again, text) = r.scan(&[]);
    assert!(!waited(&text) && !again, "{text}");
}
