//! Fast, parallel discovery of repositories under root directories.
//!
//! This is pure filesystem work (no git parsing beyond reading a `.git` file),
//! parallelised with rayon. Each directory costs one `lstat` of `.git` and,
//! unless that settles it, one listing (which also detects bare repos).
//! Symlinks are only followed when asked, with cycle detection.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsString;
use std::fs::{self, FileType};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// Directories not descended into (roots are always scanned).
    pub skip: Skip,
    /// 0: no limit.
    pub max_depth: usize,
    /// Keep descending into a repository's work tree looking for nested repos.
    pub scan_inside_repos: bool,
    /// Don't descend into directories whose name starts with `.` (roots
    /// themselves are always scanned).
    pub skip_hidden: bool,
    /// Follow symlinks to directories. Cycles are cut, and a repo reachable
    /// by several paths is reported once: by its real path if that was
    /// walked, else by the first path in sort order.
    pub follow_symlinks: bool,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            skip: Skip::default(),
            max_depth: 6,
            scan_inside_repos: false,
            skip_hidden: true,
            follow_symlinks: false,
        }
    }
}

/// Directories to skip: names (patterns without `/`) and paths (with one;
/// relative ones below each root). See `skip` in the config.
#[derive(Debug, Clone, Default)]
pub struct Skip {
    names: globset::GlobSet,
    paths: globset::GlobSet,
}

impl Skip {
    /// Compile `patterns`; relative paths are joined to each of `roots`.
    /// On error, returns the offending pattern.
    pub fn new(patterns: &[String], roots: &[PathBuf]) -> Result<Self, (String, globset::Error)> {
        let glob = |p: &str, original: &str| {
            globset::GlobBuilder::new(p)
                .literal_separator(true)
                .case_insensitive(true)
                .build()
                .map_err(|e| (original.to_string(), e))
        };
        let mut names = globset::GlobSetBuilder::new();
        let mut paths = globset::GlobSetBuilder::new();
        for original in patterns {
            let p = original.trim_end_matches('/');
            if !p.contains('/') && !p.starts_with('~') {
                names.add(glob(p, original)?);
            } else if p.starts_with('/') || p.starts_with('~') {
                paths.add(glob(&resolve_prefix(p), original)?);
            } else {
                for root in roots {
                    let root = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                    let full = format!("{}/{p}", globset::escape(&root.display().to_string()));
                    paths.add(glob(&resolve_prefix(&full), original)?);
                }
            }
        }
        let build = |b: globset::GlobSetBuilder| b.build().map_err(|e| (patterns.join(", "), e));
        Ok(Self {
            names: build(names)?,
            paths: build(paths)?,
        })
    }

    pub fn matches(&self, name: &str, path: &Path) -> bool {
        self.names.is_match(name) || self.paths.is_match(path)
    }
}

/// `pattern` with `~` expanded and its leading directories before any glob
/// character canonicalized (escaped, as they're now literal), to match the
/// canonical paths scans walk.
fn resolve_prefix(pattern: &str) -> String {
    let pattern = crate::paths::expand_tilde(Path::new(pattern))
        .display()
        .to_string();
    let glob_at = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    let split = pattern[..glob_at].rfind('/').unwrap_or(0);
    let (prefix, rest) = pattern.split_at(split);
    match fs::canonicalize(prefix) {
        Ok(real) if !prefix.is_empty() => {
            format!("{}{rest}", globset::escape(&real.display().to_string()))
        }
        _ => pattern,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub found: Vec<Found>,
    /// Directories at `max_depth` that had subdirectories we didn't enter.
    pub truncated: Vec<PathBuf>,
}

/// Find repositories under `roots`. Results are sorted by path and de-duplicated
/// (roots may overlap).
pub fn discover(roots: &[PathBuf], opts: &DiscoverOptions) -> Vec<Found> {
    discover_all(roots, opts).found
}

/// Like [`discover`], also reporting where `max_depth` cut the walk short.
pub fn discover_all(roots: &[PathBuf], opts: &DiscoverOptions) -> Discovery {
    let sink = Sink::default();
    rayon::scope(|s| {
        for root in roots {
            let Ok(root) = fs::canonicalize(root) else {
                continue;
            };
            let sink = &sink;
            s.spawn(move |s| visit(s, root, 0, opts, sink, Arc::default()));
        }
    });
    let mut found = sink.found.into_inner().unwrap();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    if opts.follow_symlinks {
        found = dedup_real(found);
    }
    let mut truncated = sink.truncated.into_inner().unwrap();
    truncated.sort();
    truncated.dedup();
    Discovery { found, truncated }
}

#[derive(Default)]
struct Sink {
    found: Mutex<Vec<Found>>,
    truncated: Mutex<Vec<PathBuf>>,
}

/// Real directories at which the walk to here followed a symlink: entering
/// a link whose target contains any of them (or the current directory)
/// would walk in a circle.
type Links = Arc<Vec<PathBuf>>;

fn visit<'s>(
    s: &rayon::Scope<'s>,
    dir: PathBuf,
    depth: usize,
    opts: &'s DiscoverOptions,
    sink: &'s Sink,
    links: Links,
) {
    let mut kind = classify_dotgit(&dir);
    if let Some(kind) = kind
        && !opts.scan_inside_repos
    {
        sink.found.lock().unwrap().push(Found { path: dir, kind });
        return;
    }
    let entries = list(&dir);
    if kind.is_none() && is_bare_listing(&entries) {
        kind = Some(RepoKind::Bare);
    }
    if let Some(kind) = kind {
        sink.found.lock().unwrap().push(Found {
            path: dir.clone(),
            kind,
        });
        if kind == RepoKind::Bare {
            return;
        }
    }
    let subdirs = subdirs(&dir, entries, opts, &links);
    if opts.max_depth != 0 && depth >= opts.max_depth {
        if !subdirs.is_empty() {
            sink.truncated.lock().unwrap().push(dir);
        }
        return;
    }
    for (path, links) in subdirs {
        s.spawn(move |s| visit(s, path, depth + 1, opts, sink, links));
    }
}

struct DirEntry {
    name: OsString,
    path: PathBuf,
    ty: FileType,
}

fn list(dir: &Path) -> Vec<DirEntry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            Some(DirEntry {
                ty: e.file_type().ok()?, // from the listing; no extra stat
                name: e.file_name(),
                path: e.path(),
            })
        })
        .collect()
}

/// Directories worth descending into, each with the links taken to reach it.
fn subdirs(
    dir: &Path,
    entries: Vec<DirEntry>,
    opts: &DiscoverOptions,
    links: &Links,
) -> Vec<(PathBuf, Links)> {
    let mut real_dir = None;
    entries
        .into_iter()
        .filter(|e| {
            let name = e.name.to_string_lossy();
            name != ".git"
                && !(opts.skip_hidden && name.starts_with('.'))
                && !opts.skip.matches(&name, &e.path)
        })
        .filter_map(|e| {
            if e.ty.is_dir() {
                return Some((e.path, links.clone()));
            }
            if !(opts.follow_symlinks && e.ty.is_symlink()) {
                return None;
            }
            let target = fs::canonicalize(&e.path).ok()?;
            if !target.is_dir() {
                return None;
            }
            let here = real_dir
                .get_or_insert_with(|| fs::canonicalize(dir).unwrap_or_else(|_| dir.into()))
                .clone();
            let mut taken = links.as_ref().clone();
            taken.push(here);
            if taken.iter().any(|l| l.starts_with(&target)) {
                return None; // would walk in a circle
            }
            Some((e.path, Arc::new(taken)))
        })
        .collect()
}

/// With symlinks followed, keep one path per real repo: the real path if it
/// was found, else the first in sort order (`found` is sorted).
fn dedup_real(found: Vec<Found>) -> Vec<Found> {
    let mut by_real: BTreeMap<PathBuf, Found> = BTreeMap::new();
    for f in found {
        let real = fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
        match by_real.entry(real) {
            Entry::Vacant(v) => {
                v.insert(f);
            }
            Entry::Occupied(mut o) => {
                if &f.path == o.key() {
                    o.insert(f);
                }
            }
        }
    }
    let mut v: Vec<Found> = by_real.into_values().collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// Decide whether `dir` is a repository, and which kind.
pub fn classify(dir: &Path) -> Option<RepoKind> {
    classify_dotgit(dir).or_else(|| is_bare_listing(&list(dir)).then_some(RepoKind::Bare))
}

/// A work tree's kind, from its `.git` (one `lstat`).
fn classify_dotgit(dir: &Path) -> Option<RepoKind> {
    let dotgit = dir.join(".git");
    match fs::symlink_metadata(&dotgit) {
        Ok(m) if m.is_dir() => Some(RepoKind::Main),
        Ok(m) if m.is_file() => Some(classify_gitfile(&dotgit)),
        _ => None,
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

/// A bare repo has `HEAD`, `objects/` and `refs/`; read from the listing.
fn is_bare_listing(entries: &[DirEntry]) -> bool {
    let has = |name: &str, want: fn(&FileType) -> bool| {
        entries.iter().any(|e| e.name == name && want(&e.ty))
    };
    has("HEAD", FileType::is_file)
        && has("objects", FileType::is_dir)
        && has("refs", FileType::is_dir)
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
            skip: Skip::new(&["node_modules".into()], &[]).unwrap(),
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
    fn reports_where_max_depth_stopped_the_walk() {
        let t = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(t.path()).unwrap();
        fake_repo(&root, "a/b/deep");
        mk(&root, "a/leaf"); // at the limit but empty: nothing missed
        mk(&root, "x/y/node_modules"); // only skipped dirs below: nothing missed
        fake_repo(&root, "a/repo-at-limit");
        let opts = DiscoverOptions {
            max_depth: 2,
            skip: Skip::new(&["node_modules".into()], &[]).unwrap(),
            ..Default::default()
        };
        let d = discover_all(std::slice::from_ref(&root), &opts);
        assert_eq!(d.truncated, vec![root.join("a/b")]);
        assert_eq!(
            d.found.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            vec![root.join("a/repo-at-limit")]
        );
        // inverse: deep enough, nothing truncated
        let opts = DiscoverOptions {
            max_depth: 10,
            ..opts
        };
        let d = discover_all(std::slice::from_ref(&root), &opts);
        assert!(d.truncated.is_empty(), "{:?}", d.truncated);
        assert_eq!(d.found.len(), 2);
    }

    #[test]
    fn max_depth_zero_means_no_limit() {
        let t = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(t.path()).unwrap();
        fake_repo(&root, "1/2/3/4/5/6/7/8/9/deep");
        let opts = DiscoverOptions {
            max_depth: 0,
            ..Default::default()
        };
        let d = discover_all(std::slice::from_ref(&root), &opts);
        assert_eq!(d.found.len(), 1);
        assert!(d.truncated.is_empty(), "{:?}", d.truncated);
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

    fn bare(dir: &Path, rel: &str) -> PathBuf {
        let p = mk(dir, rel);
        mk(&p, "objects");
        mk(&p, "refs");
        fs::write(p.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        p
    }

    #[test]
    fn finds_bare_repos_from_the_listing_but_not_lookalikes() {
        let t = tempfile::tempdir().unwrap();
        bare(t.path(), "org/b.git");
        let fake = mk(t.path(), "org/not-bare");
        mk(&fake, "refs");
        fs::write(fake.join("HEAD"), "x").unwrap();
        fs::write(fake.join("objects"), "a file, not a dir").unwrap();
        mk(&fake, "inside/x");
        fake_repo(t.path(), "org/not-bare/inside/real");
        assert_eq!(
            found(t.path(), &DiscoverOptions::default()),
            vec![
                ("org/b.git".into(), RepoKind::Bare),
                ("org/not-bare/inside/real".into(), RepoKind::Main),
            ]
        );
    }

    #[test]
    fn hidden_directories_are_skipped_unless_a_root_or_enabled() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "a");
        fake_repo(t.path(), ".config/emacs");
        fake_repo(t.path(), "dev/.hidden-repo");
        let opts = DiscoverOptions::default();
        assert_eq!(found(t.path(), &opts), vec![("a".into(), RepoKind::Main)]);
        // a hidden root is scanned (its hidden children still aren't)
        assert_eq!(
            found(&t.path().join(".config"), &opts),
            vec![("emacs".into(), RepoKind::Main)]
        );
        let opts = DiscoverOptions {
            skip_hidden: false,
            ..opts
        };
        assert_eq!(found(t.path(), &opts).len(), 3);
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

    #[cfg(unix)]
    #[test]
    fn followed_symlinks_find_repos_once_and_never_loop() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let root = mk(t.path(), "root");
        let out = mk(t.path(), "elsewhere");
        fake_repo(&out, "ext/r");
        fake_repo(&root, "own");
        symlink(out.join("ext"), root.join("l1")).unwrap();
        symlink(out.join("ext"), root.join("l2")).unwrap(); // same target twice
        symlink(root.join("own"), root.join("an-alias")).unwrap(); // sorts before the real one
        // a cycle through two directories, neither an ancestor of the other
        let x = mk(&root, "x");
        let y = mk(&root, "y");
        fake_repo(&x, "rx");
        symlink(&y, x.join("to-y")).unwrap();
        symlink(&x, y.join("to-x")).unwrap();
        symlink(&root, root.join("self")).unwrap();
        let opts = DiscoverOptions {
            follow_symlinks: true,
            // below the OS's own symlink limit (ELOOP), so only our cycle
            // check can keep the walk from reaching max_depth
            max_depth: 12,
            ..Default::default()
        };
        assert_eq!(
            found(&root, &opts),
            vec![
                ("l1/r".into(), RepoKind::Main), // first of the links by path
                ("own".into(), RepoKind::Main),  // the real path wins over an alias
                ("x/rx".into(), RepoKind::Main),
            ]
        );
        // cycles are cut, not merely stopped by max_depth
        let canon = fs::canonicalize(&root).unwrap();
        let d = discover_all(std::slice::from_ref(&canon), &opts);
        assert!(d.truncated.is_empty(), "{:?}", d.truncated);
        // inverse: not followed by default
        assert_eq!(
            found(&root, &DiscoverOptions::default()),
            vec![
                ("own".into(), RepoKind::Main),
                ("x/rx".into(), RepoKind::Main)
            ]
        );
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
