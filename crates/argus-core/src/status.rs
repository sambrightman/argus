//! Read-only repository status via gix (in-process, no `git` subprocesses).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gix::bstr::ByteSlice;
use gix::remote::Direction;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "name", rename_all = "kebab-case")]
pub enum Head {
    Branch(String),
    /// Short commit id.
    Detached(String),
    /// No commits yet on this branch.
    Unborn(String),
}

impl Head {
    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch(b) | Head::Unborn(b) => Some(b),
            Head::Detached(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracking {
    /// Short tracking ref name, e.g. `origin/main`.
    pub name: String,
    pub ahead: usize,
    pub behind: usize,
    /// Upstream is configured but the tracking ref no longer exists.
    pub gone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub upstream: Option<Tracking>,
    pub is_head: bool,
    /// Tip is reachable from the default branch (safe to leave/delete).
    pub merged: bool,
    /// Commits not reachable from any remote-tracking ref; `None` when the
    /// repo has no remote-tracking refs at all.
    pub unpushed: Option<usize>,
}

impl BranchInfo {
    /// Has commits that exist on no remote we know of (and aren't merged).
    pub fn is_unpushed(&self) -> bool {
        if self.merged {
            return false;
        }
        match self.unpushed {
            Some(n) => n > 0,
            None => true,
        }
    }

    /// The upstream is the same-named branch on some remote, so a plain
    /// `git push` goes where the user expects.
    pub fn upstream_is_same_name(&self) -> bool {
        self.upstream
            .as_ref()
            .is_some_and(|t| t.name.ends_with(&format!("/{}", self.name)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    /// `git status --short` style codes: index (X) and worktree (Y).
    pub index: char,
    pub worktree: char,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Up to [`MAX_FILES`] entries, sorted by path.
    pub files: Vec<FileChange>,
}

pub const MAX_FILES: usize = 500;

impl Changes {
    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }
    /// Distinct changed paths.
    pub fn total(&self) -> usize {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InProgress {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Am,
}

impl InProgress {
    pub fn label(self) -> &'static str {
        match self {
            InProgress::Merge => "merge",
            InProgress::Rebase => "rebase",
            InProgress::CherryPick => "cherry-pick",
            InProgress::Revert => "revert",
            InProgress::Bisect => "bisect",
            InProgress::Am => "am",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    pub head: Head,
    /// Short id of the commit HEAD points at (none when unborn).
    pub head_commit: Option<String>,
    /// Tags on that commit: annotated first, then newest version first.
    pub tags: Vec<String>,
    pub default_branch: Option<String>,
    /// Remote used for fetching, usually `origin`.
    pub remote: Option<String>,
    pub remote_url: Option<String>,
    pub upstream: Option<Tracking>,
    pub changes: Changes,
    pub stashes: usize,
    /// Detached HEAD only: commits reachable from HEAD but from no branch,
    /// tag or remote ref (lost to gc once HEAD moves).
    pub detached_orphans: usize,
    pub in_progress: Option<InProgress>,
    pub branches: Vec<BranchInfo>,
    /// Linked worktrees (excluding the main one).
    pub worktrees: Vec<PathBuf>,
    /// Checked-out submodule work trees.
    pub submodules: Vec<PathBuf>,
    pub last_fetch: Option<Timestamp>,
    pub last_commit: Option<Timestamp>,
    pub bare: bool,
    /// A linked worktree: shares branches with its main checkout.
    pub linked_worktree: bool,
    /// A submodule: a detached HEAD at the parent's pinned commit is normal.
    pub submodule: bool,
    /// Whether `git pull` could integrate the upstream; only computed when
    /// the current branch has diverged from it (ahead *and* behind).
    pub integration: Option<Integration>,
}

/// Predicted outcome of pulling a diverged branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "detail", rename_all = "kebab-case")]
pub enum Integration {
    /// Merging the upstream applies without conflicts.
    Clean,
    /// Paths that would conflict.
    Conflicts(Vec<String>),
    /// `git pull` would refuse, e.g. no `pull.rebase`/`pull.ff` setting.
    Refused(String),
    /// Prediction failed (e.g. git older than 2.38).
    Unknown(String),
}

impl RepoStatus {
    pub fn on_default(&self) -> bool {
        match (&self.head, &self.default_branch) {
            (Head::Branch(b) | Head::Unborn(b), Some(d)) => b == d,
            _ => false,
        }
    }

    pub fn ahead(&self) -> usize {
        self.upstream.as_ref().map_or(0, |t| t.ahead)
    }

    pub fn behind(&self) -> usize {
        self.upstream.as_ref().map_or(0, |t| t.behind)
    }

    /// Local branches with commits not on any remote or the default branch.
    pub fn unpushed_branches(&self) -> impl Iterator<Item = &BranchInfo> {
        self.branches.iter().filter(|b| b.is_unpushed())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StatusError(pub String);

fn err(e: impl std::fmt::Display) -> StatusError {
    StatusError(e.to_string())
}

/// Options influencing how much work status does.
#[derive(Debug, Clone, Copy)]
pub struct StatusOptions {
    /// Compute dirty state of submodules (slower).
    pub submodules: bool,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self { submodules: true }
    }
}

pub fn status(path: &Path, opts: StatusOptions) -> Result<RepoStatus, StatusError> {
    let repo = crate::isolation::open(path).map_err(err)?;
    status_of(&repo, opts)
}

pub fn status_of(repo: &gix::Repository, opts: StatusOptions) -> Result<RepoStatus, StatusError> {
    let head = read_head(repo)?;
    let remote = repo
        .remote_default_name(Direction::Fetch)
        .map(|n| n.to_str_lossy().into_owned())
        .or_else(|| {
            repo.remote_names()
                .into_iter()
                .next()
                .map(|n| n.to_str_lossy().into_owned())
        });
    let remote_url = remote.as_deref().and_then(|r| {
        // The URL as configured, before `insteadOf` rewrites: that is the identity.
        let r = repo.try_find_remote_without_url_rewrite(r)?.ok()?;
        r.url(Direction::Fetch)
            .map(|u| u.to_bstring().to_str_lossy().into_owned())
    });
    let default_branch = default_branch(repo, remote.as_deref());
    let default_tip = default_branch
        .as_deref()
        .and_then(|d| default_tip(repo, remote.as_deref(), d));

    let branches = branches(repo, head.branch(), default_tip)?;
    let upstream = head
        .branch()
        .and_then(|b| branches.iter().find(|bi| bi.name == b))
        .and_then(|bi| bi.upstream.clone());

    let bare = repo.workdir().is_none();
    let changes = if bare {
        Changes::default()
    } else {
        changes(repo, opts)?
    };
    let integration = integration(repo, &head, upstream.as_ref());
    let detached_orphans = match &head {
        Head::Detached(_) if repo.kind() != gix::repository::Kind::Submodule => {
            detached_orphans(repo)?
        }
        _ => 0,
    };

    Ok(RepoStatus {
        head,
        head_commit: repo.head_id().ok().map(|id| id.shorten_or_id().to_string()),
        tags: head_tags(repo),
        default_branch,
        remote,
        remote_url,
        upstream,
        changes,
        stashes: stash_count(repo.common_dir()),
        detached_orphans,
        in_progress: repo.state().map(map_state),
        branches,
        worktrees: worktrees(repo),
        submodules: submodules(repo),
        last_fetch: mtime(&repo.git_dir().join("FETCH_HEAD"))
            .or_else(|| mtime(&repo.common_dir().join("FETCH_HEAD"))),
        last_commit: repo
            .head_commit()
            .ok()
            .and_then(|c| c.time().ok())
            .and_then(|t| Timestamp::from_second(t.seconds).ok()),
        bare,
        linked_worktree: repo.kind() == gix::repository::Kind::LinkedWorkTree,
        submodule: repo.kind() == gix::repository::Kind::Submodule,
        integration,
    })
}

/// Porcelain: `git merge-tree --write-tree` (git 2.38+) is a read-only trial
/// merge that matches what `git pull` will do; it's only run for diverged
/// branches, which are rare, so the process cost doesn't matter. A clean
/// merge doesn't guarantee a clean rebase (commits replay one by one), but
/// it's a good predictor, and a pull that does conflict is aborted anyway.
fn integration(
    repo: &gix::Repository,
    head: &Head,
    upstream: Option<&Tracking>,
) -> Option<Integration> {
    let branch = match head {
        Head::Branch(b) => b,
        _ => return None,
    };
    let t = upstream?;
    if t.gone || t.ahead == 0 || t.behind == 0 {
        return None;
    }
    let dir = repo.workdir()?;
    if let Some(why) = pull_refusal(repo, branch) {
        return Some(Integration::Refused(why));
    }
    let o = crate::ops::git_in(
        dir,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "HEAD",
            "@{upstream}",
        ],
    );
    Some(match o.code {
        Some(0) => Integration::Clean,
        Some(1) => Integration::Conflicts(
            o.output
                .lines()
                .skip(1) // the tree id
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
        ),
        _ => Integration::Unknown(o.output.lines().next().unwrap_or("git failed").to_string()),
    })
}

/// Why `git pull` would refuse to reconcile a diverged branch, if it would:
/// without `branch.<b>.rebase`, `pull.rebase` or `pull.ff` it asks how to,
/// and `pull.ff = only` refuses outright. (Conservative: `only` is treated as
/// refusing even alongside a rebase setting.)
fn pull_refusal(repo: &gix::Repository, branch: &str) -> Option<String> {
    let cfg = repo.config_snapshot();
    let ff = cfg
        .string("pull.ff")
        .map(|v| v.to_str_lossy().to_lowercase());
    if ff.as_deref() == Some("only") {
        return Some("pull.ff = only refuses diverged branches".into());
    }
    let rebase = cfg
        .string(format!("branch.{branch}.rebase").as_str())
        .is_some()
        || cfg.string("pull.rebase").is_some();
    if !rebase && ff.is_none() {
        return Some("git pull would refuse: set pull.rebase (or pull.ff) to choose".into());
    }
    None
}

fn read_head(repo: &gix::Repository) -> Result<Head, StatusError> {
    let head = repo.head().map_err(err)?;
    Ok(match &head.kind {
        gix::head::Kind::Symbolic(r) => Head::Branch(r.name.shorten().to_str_lossy().into_owned()),
        gix::head::Kind::Unborn(name) => Head::Unborn(name.shorten().to_str_lossy().into_owned()),
        gix::head::Kind::Detached { .. } => Head::Detached(
            head.id()
                .map(|id| id.shorten_or_id().to_string())
                .unwrap_or_default(),
        ),
    })
}

/// The remote's HEAD if known (`refs/remotes/origin/HEAD`), otherwise a
/// local `main`/`master`, otherwise `init.defaultBranch`.
fn default_branch(repo: &gix::Repository, remote: Option<&str>) -> Option<String> {
    if let Some(remote) = remote
        && let Ok(Some(r)) = repo.try_find_reference(format!("refs/remotes/{remote}/HEAD").as_str())
        && let gix::refs::TargetRef::Symbolic(target) = r.target()
    {
        let prefix = format!("refs/remotes/{remote}/");
        if let Some(b) = target.as_bstr().to_str_lossy().strip_prefix(&prefix) {
            return Some(b.to_string());
        }
    }
    for candidate in ["main", "master"] {
        let local = format!("refs/heads/{candidate}");
        let tracked = remote.map(|r| format!("refs/remotes/{r}/{candidate}"));
        if exists(repo, &local) || tracked.is_some_and(|t| exists(repo, &t)) {
            return Some(candidate.to_string());
        }
    }
    repo.config_snapshot()
        .string("init.defaultBranch")
        .map(|s| s.to_str_lossy().into_owned())
}

fn exists(repo: &gix::Repository, name: &str) -> bool {
    matches!(repo.try_find_reference(name), Ok(Some(_)))
}

/// Commit the default branch points to: remote-tracking ref preferred, as it
/// is what "pushed/merged" means; local branch as a fallback.
fn default_tip(
    repo: &gix::Repository,
    remote: Option<&str>,
    default: &str,
) -> Option<gix::ObjectId> {
    let mut names = vec![];
    if let Some(r) = remote {
        names.push(format!("refs/remotes/{r}/{default}"));
    }
    names.push(format!("refs/heads/{default}"));
    names.iter().find_map(|n| peel(repo, n))
}

fn peel(repo: &gix::Repository, name: &str) -> Option<gix::ObjectId> {
    let mut r = repo.try_find_reference(name).ok()??;
    r.peel_to_id().ok().map(|id| id.detach())
}

/// Number of commits reachable from `a` but not from `b`.
fn count_exclusive(
    repo: &gix::Repository,
    a: gix::ObjectId,
    b: gix::ObjectId,
) -> Result<usize, StatusError> {
    if a == b {
        return Ok(0);
    }
    let walk = repo.rev_walk([a]).with_hidden([b]).all().map_err(err)?;
    let mut n = 0;
    for info in walk {
        info.map_err(err)?;
        n += 1;
    }
    Ok(n)
}

fn branches(
    repo: &gix::Repository,
    head_branch: Option<&str>,
    default_tip: Option<gix::ObjectId>,
) -> Result<Vec<BranchInfo>, StatusError> {
    let platform = repo.references().map_err(err)?;
    let mut remote_tips: Vec<gix::ObjectId> = vec![];
    for r in platform.remote_branches().map_err(err)? {
        let mut r = r.map_err(err)?;
        if let gix::refs::TargetRef::Symbolic(_) = r.target() {
            continue; // origin/HEAD
        }
        if let Ok(id) = r.peel_to_id() {
            remote_tips.push(id.detach());
        }
    }
    remote_tips.sort();
    remote_tips.dedup();
    let mut out = vec![];
    for r in platform.local_branches().map_err(err)? {
        let mut r = r.map_err(err)?;
        let full = r.name().to_owned();
        let name = full.shorten().to_str_lossy().into_owned();
        let Ok(tip) = r.peel_to_id() else { continue };
        let tip = tip.detach();
        let upstream = match repo.branch_remote_tracking_ref_name(full.as_ref(), Direction::Fetch) {
            Some(Ok(tracking)) => {
                let short = tracking.shorten().to_str_lossy().into_owned();
                match peel(repo, &tracking.as_bstr().to_str_lossy()) {
                    Some(up) => Some(Tracking {
                        name: short,
                        ahead: count_exclusive(repo, tip, up)?,
                        behind: count_exclusive(repo, up, tip)?,
                        gone: false,
                    }),
                    None => Some(Tracking {
                        name: short,
                        ahead: 0,
                        behind: 0,
                        gone: true,
                    }),
                }
            }
            _ => None,
        };
        let merged = match default_tip {
            Some(d) => count_exclusive(repo, tip, d)? == 0,
            None => false,
        };
        let unpushed = if remote_tips.is_empty() {
            None
        } else if remote_tips.binary_search(&tip).is_ok() {
            Some(0)
        } else {
            let walk = repo
                .rev_walk([tip])
                .with_hidden(remote_tips.iter().copied())
                .all()
                .map_err(err)?;
            let mut n = 0;
            for info in walk {
                info.map_err(err)?;
                n += 1;
            }
            Some(n)
        };
        out.push(BranchInfo {
            is_head: head_branch == Some(name.as_str()),
            name,
            upstream,
            merged,
            unpushed,
        });
    }
    out.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn changes(repo: &gix::Repository, opts: StatusOptions) -> Result<Changes, StatusError> {
    use gix::status::index_worktree::Item as IW;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let submodules = if opts.submodules {
        gix::status::Submodule::AsConfigured { check_dirty: true }
    } else {
        gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        }
    };
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(err)?
        // Per-file, then collapsed below like git does. gix's own collapsing
        // reports directories holding only ignored files/empty dirs as untracked.
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(submodules)
        .into_iter(None)
        .map_err(err)?;

    // path -> (index code, worktree code)
    let mut files: BTreeMap<String, (char, char)> = BTreeMap::new();
    for item in iter {
        let item = item.map_err(err)?;
        match item {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef as C;
                let code = match change {
                    C::Addition { .. } => 'A',
                    C::Deletion { .. } => 'D',
                    C::Modification { .. } => 'M',
                    C::Rewrite { copy: true, .. } => 'C',
                    C::Rewrite { .. } => 'R',
                };
                files
                    .entry(change.location().to_str_lossy().into_owned())
                    .or_insert((' ', ' '))
                    .0 = code;
            }
            gix::status::Item::IndexWorktree(iw) => {
                let mut path = iw.rela_path().to_str_lossy().into_owned();
                if let IW::DirectoryContents { entry, .. } = &iw {
                    // like `git status`, mark collapsed directories with a slash
                    if entry.disk_kind == Some(gix::dir::entry::Kind::Directory)
                        && !path.ends_with('/')
                    {
                        path.push('/');
                    }
                }
                let code = match &iw {
                    IW::Modification { status, .. } => match status {
                        EntryStatus::Conflict { .. } => 'U',
                        EntryStatus::Change(Change::Removed) => 'D',
                        EntryStatus::Change(Change::Type { .. }) => 'T',
                        EntryStatus::Change(_) => 'M',
                        EntryStatus::IntentToAdd => 'A',
                        EntryStatus::NeedsUpdate(_) => continue,
                    },
                    IW::DirectoryContents { entry, .. } => {
                        if entry.status != gix::dir::entry::Status::Untracked {
                            continue;
                        }
                        '?'
                    }
                    IW::Rewrite { .. } => 'R',
                };
                let e = files.entry(path).or_insert((' ', ' '));
                if code == '?' {
                    *e = ('?', '?');
                } else if code == 'U' {
                    *e = ('U', 'U');
                } else {
                    e.1 = code;
                }
            }
        }
    }

    collapse_untracked(repo, &mut files)?;
    let mut c = Changes::default();
    for (path, (x, y)) in files {
        match (x, y) {
            ('?', _) => c.untracked += 1,
            ('U', _) => c.conflicted += 1,
            _ => {
                if x != ' ' {
                    c.staged += 1;
                }
                if y != ' ' {
                    c.unstaged += 1;
                }
            }
        }
        if c.files.len() < MAX_FILES {
            c.files.push(FileChange {
                path,
                index: x,
                worktree: y,
            });
        }
    }
    Ok(c)
}

/// Replace untracked files inside directories with no tracked content by the
/// outermost such directory (`dir/`), matching `git status` "normal" mode.
fn collapse_untracked(
    repo: &gix::Repository,
    files: &mut BTreeMap<String, (char, char)>,
) -> Result<(), StatusError> {
    if !files.values().any(|c| c.0 == '?') {
        return Ok(());
    }
    let index = repo.index_or_empty().map_err(err)?;
    let mut tracked_dirs: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
    for e in index.entries() {
        let p: &[u8] = e.path(&index).as_ref();
        let mut i = 0;
        while let Some(off) = p[i..].iter().position(|&b| b == b'/') {
            i += off;
            tracked_dirs.insert(&p[..i]);
            i += 1;
        }
    }
    let untracked: Vec<String> = files
        .iter()
        .filter(|(_, c)| c.0 == '?')
        .map(|(p, _)| p.clone())
        .collect();
    for path in untracked {
        let bytes = path.trim_end_matches('/').as_bytes();
        let mut collapsed = None;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'/' && !tracked_dirs.contains(&bytes[..i]) {
                collapsed = Some(format!("{}/", &path[..i]));
                break;
            }
        }
        if let Some(dir) = collapsed {
            files.remove(&path);
            files.insert(dir, ('?', '?'));
        }
    }
    Ok(())
}

/// Tags pointing at HEAD's commit. Packed refs record where annotated tags
/// lead, so only loose annotated tags cost an object read.
fn head_tags(repo: &gix::Repository) -> Vec<String> {
    let Ok(head) = repo.head_id() else {
        return vec![];
    };
    let head = head.detach();
    let Ok(platform) = repo.references() else {
        return vec![];
    };
    let Ok(refs) = platform.prefixed("refs/tags/") else {
        return vec![];
    };
    let mut found: Vec<(bool, String)> = vec![];
    for mut r in refs.flatten() {
        let gix::refs::Target::Object(target) = r.inner.target.clone() else {
            continue;
        };
        let peeled = match r.inner.peeled {
            Some(p) => p,
            None if target == head => head,
            None => match repo.find_header(target) {
                Ok(h) if h.kind() == gix::object::Kind::Tag => match r.peel_to_id() {
                    Ok(id) => id.detach(),
                    Err(_) => continue,
                },
                _ => continue,
            },
        };
        if peeled == head {
            let annotated = target != head;
            found.push((annotated, r.name().shorten().to_str_lossy().into_owned()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| version_cmp(&b.1, &a.1)));
    found.into_iter().map(|(_, name)| name).collect()
}

/// Compare names with digit runs as numbers, so `v1.10` > `v1.9`.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut v = vec![];
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                v.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        v
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let o = match (x, y) {
            ((true, x), (true, y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if o.is_ne() {
            return o;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
}

fn detached_orphans(repo: &gix::Repository) -> Result<usize, StatusError> {
    let Ok(head) = repo.head_id() else {
        return Ok(0);
    };
    let platform = repo.references().map_err(err)?;
    let mut tips = vec![];
    // not `all()`: stash commits have HEAD as a parent and would hide everything
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        for r in platform.prefixed(prefix).map_err(err)? {
            let mut r = r.map_err(err)?;
            if let Ok(id) = r.peel_to_id() {
                tips.push(id.detach());
            }
        }
    }
    if tips.contains(&head.detach()) {
        return Ok(0); // e.g. a checked-out tag: no walk needed
    }
    let walk = repo
        .rev_walk([head.detach()])
        .with_hidden(tips)
        .all()
        .map_err(err)?;
    let mut n = 0;
    for info in walk {
        info.map_err(err)?;
        n += 1;
    }
    Ok(n)
}

fn stash_count(common_dir: &Path) -> usize {
    std::fs::read(common_dir.join("logs/refs/stash"))
        .map(|b| b.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

pub(crate) fn map_state(s: gix::state::InProgress) -> InProgress {
    use gix::state::InProgress as S;
    match s {
        S::Merge => InProgress::Merge,
        S::Rebase | S::RebaseInteractive | S::ApplyMailboxRebase => InProgress::Rebase,
        S::CherryPick | S::CherryPickSequence => InProgress::CherryPick,
        S::Revert | S::RevertSequence => InProgress::Revert,
        S::Bisect => InProgress::Bisect,
        S::ApplyMailbox => InProgress::Am,
    }
}

fn worktrees(repo: &gix::Repository) -> Vec<PathBuf> {
    // A linked worktree sees all of its siblings (and itself); only the main
    // work tree reports them, so that each is listed exactly once.
    if repo.kind() == gix::repository::Kind::LinkedWorkTree {
        return vec![];
    }
    let Ok(wts) = repo.worktrees() else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = wts.iter().filter_map(|p| p.base().ok()).collect();
    v.sort();
    v
}

fn submodules(repo: &gix::Repository) -> Vec<PathBuf> {
    let Ok(Some(sms)) = repo.submodules() else {
        return vec![];
    };
    let mut v: Vec<PathBuf> = sms
        .filter(|sm| sm.state().is_ok_and(|s| s.worktree_checkout))
        .filter_map(|sm| sm.work_dir().ok())
        .collect();
    v.sort();
    v
}

fn mtime(p: &Path) -> Option<Timestamp> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Timestamp::from_second(d.as_secs() as i64).ok()
}
