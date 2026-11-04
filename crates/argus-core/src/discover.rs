//! Fast, parallel discovery of repositories under root directories.
//!
//! This is pure filesystem work (no git parsing beyond reading a `.git` file),
//! parallelised with rayon. Symlinks are not followed, which avoids loops.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum RepoKind {
    /// A normal clone with a work tree.
    Main,
    /// A linked worktree (`git worktree add`).
    Worktree,
    /// A checked-out submodule.
    Submodule,
    /// A bare repository (no work tree).
    Bare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Work tree directory, or the git dir for bare repos.
    pub path: PathBuf,
    pub kind: RepoKind,
}

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub skip_dirs: Vec<String>,
    pub max_depth: usize,
    /// Keep descending into a repository's work tree looking for nested repos.
    pub scan_inside_repos: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            skip_dirs: vec![],
            max_depth: 6,
            scan_inside_repos: false,
        }
    }
}

/// Find repositories under `roots`. Results are sorted by path and de-duplicated
/// (roots may overlap).
pub fn discover(roots: &[PathBuf], opts: &DiscoverOptions) -> Vec<Found> {
    let out = Mutex::new(Vec::new());
    rayon::scope(|s| {
        for root in roots {
            let Ok(root) = fs::canonicalize(root) else {
                continue;
            };
            let out = &out;
            s.spawn(move |s| visit(s, root, 0, opts, out));
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v.dedup_by(|a, b| a.path == b.path);
    v
}

fn visit<'s>(
    s: &rayon::Scope<'s>,
    dir: PathBuf,
    depth: usize,
    opts: &'s DiscoverOptions,
    out: &'s Mutex<Vec<Found>>,
) {
    if let Some(kind) = classify(&dir) {
        out.lock().unwrap().push(Found {
            path: dir.clone(),
            kind,
        });
        if kind == RepoKind::Bare || !opts.scan_inside_repos {
            return;
        }
    }
    if depth >= opts.max_depth {
        return;
    }
    let Ok(rd) = fs::read_dir(&dir) else { return };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() {
            continue; // includes symlinks: never followed
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || opts.skip_dirs.iter().any(|s| s == name.as_ref()) {
            continue;
        }
        let path = entry.path();
        s.spawn(move |s| visit(s, path, depth + 1, opts, out));
    }
}

/// Decide whether `dir` is a repository, and which kind.
pub fn classify(dir: &Path) -> Option<RepoKind> {
    let dotgit = dir.join(".git");
    match fs::symlink_metadata(&dotgit) {
        Ok(m) if m.is_dir() => return Some(RepoKind::Main),
        Ok(m) if m.is_file() => return Some(classify_gitfile(&dotgit)),
        _ => {}
    }
    if is_bare(dir) {
        Some(RepoKind::Bare)
    } else {
        None
    }
}

fn classify_gitfile(dotgit: &Path) -> RepoKind {
    let Ok(s) = fs::read_to_string(dotgit) else {
        return RepoKind::Main;
    };
    let target = s
        .trim()
        .strip_prefix("gitdir:")
        .unwrap_or("")
        .trim()
        .replace('\\', "/");
    let parent = target.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if parent.ends_with("/worktrees") || parent == "worktrees" {
        RepoKind::Worktree
    } else if target.contains("/modules/") || target.starts_with("modules/") {
        RepoKind::Submodule
    } else {
        // `git clone --separate-git-dir`
        RepoKind::Main
    }
}

fn is_bare(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn fake_repo(dir: &Path, rel: &str) -> PathBuf {
        let p = mk(dir, rel);
        fs::create_dir_all(p.join(".git")).unwrap();
        p
    }

    fn found(root: &Path, opts: &DiscoverOptions) -> Vec<(String, RepoKind)> {
        let root = fs::canonicalize(root).unwrap();
        discover(std::slice::from_ref(&root), opts)
            .into_iter()
            .map(|f| {
                (
                    f.path.strip_prefix(&root).unwrap().display().to_string(),
                    f.kind,
                )
            })
            .collect()
    }

    #[test]
    fn finds_nested_containers_and_skips() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "a");
        fake_repo(t.path(), "org/b");
        fake_repo(t.path(), "work/org/team/c");
        fake_repo(t.path(), "proj/node_modules/dep");
        mk(t.path(), "empty/dir");
        let opts = DiscoverOptions {
            skip_dirs: vec!["node_modules".into()],
            ..Default::default()
        };
        assert_eq!(
            found(t.path(), &opts),
            vec![
                ("a".into(), RepoKind::Main),
                ("org/b".into(), RepoKind::Main),
                ("work/org/team/c".into(), RepoKind::Main)
            ]
        );
    }

    #[test]
    fn respects_max_depth() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "1/2/3/deep");
        let shallow = DiscoverOptions {
            max_depth: 3,
            ..Default::default()
        };
        assert!(found(t.path(), &shallow).is_empty());
        let deep = DiscoverOptions {
            max_depth: 4,
            ..Default::default()
        };
        assert_eq!(found(t.path(), &deep).len(), 1);
    }

    #[test]
    fn nested_repo_only_when_asked() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "outer");
        fake_repo(t.path(), "outer/inner");
        assert_eq!(found(t.path(), &DiscoverOptions::default()).len(), 1);
        let opts = DiscoverOptions {
            scan_inside_repos: true,
            ..Default::default()
        };
        assert_eq!(found(t.path(), &opts).len(), 2);
    }

    #[test]
    fn does_not_descend_into_dot_git() {
        let t = tempfile::tempdir().unwrap();
        let r = fake_repo(t.path(), "r");
        // a bare-looking structure inside .git must not be reported
        mk(&r, ".git/modules/sub/objects");
        mk(&r, ".git/modules/sub/refs");
        fs::write(r.join(".git/modules/sub/HEAD"), "ref: refs/heads/main\n").unwrap();
        let opts = DiscoverOptions {
            scan_inside_repos: true,
            ..Default::default()
        };
        assert_eq!(found(t.path(), &opts), vec![("r".into(), RepoKind::Main)]);
    }

    #[test]
    fn classifies_gitfiles_and_bare() {
        let t = tempfile::tempdir().unwrap();
        let wt = mk(t.path(), "wt");
        fs::write(wt.join(".git"), "gitdir: /x/main/.git/worktrees/wt\n").unwrap();
        let sm = mk(t.path(), "sm");
        fs::write(sm.join(".git"), "gitdir: ../.git/modules/sm\n").unwrap();
        let sep = mk(t.path(), "sep");
        fs::write(sep.join(".git"), "gitdir: /elsewhere/sep.git\n").unwrap();
        let bare = mk(t.path(), "bare.git");
        mk(&bare, "objects");
        mk(&bare, "refs");
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(classify(&wt), Some(RepoKind::Worktree));
        assert_eq!(classify(&sm), Some(RepoKind::Submodule));
        assert_eq!(classify(&sep), Some(RepoKind::Main));
        assert_eq!(classify(&bare), Some(RepoKind::Bare));
        assert_eq!(classify(t.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "a/r");
        std::os::unix::fs::symlink(t.path(), t.path().join("a/loop")).unwrap();
        std::os::unix::fs::symlink(t.path().join("a/r"), t.path().join("alias")).unwrap();
        let opts = DiscoverOptions {
            max_depth: 50,
            ..Default::default()
        };
        assert_eq!(found(t.path(), &opts), vec![("a/r".into(), RepoKind::Main)]);
    }

    #[test]
    fn overlapping_roots_dedupe_and_missing_roots_ignored() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "x/r");
        let roots = vec![
            t.path().to_path_buf(),
            t.path().join("x"),
            t.path().join("missing"),
        ];
        assert_eq!(discover(&roots, &DiscoverOptions::default()).len(), 1);
    }
}
