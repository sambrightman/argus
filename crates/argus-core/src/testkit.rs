//! Test fixtures: real repositories built with the `git` CLI in temp dirs,
//! isolated from the developer's own git configuration.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use tempfile::TempDir;

static GLOBAL: OnceLock<(TempDir, PathBuf)> = OnceLock::new();

/// Private global gitconfig shared by all tests in the process.
fn global_config() -> &'static Path {
    let (_, p) = GLOBAL.get_or_init(|| {
        let d = tempfile::tempdir().expect("tempdir");
        let p = d.path().join("gitconfig");
        std::fs::write(
            &p,
            "[user]\n\tname = Test\n\temail = test@example.com\n\
             [init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n\
             [protocol \"file\"]\n\tallow = always\n\
             [advice]\n\tdetachedHead = false\n",
        )
        .unwrap();
        crate::isolation::isolate(vec![
            ("GIT_CONFIG_GLOBAL".into(), p.clone().into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]);
        (d, p)
    });
    p
}

/// Run git in `dir`, panicking with its output on failure. Returns stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let cfg = global_config();
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", cfg)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

pub fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
}

pub struct Sandbox {
    pub dir: TempDir,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    pub fn new() -> Self {
        global_config();
        let dir = tempfile::tempdir().unwrap();
        Self { dir }
    }

    /// Canonical root (macOS tmp dirs are symlinked).
    pub fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    /// A local repo with one commit and no remote.
    pub fn init(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-q"]);
        write(&p, "README", "hello\n");
        commit(&p, "initial");
        p
    }

    /// A bare "forge" repo at `forge/<owner>/<name>.git` with one commit on main.
    pub fn remote(&self, owner: &str, name: &str) -> PathBuf {
        let bare = self.path(&format!("forge/{owner}/{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);
        let seed = self.path(&format!("seed/{owner}/{name}"));
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q"]);
        write(&seed, "README", &format!("{name}\n"));
        commit(&seed, "initial");
        git(&seed, &["push", "-q", bare.to_str().unwrap(), "main"]);
        bare
    }

    /// Clone a `remote()` as if it came from `https://github.com/<owner>/<name>`:
    /// the remote URL looks like GitHub's, and an `insteadOf` rewrite in the
    /// clone's config points git at the local bare repo.
    pub fn clone_as_github(&self, owner: &str, name: &str, rel: &str) -> PathBuf {
        let forge = format!("{}/", self.path("forge").display());
        let rewrite = format!("url.{forge}.insteadOf=https://github.com/");
        let url = format!("https://github.com/{owner}/{name}.git");
        let dest = self.path(rel);
        git(
            &self.root(),
            &[
                "-c",
                &rewrite,
                "clone",
                "-q",
                "--config",
                &rewrite,
                &url,
                dest.to_str().unwrap(),
            ],
        );
        dest
    }

    /// Push a new commit to the remote from a scratch clone, so that clones fall behind.
    pub fn advance_remote(&self, owner: &str, name: &str, file: &str) {
        let bare = self.path(&format!("forge/{owner}/{name}.git"));
        let tmp = self.path(&format!("scratch/{owner}/{name}-{file}"));
        git(
            &self.root(),
            &["clone", "-q", bare.to_str().unwrap(), tmp.to_str().unwrap()],
        );
        write(&tmp, file, "remote change\n");
        commit(&tmp, &format!("remote {file}"));
        git(&tmp, &["push", "-q", "origin", "HEAD"]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}

/// Environment for running argus/git as a subprocess in isolation: private
/// git config, and `HOME`/XDG dirs inside `home`.
pub fn subprocess_env(home: &Path) -> Vec<(String, String)> {
    let cfg = global_config();
    vec![
        ("GIT_CONFIG_GLOBAL".into(), cfg.display().to_string()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("HOME".into(), home.display().to_string()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").display().to_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            home.join(".state").display().to_string(),
        ),
        ("NO_COLOR".into(), "1".into()),
        ("GH_TOKEN".into(), String::new()),
    ]
}
