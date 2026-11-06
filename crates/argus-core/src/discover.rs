//! Fast, parallel discovery of repositories under root directories.
//!
//! This is pure filesystem work (no git parsing beyond reading a `.git` file),
//! parallelised with rayon. Each directory costs one `lstat` of `.git` and,
//! unless that settles it, one listing (which also detects bare repos).
//! Symlinks are only followed when asked, with cycle detection.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};
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
            } else {
                let full: Vec<String> = if p.starts_with('/') || p.starts_with('~') {
                    vec![p.to_string()]
                } else {
                    roots
                        .iter()
                        .map(|r| {
                            let r = fs::canonicalize(r).unwrap_or_else(|_| r.clone());
                            format!("{}/{p}", globset::escape(&r.display().to_string()))
                        })
                        .collect()
                };
                // Resolved only: walks start from real roots, and a
                // directory's spellings include the one with these links
                // resolved, so the pattern as written can't match otherwise.
                for f in full {
                    let f = crate::paths::expand_tilde(Path::new(&f));
                    paths.add(glob(&resolve_literal(&f.display().to_string()), original)?);
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

/// `pattern` with its literal part (all of it, or up to the directory
/// before the first glob character) resolved to a real path, following
/// every symlink: the longest existing part is canonicalized and the rest
/// kept. Resolved parts are escaped, as they're now literal.
fn resolve_literal(pattern: &str) -> String {
    let literal_end = match pattern.find(['*', '?', '[', '{']) {
        Some(at) => pattern[..at].rfind('/').unwrap_or(0),
        None => pattern.len(),
    };
    let (literal, rest) = pattern.split_at(literal_end);
    let mut existing = Path::new(literal);
    let mut tail: Vec<String> = vec![];
    while !existing.as_os_str().is_empty() {
        if let Ok(real) = fs::canonicalize(existing) {
            let mut out = globset::escape(&real.display().to_string());
            for t in tail.iter().rev() {
                out.push('/');
                out.push_str(&globset::escape(t));
            }
            return out + rest;
        }
        let Some(name) = existing.file_name() else {
            break;
        };
        tail.push(name.to_string_lossy().into_owned());
        existing = existing.parent().unwrap_or(Path::new(""));
    }
    pattern.to_string()
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
    let real_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| fs::canonicalize(r).ok())
        .collect();
    rayon::scope(|s| {
        for root in &real_roots {
            let sink = &sink;
            s.spawn(move |s| visit(s, root.clone(), 0, opts, sink, Arc::default()));
        }
    });
    let mut found = sink.found.into_inner().unwrap();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    if opts.follow_symlinks {
        let skipped: HashSet<PathBuf> = sink.skipped.into_inner().unwrap().into_iter().collect();
        found = dedup_real(found, &skipped, &real_roots);
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
    /// Real paths of skipped directories (with `-L` only): a directory
    /// skipped on one route is skipped on every route.
    skipped: Mutex<Vec<PathBuf>>,
}

/// A symlink the walk followed to get here.
#[derive(Debug)]
struct Hop {
    /// The real directory holding the link: entering a link whose target
    /// contains any hop's `from` (or the current directory) would walk in
    /// a circle.
    from: PathBuf,
    /// The link's path as walked.
    link: PathBuf,
    /// Its target's real path.
    target: PathBuf,
}

/// The symlinks followed on the way down, outermost first.
type Links = Arc<Vec<Hop>>;

/// Every spelling of the walked `path` below `links`: as walked, then
/// with each link resolved in turn (the last is the real path).
fn spellings(path: &Path, links: &[Hop]) -> Vec<PathBuf> {
    let mut v = vec![path.to_path_buf()];
    for hop in links {
        if let Ok(rest) = path.strip_prefix(&hop.link) {
            v.push(hop.target.join(rest));
        }
    }
    v
}

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
    let subdirs = subdirs(&dir, entries, opts, &links, sink);
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

/// Directories worth descending into, each with the links taken to reach
/// it. A directory is skipped if any of its spellings (or, for a link, its
/// target's) matches.
fn subdirs(
    dir: &Path,
    entries: Vec<DirEntry>,
    opts: &DiscoverOptions,
    links: &Links,
    sink: &Sink,
) -> Vec<(PathBuf, Links)> {
    let mut real_dir = None;
    let skipped = |real: PathBuf| {
        if opts.follow_symlinks {
            sink.skipped.lock().unwrap().push(real);
        }
    };
    entries
        .into_iter()
        .filter_map(|e| {
            let name = e.name.to_string_lossy();
            if name == ".git" || (opts.skip_hidden && name.starts_with('.')) {
                return None;
            }
            // Below no link (always without -L) the walked path is the
            // only spelling: no allocation on this hot path.
            let skip_spelling = || {
                if links.is_empty() {
                    opts.skip.matches(&name, &e.path).then(|| e.path.clone())
                } else {
                    let all = spellings(&e.path, links);
                    let hit = all.iter().any(|p| opts.skip.matches(&name, p));
                    hit.then(|| all.last().unwrap().clone())
                }
            };
            if e.ty.is_dir() {
                if let Some(real) = skip_spelling() {
                    skipped(real);
                    return None;
                }
                return Some((e.path, links.clone()));
            }
            if !(opts.follow_symlinks && e.ty.is_symlink()) {
                return None;
            }
            let target = fs::canonicalize(&e.path).ok()?;
            if !target.is_dir() {
                return None;
            }
            let target_name = target.file_name().map(|n| n.to_string_lossy());
            if skip_spelling().is_some()
                || opts
                    .skip
                    .matches(target_name.as_deref().unwrap_or(""), &target)
            {
                skipped(target);
                return None;
            }
            let here = real_dir
                .get_or_insert_with(|| fs::canonicalize(dir).unwrap_or_else(|_| dir.into()))
                .clone();
            if walks_in_a_circle(&here, &target, links) {
                return None;
            }
            let mut taken: Vec<Hop> = links
                .iter()
                .map(|h| Hop {
                    from: h.from.clone(),
                    link: h.link.clone(),
                    target: h.target.clone(),
                })
                .collect();
            taken.push(Hop {
                from: here,
                link: e.path.clone(),
                target,
            });
            Some((e.path, Arc::new(taken)))
        })
        .collect()
}

/// Whether following a link in the real directory `here` to the real
/// `target` would walk in a circle: `target` contains `here` or any
/// directory where an earlier link was taken. A target inside `here` is
/// fine (it's a shortcut into the subtree, not a loop).
fn walks_in_a_circle(here: &Path, target: &Path, links: &[Hop]) -> bool {
    here.starts_with(target) || links.iter().any(|h| h.from.starts_with(target))
}

/// With symlinks followed, keep one path per real repo: the real path if it
/// was found, else the first in sort order (`found` is sorted). Repos inside
/// a directory skipped on some route are dropped, unless a root is at or
/// inside that directory (roots are always scanned).
fn dedup_real(found: Vec<Found>, skipped: &HashSet<PathBuf>, roots: &[PathBuf]) -> Vec<Found> {
    let mut by_real: BTreeMap<PathBuf, Found> = BTreeMap::new();
    for f in found {
        let real = fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone());
        let inside_skipped = real.ancestors().find(|a| skipped.contains(*a));
        if let Some(dir) = inside_skipped
            && !roots
                .iter()
                .any(|r| r.starts_with(dir) && real.starts_with(r))
        {
            continue;
        }
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

    /// Repos found from `roots` (relative to `base`), skipping `skip`
    /// patterns written relative to `base` (made absolute), with `-L`.
    #[cfg(unix)]
    fn found_skipping(base: &Path, roots: &[&str], skip: &[&str], follow: bool) -> Vec<String> {
        let roots: Vec<PathBuf> = roots.iter().map(|r| base.join(r)).collect();
        let skip: Vec<String> = skip
            .iter()
            .map(|p| {
                if p.contains('/') {
                    format!("{}/{p}", base.display())
                } else {
                    p.to_string()
                }
            })
            .collect();
        let opts = DiscoverOptions {
            skip: Skip::new(&skip, &roots).unwrap(),
            follow_symlinks: follow,
            ..Default::default()
        };
        let base = fs::canonicalize(base).unwrap();
        let mut v: Vec<String> = discover(&roots, &opts)
            .into_iter()
            .map(|f| {
                f.path
                    .strip_prefix(&base)
                    .unwrap_or(&f.path)
                    .display()
                    .to_string()
            })
            .collect();
        v.sort();
        v
    }

    /// `dev/alias -> data/proj`, holding repos `vendor/lib` and `keep`;
    /// `dev/a -> x`, `x/b -> y`, holding repo `c/r`.
    #[cfg(unix)]
    fn linked_tree() -> tempfile::TempDir {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let b = t.path();
        fake_repo(b, "data/proj/vendor/lib");
        fake_repo(b, "data/proj/keep");
        mk(b, "dev");
        symlink(b.join("data/proj"), b.join("dev/alias")).unwrap();
        fake_repo(b, "y/c/r");
        mk(b, "x");
        symlink(b.join("y"), b.join("x/b")).unwrap();
        symlink(b.join("x"), b.join("dev/a")).unwrap();
        t
    }

    #[cfg(unix)]
    #[test]
    fn skip_matches_every_spelling_through_symlinks() {
        let t = linked_tree();
        let b = t.path();
        let lib = "dev/alias/vendor/lib".to_string();
        let keep = "dev/alias/keep".to_string();
        let all = found_skipping(b, &["dev"], &[], true);
        assert!(
            all.contains(&lib) && all.contains(&keep) && all.contains(&"dev/a/b/c/r".into()),
            "{all:?}"
        );
        // written through the link, or by the real path: skipped via the link
        for pattern in ["dev/alias/vendor", "data/proj/vendor", "dev/*/vendor"] {
            let v = found_skipping(b, &["dev"], &[pattern], true);
            assert!(!v.contains(&lib) && v.contains(&keep), "{pattern}: {v:?}");
        }
        // the link itself, by its path or its target's
        for pattern in ["dev/alias", "data/proj"] {
            let v = found_skipping(b, &["dev"], &[pattern], true);
            assert!(!v.contains(&lib) && !v.contains(&keep), "{pattern}: {v:?}");
        }
        // names: the link's own, or its target's
        for name in ["alias", "proj"] {
            let v = found_skipping(b, &["dev"], &[name], true);
            assert!(!v.contains(&keep), "{name}: {v:?}");
        }
        // two links in a row: the partly resolved spelling (x/b/c) counts
        // too, as do the walked (dev/a/b/c) and real (y/c) ones
        for pattern in ["x/*/c", "dev/a/*/c", "y/c", "x/b/c"] {
            let v = found_skipping(b, &["dev"], &[pattern], true);
            assert!(!v.iter().any(|p| p.ends_with("c/r")), "{pattern}: {v:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_skipped_on_one_route_is_skipped_on_every_route() {
        let t = linked_tree();
        let b = t.path();
        // dev/*/vendor matches only through the link; from the data root
        // the same directory is data/proj/vendor, which doesn't match
        let v = found_skipping(b, &["dev", "data"], &["dev/*/vendor"], true);
        assert!(!v.iter().any(|p| p.ends_with("vendor/lib")), "{v:?}");
        assert!(v.iter().any(|p| p.ends_with("keep")), "{v:?}");
        // inverse: a root inside the skipped directory is still scanned
        let v = found_skipping(b, &["dev", "data/proj/vendor"], &["dev/*/vendor"], true);
        assert!(v.iter().any(|p| p.ends_with("vendor/lib")), "{v:?}");
    }

    #[cfg(unix)]
    #[test]
    fn literal_patterns_resolve_links_without_following_them() {
        let t = linked_tree();
        let b = t.path();
        // no -L: dev/alias isn't walked, but the pattern resolves to
        // data/proj, which the data root reaches by its real path
        let v = found_skipping(b, &["data"], &["dev/alias"], false);
        assert!(v.is_empty(), "{v:?}");
        let v = found_skipping(b, &["data"], &["dev/alias/vendor"], false);
        assert_eq!(v, ["data/proj/keep"]);
        // inverse: links aren't followed, so nothing is found through them
        assert!(found_skipping(b, &["dev"], &[], false).is_empty());
    }

    #[test]
    fn a_link_loops_only_if_its_target_contains_where_the_walk_has_been() {
        let p = Path::new;
        let hop = |from: &str| Hop {
            from: p(from).into(),
            link: p("/unused").into(),
            target: p("/unused").into(),
        };
        // the target contains here: a loop
        assert!(walks_in_a_circle(p("/t/a/b"), p("/t/a"), &[]));
        assert!(walks_in_a_circle(p("/t/a"), p("/t/a"), &[]));
        assert!(walks_in_a_circle(p("/t/a"), p("/"), &[]));
        // inverse: a shortcut into here's own subtree, or elsewhere
        assert!(!walks_in_a_circle(p("/t/a"), p("/t/a/b"), &[]));
        assert!(!walks_in_a_circle(p("/t/a"), p("/t/ab"), &[]));
        assert!(!walks_in_a_circle(p("/t/a"), p("/u"), &[]));
        // the target contains where an earlier link was taken
        assert!(walks_in_a_circle(p("/u/x"), p("/t"), &[hop("/t/a")]));
        assert!(!walks_in_a_circle(p("/u/x"), p("/v"), &[hop("/t/a")]));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_a_skipped_part_of_its_own_subtree_is_followed() {
        let t = tempfile::tempdir().unwrap();
        fake_repo(t.path(), "a/.stash/r");
        std::os::unix::fs::symlink(t.path().join("a/.stash"), t.path().join("a/visible")).unwrap();
        let opts = DiscoverOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        // .stash is hidden, so the link is the only way to r
        assert_eq!(
            found(t.path(), &opts),
            vec![("a/visible/r".into(), RepoKind::Main)]
        );
        // inverse: not following links, r isn't found
        assert!(found(t.path(), &DiscoverOptions::default()).is_empty());
    }
}
