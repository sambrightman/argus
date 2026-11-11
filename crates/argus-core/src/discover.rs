//! Fast, parallel discovery of repositories under root directories.
//!
//! This is pure filesystem work (no git parsing beyond reading a `.git` file),
//! parallelised with rayon. Each directory costs one `lstat` of `.git` and,
//! unless that settles it, one listing (which also detects bare repos).
//! Symlinks are only followed when asked, with cycle detection. With
//! `one_file_system`, each subdirectory costs an `lstat` as well.

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
    /// Don't enter directories (or follow links to them) on a different
    /// filesystem from their root's, like `find -xdev`. Unix only.
    pub one_file_system: bool,
    #[doc(hidden)]
    pub tuning: Tuning,
}

/// Optimizations of the `-L` walk, switchable to compare against the plain
/// walk (tests, the `discover` example). Both on by default.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    /// Take found repos' real paths from the links followed (no syscalls)
    /// rather than canonicalizing each. Same results.
    pub real_from_links: bool,
    /// Don't follow a link whose target the plain walk reaches anyway, at
    /// the same depth or shallower, deciding everything below it the same
    /// way. Same repos; a repo reachable only through further links below
    /// such a target may be spelled by another of its paths, and
    /// truncation reports through the link's path are dropped.
    pub prune_covered_links: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            real_from_links: true,
            prune_covered_links: true,
        }
    }
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            skip: Skip::default(),
            max_depth: 6,
            scan_inside_repos: false,
            skip_hidden: true,
            follow_symlinks: false,
            one_file_system: false,
            tuning: Tuning::default(),
        }
    }
}

/// Directories to skip: names (patterns without `/`) and paths (with one;
/// relative ones below each root). See `skip` in the config.
#[derive(Debug, Clone, Default)]
pub struct Skip {
    names: globset::GlobSet,
    paths: globset::GlobSet,
    /// Each path pattern's literal leading directory, as matchers for the
    /// directory itself and for anything below it.
    path_prefixes: Vec<(PathBuf, globset::GlobMatcher, globset::GlobMatcher)>,
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
        let mut path_prefixes = vec![];
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
                    let resolved = resolve_literal(&f.display().to_string());
                    paths.add(glob(&resolved, original)?);
                    let prefix = match literal_dir(&resolved) {
                        "" => "/",
                        d => d,
                    };
                    let escaped = globset::escape(prefix.trim_end_matches('/'));
                    path_prefixes.push((
                        PathBuf::from(prefix),
                        glob(&escaped, original)?.compile_matcher(),
                        glob(&format!("{escaped}/**"), original)?.compile_matcher(),
                    ));
                }
            }
        }
        let build = |b: globset::GlobSetBuilder| b.build().map_err(|e| (patterns.join(", "), e));
        Ok(Self {
            names: build(names)?,
            paths: build(paths)?,
            path_prefixes,
        })
    }

    pub fn matches(&self, name: &str, path: &Path) -> bool {
        self.names.is_match(name) || self.paths.is_match(path)
    }

    /// Whether some path pattern might match `dir` or a path below it:
    /// its literal leading directory is at, above or below `dir`, compared
    /// as the patterns match (case-insensitively). May say yes when the
    /// answer is no, never the reverse.
    fn may_match_below(&self, dir: &Path) -> bool {
        if self.path_prefixes.is_empty() {
            return false;
        }
        let below_dir = globset::GlobBuilder::new(&format!(
            "{}/**",
            globset::escape(&dir.display().to_string())
        ))
        .literal_separator(true)
        .case_insensitive(true)
        .build();
        let Ok(below_dir) = below_dir.map(|g| g.compile_matcher()) else {
            return true;
        };
        // the prefix at or above `dir` (its own matchers), or below it
        self.path_prefixes.iter().any(|(prefix, at, below)| {
            at.is_match(dir) || below.is_match(dir) || below_dir.is_match(prefix)
        })
    }
}

/// The literal leading directory of glob `pattern`: all of it without glob
/// characters, else up to the `/` before the first one (escapes, which
/// use `[`, count as glob characters: the result only gets shorter).
fn literal_dir(pattern: &str) -> &str {
    match pattern.find(['*', '?', '[', '{']) {
        Some(at) => &pattern[..pattern[..at].rfind('/').unwrap_or(0)],
        None => pattern,
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
    let real_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| fs::canonicalize(r).ok())
        .collect();
    let sink = Sink {
        roots: real_roots.clone(),
        ..Sink::default()
    };
    rayon::scope(|s| {
        for root in &real_roots {
            let sink = &sink;
            let walk = Walk {
                opts,
                sink,
                device: opts.one_file_system.then(|| device(root)).flatten(),
            };
            s.spawn(move |s| visit(s, root.clone(), 0, walk, Arc::default()));
        }
    });
    let mut found = sink.found.into_inner().unwrap();
    found.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    found.dedup_by(|a, b| a.0.path == b.0.path);
    let found = if opts.follow_symlinks {
        let skipped: HashSet<PathBuf> = sink.skipped.into_inner().unwrap().into_iter().collect();
        dedup_real(found, &skipped, &real_roots)
    } else {
        found.into_iter().map(|(f, _)| f).collect()
    };
    let mut truncated = sink.truncated.into_inner().unwrap();
    truncated.sort();
    truncated.dedup();
    Discovery { found, truncated }
}

#[derive(Default)]
struct Sink {
    /// The canonical roots.
    roots: Vec<PathBuf>,
    /// With each repo, its real path when known without a syscall (`-L`
    /// with `real_from_links`).
    found: Mutex<Vec<(Found, Option<PathBuf>)>>,
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
            v.push(join(&hop.target, rest));
        }
    }
    v
}

/// The real path of the walked `path` below `links`: its last spelling.
/// Exact, as roots and link targets are canonical and the walk enters
/// nothing else that could be a link.
fn real_path(path: &Path, links: &[Hop]) -> PathBuf {
    match links.last().map(|h| (h, path.strip_prefix(&h.link))) {
        None => path.to_path_buf(),
        Some((h, Ok(rest))) => join(&h.target, rest),
        Some((_, Err(_))) => fs::canonicalize(path).unwrap_or_else(|_| path.into()),
    }
}

/// `base` joined with `rest`, without the trailing separator that joining
/// an empty path adds.
fn join(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

/// What every directory under one root is walked with.
#[derive(Clone, Copy)]
struct Walk<'s> {
    opts: &'s DiscoverOptions,
    sink: &'s Sink,
    /// The root's filesystem, with `one_file_system`: directories on any
    /// other aren't entered.
    device: Option<u64>,
}

/// The filesystem `path` is on; `None` off Unix, where std has no stable
/// equivalent (so `one_file_system` does nothing). One function rather
/// than one per platform, so what tests exercise is all of it.
fn device(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path).ok().map(|m| m.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn visit<'s>(s: &rayon::Scope<'s>, dir: PathBuf, depth: usize, walk: Walk<'s>, links: Links) {
    let Walk { opts, sink, .. } = walk;
    // without -L there are no links: the path itself, dropped unused
    let real = || opts.tuning.real_from_links.then(|| real_path(&dir, &links));
    let mut kind = classify_dotgit(&dir);
    if let Some(kind) = kind
        && !opts.scan_inside_repos
    {
        let real = real();
        sink.found
            .lock()
            .unwrap()
            .push((Found { path: dir, kind }, real));
        return;
    }
    let entries = list(&dir);
    if kind.is_none() && is_bare_listing(&entries) {
        kind = Some(RepoKind::Bare);
    }
    if let Some(kind) = kind {
        let found = Found {
            path: dir.clone(),
            kind,
        };
        let real = real();
        sink.found.lock().unwrap().push((found, real));
        if kind == RepoKind::Bare {
            return;
        }
    }
    let subdirs = subdirs(&dir, depth, entries, walk, &links);
    if opts.max_depth != 0 && depth >= opts.max_depth {
        if !subdirs.is_empty() {
            sink.truncated.lock().unwrap().push(dir);
        }
        return;
    }
    for (path, links) in subdirs {
        s.spawn(move |s| visit(s, path, depth + 1, walk, links));
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

/// Whether `name` is a directory in `entries`, as the walk would enter it.
fn lists_dir(entries: &[DirEntry], name: &std::ffi::OsStr) -> bool {
    entries.iter().any(|e| e.name == name && e.ty.is_dir())
}

/// Directories worth descending into, each with the links taken to reach
/// it. A directory is skipped if any of its spellings (or, for a link, its
/// target's) matches.
fn subdirs(
    dir: &Path,
    depth: usize,
    entries: Vec<DirEntry>,
    walk: Walk,
    links: &Links,
) -> Vec<(PathBuf, Links)> {
    let Walk { opts, sink, .. } = walk;
    let elsewhere = |dir: &Path| walk.device.is_some_and(|d| device(dir) != Some(d));
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
                if elsewhere(&e.path) {
                    return None;
                }
                return Some((e.path, links.clone()));
            }
            if !(opts.follow_symlinks && e.ty.is_symlink()) {
                return None;
            }
            let target = fs::canonicalize(&e.path).ok()?;
            if !target.is_dir() || elsewhere(&target) {
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
            if opts.tuning.prune_covered_links
                && plain_walk_covers(&target, &e.path, depth, links, opts, &sink.roots)
            {
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

/// Whether the walk without links reaches `target` (canonical), and
/// decides everything below it as the walk through `link` would, so that
/// following `link` (found in a directory at `depth`) would find no repo
/// the walk doesn't find anyway:
///
/// * `link` is above `max_depth`: at the limit its directory isn't entered
///   anyway, and dropping it would change what's reported as truncated.
/// * No path pattern can match below any spelling of `link` (names are
///   the same on both routes below `target`, and real spellings too).
/// * `target` is under a root, at the same depth as through the link or
///   shallower (or there's no depth limit), and each directory on the way
///   is listed by its parent, not hidden, `.git` or skipped, and doesn't
///   stop the walk by being a repo.
///
/// Below `target` that walk has followed no links, so its cycle checks cut
/// no more than the link's route would.
fn plain_walk_covers(
    target: &Path,
    link: &Path,
    depth: usize,
    links: &[Hop],
    opts: &DiscoverOptions,
    roots: &[PathBuf],
) -> bool {
    if opts.max_depth != 0 && depth >= opts.max_depth {
        return false;
    }
    if spellings(link, links)
        .iter()
        .any(|s| opts.skip.may_match_below(s))
    {
        return false;
    }
    let depth_via_link = depth + 1;
    roots.iter().any(|root| {
        let Ok(rel) = target.strip_prefix(root) else {
            return false;
        };
        let names: Vec<_> = rel.components().map(|c| c.as_os_str().to_owned()).collect();
        if opts.max_depth != 0 && names.len() > depth_via_link {
            return false;
        }
        let mut dir = root.clone();
        for name in &names {
            // as `visit` decides, from one listing
            let entries = list(&dir);
            let stops = match classify_dotgit(&dir) {
                Some(_) => !opts.scan_inside_repos,
                None => is_bare_listing(&entries),
            };
            let listed = lists_dir(&entries, name);
            let child = dir.join(name);
            let n = name.to_string_lossy();
            if stops
                || !listed
                || n == ".git"
                || (opts.skip_hidden && n.starts_with('.'))
                || opts.skip.matches(&n, &child)
            {
                return false;
            }
            dir = child;
        }
        true
    })
}

/// With symlinks followed, keep one path per real repo: the real path if it
/// was found, else the first in sort order (`found` is sorted). Repos inside
/// a directory skipped on some route are dropped, unless a root is at or
/// inside that directory (roots are always scanned).
fn dedup_real(
    found: Vec<(Found, Option<PathBuf>)>,
    skipped: &HashSet<PathBuf>,
    roots: &[PathBuf],
) -> Vec<Found> {
    let mut by_real: BTreeMap<PathBuf, Found> = BTreeMap::new();
    for (f, real) in found {
        let real =
            real.unwrap_or_else(|| fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone()));
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

    #[test]
    fn only_a_directory_by_that_name_is_listed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("dir")).unwrap();
        std::fs::write(d.path().join("file"), "").unwrap();
        let entries = list(d.path());
        assert!(lists_dir(&entries, "dir".as_ref()));
        assert!(!lists_dir(&entries, "file".as_ref()));
        assert!(!lists_dir(&entries, "missing".as_ref()));
    }

    #[test]
    fn a_patterns_literal_dir_stops_before_the_first_glob() {
        assert_eq!(literal_dir("/a/b/*/c"), "/a/b");
        assert_eq!(literal_dir("/a/b"), "/a/b");
        assert_eq!(literal_dir("x*"), "");
    }

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

    #[cfg(unix)]
    fn with(
        opts: &DiscoverOptions,
        real_from_links: bool,
        prune_covered_links: bool,
    ) -> DiscoverOptions {
        DiscoverOptions {
            tuning: Tuning {
                real_from_links,
                prune_covered_links,
            },
            ..opts.clone()
        }
    }

    #[cfg(unix)]
    fn rel(base: &Path, paths: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
        paths
            .into_iter()
            .map(|p| p.strip_prefix(base).unwrap_or(&p).display().to_string())
            .collect()
    }

    /// Each optimization against the plain `-L` walk, on random trees with
    /// loops, link chains, links into repos and hidden directories,
    /// overlapping roots, and every kind of skip pattern
    /// (`ARGUS_RANDOM_TREES` sets how many).
    #[cfg(unix)]
    #[test]
    fn link_optimizations_agree_with_the_plain_walk_on_random_trees() {
        use std::os::unix::fs::symlink;
        let mut rng = 0x9e3779b97f4a7c15u64;
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n as u64) as usize
        };
        let names = ["a", "b", "c", "go", "pkg", "vendor", ".h", "x"];
        let mut pruned_somewhere = false;
        // 400 trees by default (fewer may not exercise pruning);
        // ARGUS_RANDOM_TREES=5000 for a thorough run
        let trees: u64 = std::env::var("ARGUS_RANDOM_TREES")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(400);
        for seed in 0..trees {
            let t = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(t.path()).unwrap();
            let mut dirs = vec![base.clone()];
            for _ in 0..20 + next(25) {
                let parent = dirs[next(dirs.len())].clone();
                if parent.components().count() - base.components().count() >= 5 {
                    continue;
                }
                let d = parent.join(names[next(names.len())]);
                fs::create_dir_all(&d).unwrap();
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
            for d in &dirs[1..] {
                match next(20) {
                    0..=4 => fs::create_dir_all(d.join(".git")).unwrap(),
                    5 => {
                        fs::write(d.join("HEAD"), "ref: refs/heads/main\n").unwrap();
                        fs::create_dir_all(d.join("objects")).unwrap();
                        fs::create_dir_all(d.join("refs")).unwrap();
                    }
                    _ => {}
                }
            }
            let mut links = vec![];
            for i in 0..2 + next(7) {
                let at = dirs[next(dirs.len())].join(format!("l{i}"));
                let pool: Vec<&PathBuf> = dirs.iter().chain(&links).collect();
                let target = pool[next(pool.len())].clone();
                if symlink(&target, &at).is_ok() {
                    links.push(at);
                }
            }
            let any = |next: &mut dyn FnMut(usize) -> usize| -> PathBuf {
                let pool: Vec<&PathBuf> = dirs.iter().chain(&links).collect();
                pool[next(pool.len())].clone()
            };
            let roots: Vec<PathBuf> = (0..1 + next(3)).map(|_| any(&mut next)).collect();
            let mut skip = vec![];
            for _ in 0..next(4) {
                let p = any(&mut next).display().to_string();
                skip.push(match next(8) {
                    0 => names[next(names.len())].to_string(),
                    1 => "l1".to_string(),
                    2 => p,
                    3 => format!("{p}/a"),
                    4 => format!("{}/*/{}", base.display(), names[next(names.len())]),
                    5 => format!("{}/**/{}", base.display(), names[next(names.len())]),
                    6 => "go/pkg".to_string(),
                    _ => "*/c".to_string(),
                });
            }
            let opts = DiscoverOptions {
                skip: Skip::new(&skip, &roots).unwrap(),
                max_depth: [0, 1, 2, 3, 4, 6][next(6)],
                scan_inside_repos: next(5) == 0,
                skip_hidden: next(4) != 0,
                follow_symlinks: true,
                one_file_system: false,
                tuning: Tuning::default(),
            };
            let ctx = format!(
                "seed {seed}: roots {:?} skip {skip:?} max_depth {} inside {} hidden {}\nlinks {:?}",
                rel(&base, roots.clone()),
                opts.max_depth,
                opts.scan_inside_repos,
                opts.skip_hidden,
                links
                    .iter()
                    .map(|l| format!(
                        "{} -> {}",
                        l.strip_prefix(&base).unwrap().display(),
                        fs::read_link(l)
                            .unwrap()
                            .strip_prefix(&base)
                            .unwrap_or(Path::new("?"))
                            .display()
                    ))
                    .collect::<Vec<_>>()
            );
            let run = |o: &DiscoverOptions| discover_all(&roots, o);
            let plain = run(&with(&opts, false, false));
            // real paths from links: identical
            assert_eq!(run(&with(&opts, true, false)), plain, "{ctx}");
            let pruned = run(&with(&opts, false, true));
            assert_eq!(run(&with(&opts, true, true)), pruned, "{ctx}");
            pruned_somewhere |= pruned != plain;
            // pruning: the same repos, by real path and kind ...
            let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.into());
            let set = |d: &Discovery| -> std::collections::BTreeSet<(PathBuf, RepoKind)> {
                d.found.iter().map(|f| (real(&f.path), f.kind)).collect()
            };
            assert_eq!(set(&pruned), set(&plain), "{ctx}");
            // ... spelled by the real path wherever the plain walk did ...
            for f in plain.found.iter().filter(|f| real(&f.path) == f.path) {
                assert!(pruned.found.contains(f), "{ctx}\n{f:?}");
            }
            // ... and truncation reports lose only spellings through links
            for t in &pruned.truncated {
                assert!(plain.truncated.contains(t), "{ctx}\nnew truncated {t:?}");
            }
            for t in plain
                .truncated
                .iter()
                .filter(|t| !pruned.truncated.contains(t))
            {
                assert_ne!(&real(t), t, "{ctx}\ndropped a real truncated {t:?}");
            }
            // without -L, the switches change nothing
            let opts = DiscoverOptions {
                follow_symlinks: false,
                ..opts
            };
            let plain = run(&with(&opts, false, false));
            assert_eq!(run(&with(&opts, true, true)), plain, "{ctx}");
        }
        assert!(pruned_somewhere, "no tree exercised pruning");
    }

    #[cfg(unix)]
    #[test]
    fn covered_links_are_pruned() {
        let t = tempfile::tempdir().unwrap();
        let b = fs::canonicalize(t.path()).unwrap();
        mk(&b, "real/a/b/c");
        std::os::unix::fs::symlink(b.join("real"), b.join("alias")).unwrap();
        let opts = DiscoverOptions {
            max_depth: 2,
            follow_symlinks: true,
            ..Default::default()
        };
        let roots = [b.clone()];
        // the plain walk reports real/a, and alias/a through the link
        let plain = discover_all(&roots, &with(&opts, false, false));
        assert_eq!(rel(&b, plain.truncated), ["alias/a", "real/a"]);
        // pruned: the link isn't walked, as real/ is
        let pruned = discover_all(&roots, &opts);
        assert_eq!(rel(&b, pruned.truncated), ["real/a"]);
        // still when the root is a repo the walk continues into
        fs::create_dir_all(b.join(".git")).unwrap();
        let inside = DiscoverOptions {
            scan_inside_repos: true,
            ..opts
        };
        assert_eq!(rel(&b, discover_all(&roots, &inside).truncated), ["real/a"]);
    }

    /// Each case trips one condition of `plain_walk_covers`; links sit deep
    /// enough that the depth comparison holds, unless that's the case.
    #[cfg(unix)]
    #[test]
    fn links_are_followed_where_the_plain_walk_falls_short() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = tempfile::tempdir().unwrap();
        let b = fs::canonicalize(t.path()).unwrap();
        let link = |target: &str, at: &str| {
            mk(&b, Path::new(at).parent().unwrap().to_str().unwrap());
            symlink(b.join(target), b.join(at)).unwrap();
        };
        let found = |opts: &DiscoverOptions, roots: &[&str]| {
            let roots: Vec<PathBuf> = roots.iter().map(|r| b.join(r)).collect();
            let mut v = rel(&b, discover(&roots, opts).into_iter().map(|f| f.path));
            v.sort();
            v
        };
        let both = |opts: &DiscoverOptions, roots: &[&str], want: &str| {
            for o in [opts.clone(), with(opts, false, false)] {
                let v = found(&o, roots);
                assert!(v.contains(&want.to_string()), "{want}: {v:?}");
            }
        };
        let no_limit = DiscoverOptions {
            max_depth: 0,
            follow_symlinks: true,
            ..Default::default()
        };
        // into a repo, which stops the plain walk
        fake_repo(&b, "r/sub/inner");
        fs::create_dir_all(b.join("r/.git")).unwrap();
        link("r/sub", "z/z/into-repo");
        both(&no_limit, &["."], "z/z/into-repo/inner");
        // past a directory skipped by name: skipped on one route, so on
        // every route (pruning or not)
        fake_repo(&b, "vendor/p/v");
        link("vendor/p", "z/z/past-skip");
        let skip_vendor = DiscoverOptions {
            skip: Skip::new(&["vendor".into()], &[]).unwrap(),
            ..no_limit.clone()
        };
        for o in [skip_vendor.clone(), with(&skip_vendor, false, false)] {
            let v = found(&o, &["."]);
            assert!(!v.iter().any(|p| p.ends_with("/v")), "{v:?}");
        }
        // into .git, which the walk never enters, even inside repos and
        // with hidden directories scanned
        fs::create_dir_all(b.join("g/.git/modules/m/objects")).unwrap();
        fs::create_dir_all(b.join("g/.git/modules/m/refs")).unwrap();
        fs::write(b.join("g/.git/modules/m/HEAD"), "ref: refs/heads/main\n").unwrap();
        link("g/.git/modules", "z/z/into-git");
        let inside = DiscoverOptions {
            scan_inside_repos: true,
            skip_hidden: false,
            ..no_limit.clone()
        };
        both(&inside, &["."], "z/z/into-git/m");
        // deeper than max_depth for the plain walk, not through the link
        fake_repo(&b, "deep/a/b/c/far");
        link("deep/a/b", "shortcut");
        let limit = DiscoverOptions {
            max_depth: 3,
            ..no_limit.clone()
        };
        both(&limit, &["."], "shortcut/c/far");
        // past a directory that can't be listed (unless running as root)
        fake_repo(&b, "locked/p/q");
        link("locked/p", "z/z/past-lock");
        fs::set_permissions(b.join("locked"), fs::Permissions::from_mode(0o311)).unwrap();
        if fs::read_dir(b.join("locked")).is_err() {
            both(&no_limit, &["."], "z/z/past-lock/q");
        }
        fs::set_permissions(b.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A path pattern matching only a partly resolved spelling (x/b/c,
    /// through `dev/a -> x` then `x/b -> y`) skips c on that route, so the
    /// second link can't be pruned though y is a root: the repo in y/c goes.
    #[cfg(unix)]
    #[test]
    fn links_with_a_spelling_a_path_pattern_may_match_are_followed() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let b = fs::canonicalize(t.path()).unwrap();
        fake_repo(&b, "y/c/r");
        mk(&b, "dev");
        mk(&b, "x");
        symlink(b.join("y"), b.join("x/b")).unwrap();
        symlink(b.join("x"), b.join("dev/a")).unwrap();
        let roots = [b.join("dev"), b.join("y")];
        let opts = DiscoverOptions {
            skip: Skip::new(&[format!("{}/x/*/c", b.display())], &roots).unwrap(),
            follow_symlinks: true,
            ..Default::default()
        };
        for o in [opts.clone(), with(&opts, false, false)] {
            assert!(discover(&roots, &o).is_empty());
        }
    }

    /// A pattern compiled before its link existed keeps the link's path, so
    /// its literal part is at or below the link: following the link must
    /// not be pruned.
    #[cfg(unix)]
    #[test]
    fn links_at_or_above_an_unresolved_path_pattern_are_followed() {
        for pattern in ["dev/alias/vendor", "dev/alias/*"] {
            let t = tempfile::tempdir().unwrap();
            let b = fs::canonicalize(t.path()).unwrap();
            fake_repo(&b, "data/proj/vendor/lib");
            mk(&b, "dev");
            let roots = [b.join("dev"), b.join("data")];
            let opts = DiscoverOptions {
                skip: Skip::new(&[format!("{}/{pattern}", b.display())], &roots).unwrap(),
                follow_symlinks: true,
                ..Default::default()
            };
            std::os::unix::fs::symlink(b.join("data/proj"), b.join("dev/alias")).unwrap();
            for o in [opts.clone(), with(&opts, false, false)] {
                let v = discover(&roots, &o);
                assert!(v.is_empty(), "{pattern}: {v:?}");
            }
        }
    }

    /// At max_depth a directory isn't entered: a covered link there still
    /// counts towards reporting it as truncated.
    #[cfg(unix)]
    #[test]
    fn links_at_max_depth_still_count_for_truncation() {
        let t = tempfile::tempdir().unwrap();
        let b = fs::canonicalize(t.path()).unwrap();
        mk(&b, "real");
        mk(&b, "d");
        std::os::unix::fs::symlink(b.join("real"), b.join("d/only-a-link")).unwrap();
        let opts = DiscoverOptions {
            max_depth: 1,
            follow_symlinks: true,
            ..Default::default()
        };
        for o in [opts.clone(), with(&opts, false, false)] {
            assert_eq!(
                rel(&b, discover_all(std::slice::from_ref(&b), &o).truncated),
                ["d"]
            );
        }
    }
}
