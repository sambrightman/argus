//! Read-only repository status via gix (in-process, no `git` subprocesses).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
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
    /// The commit it's at.
    #[serde(default)]
    pub tip: String,
    /// Tip is reachable from the default branch (safe to leave/delete).
    pub merged: bool,
    /// Not merged, but merging it into the default branch would change
    /// nothing: its changes are there already, squash-merged or rebased
    /// (its commits aren't). Not when the default branch has since changed
    /// the same lines, or reverted them: then it can't be told, or isn't so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absorbed: bool,
    /// Commits not reachable from any remote-tracking ref; `None` when the
    /// repo has no remote-tracking refs at all. 0 too for a branch
    /// absorbed into a remote's default branch: its changes are there.
    pub unpushed: Option<usize>,
    /// When `git push` would push to a branch other than the upstream, and
    /// that's the same-named branch (say `feat` pulls from `origin/main`,
    /// and `push.default` is `current`): that branch, which argus pushes
    /// to. On the push remote (`branch.<name>.pushRemote`,
    /// `remote.pushDefault`, else the upstream's). `gone` means it doesn't
    /// exist there yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own: Option<Tracking>,
    /// Not checked yet ([`StatusOptions::branches_first`]): only its name,
    /// tip and whether it's HEAD's are known, and nothing is said of it.
    #[serde(skip)]
    pub unchecked: bool,
}

impl BranchInfo {
    /// Has commits that exist on no remote we know of. (Being merged into
    /// the default branch doesn't count: that may be only the local one.)
    /// Not one unchecked: that's not known yet.
    pub fn is_unpushed(&self) -> bool {
        !self.unchecked && self.unpushed.is_none_or(|n| n > 0)
    }

    /// Its changes are in the default branch: merged, or absorbed.
    pub fn landed(&self) -> bool {
        self.merged || self.absorbed
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
    #[serde(default)]
    pub kind: PathKind,
}

impl FileChange {
    /// Whether there's an untracked file at the path: one git has never
    /// had, or one staged for deletion and still there (`D` with `?`).
    /// A submodule's `?` is untracked content inside it, not this.
    pub fn untracked(&self) -> bool {
        self.index == '?' || (self.worktree == '?' && self.kind != PathKind::Submodule)
    }
}

/// What a changed path is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathKind {
    #[default]
    File,
    /// A submodule: `M` when it's at another commit than recorded, else
    /// (work tree only, as `git status --short`) `m` for modified content,
    /// `?` for untracked content.
    Submodule,
    /// A repo nested in the work tree, untracked; or an untracked directory
    /// (`dir/`) holding nothing else. No stash or commit takes them.
    Repo,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Of `staged` and `unstaged`, submodules at another commit than
    /// recorded: a commit records them, no stash takes them.
    pub staged_submodules: usize,
    pub unstaged_submodules: usize,
    /// Submodules whose only changes are inside them: theirs to fix (on
    /// their own rows), not counted above.
    pub submodule_content: usize,
    /// Of `untracked`, nested repos (see [`PathKind::Repo`]).
    pub untracked_repos: usize,
    /// Submodules at another commit than recorded, by path.
    #[serde(skip)]
    pub(crate) moved: Vec<String>,
    /// Submodules whose folder is gone, by path.
    #[serde(skip)]
    pub(crate) missing: Vec<String>,
    /// Up to [`MAX_FILES`] entries, sorted by path.
    pub files: Vec<FileChange>,
    /// Files whose times the index has wrong, read and found unchanged:
    /// every status reads them again, until the index is refreshed
    /// ([`crate::ops::refresh_index`]).
    #[serde(skip)]
    pub stale: usize,
}

pub const MAX_FILES: usize = 500;

impl Changes {
    /// What a parent's status shows for a submodule with these changes
    /// inside, as far as `ignore` lets them show: `m`, tracked content
    /// changed (its own, or a submodule's of its); `?`, only untracked
    /// files; or nothing.
    pub fn as_submodule(&self, ignore: gix::submodule::config::Ignore) -> Option<char> {
        use gix::submodule::config::Ignore;
        let tracked = self.staged + self.unstaged + self.conflicted + self.submodule_content > 0;
        match ignore {
            Ignore::All | Ignore::Dirty => None,
            _ if tracked => Some('m'),
            Ignore::Untracked => None,
            Ignore::None => (self.untracked > 0).then_some('?'),
        }
    }

    /// Show the submodule at `rela` with `code` ([`Self::as_submodule`]),
    /// as a status looking inside it would have: beside a staged change,
    /// and not over one that says more (another commit, deleted).
    pub fn add_submodule_content(&mut self, rela: &str, code: char) {
        match self.files.binary_search_by(|f| f.path.as_str().cmp(rela)) {
            Ok(i) if self.files[i].worktree == ' ' => self.files[i].worktree = code,
            Ok(_) => {}
            Err(i) => {
                self.submodule_content += 1;
                if self.files.len() < MAX_FILES {
                    self.files.insert(
                        i,
                        FileChange {
                            path: rela.to_string(),
                            index: ' ',
                            worktree: code,
                            kind: PathKind::Submodule,
                        },
                    );
                }
            }
        }
    }

    /// The entry for `path`: its own, or for a file in an untracked
    /// directory listed collapsed (`build/`), that directory's.
    pub fn entry(&self, path: &str) -> Option<&FileChange> {
        self.files.iter().find(|c| c.path == path).or_else(|| {
            self.files
                .iter()
                .find(|c| c.index == '?' && c.path.ends_with('/') && path.starts_with(&c.path))
        })
    }

    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }
    /// Distinct changed paths.
    pub fn total(&self) -> usize {
        self.uncommitted() + self.conflicted + self.submodule_content
    }
    /// Paths a commit of everything would take.
    pub fn uncommitted(&self) -> usize {
        self.staged + self.unstaged + self.untracked - self.untracked_repos
    }
    /// Paths a stash would take, untracked ones with `untracked`.
    pub fn stashable(&self, untracked: bool) -> usize {
        self.staged - self.staged_submodules + self.unstaged - self.unstaged_submodules
            + if untracked {
                self.untracked - self.untracked_repos
            } else {
                0
            }
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

/// A submodule as its parent's status shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmoduleShown {
    /// Its work tree.
    pub path: PathBuf,
    /// Where it is in the parent's, as the parent's changes name it.
    pub rela: String,
    /// What of its changes the parent's status leaves out
    /// (`submodule.<name>.ignore`).
    pub ignore: gix::submodule::config::Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    pub head: Head,
    /// Submodules at another commit than the repo records (unstaged), and
    /// what putting them back would cost.
    #[serde(default)]
    pub moved_submodules: Vec<MovedSubmodule>,
    /// Id of the commit HEAD points at (none when unborn).
    pub head_commit: Option<String>,
    /// Tags on that commit: annotated first, then newest version first.
    pub tags: Vec<String>,
    /// For a detached HEAD on no tag: where it is relative to the nearest
    /// tag in its history, as `git describe --tags` says (`v1.2-3-g1a2b3c4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub describe: Option<String>,
    /// For such a HEAD: the walk for a tag stopped at the limit without
    /// finding one (there may be one further back: see [`describe_fully`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub describe_gave_up: bool,
    pub default_branch: Option<String>,
    /// Remote used for fetching, usually `origin`.
    pub remote: Option<String>,
    pub remote_url: Option<String>,
    /// The other remotes' URLs as configured, by remote name: where else
    /// this clone's branches may be pushed (a fork, say).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_remotes: Vec<String>,
    pub upstream: Option<Tracking>,
    pub changes: Changes,
    /// Each stash's message, newest (`stash@{0}`) first, without git's
    /// `On <branch>: ` or `WIP on <branch>: ` prefix.
    pub stashes: Vec<String>,
    /// Each stash's commit id, in the same order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stash_ids: Vec<String>,
    /// Detached HEAD only: commits reachable from HEAD but from no branch,
    /// tag or remote ref (lost to gc once HEAD moves).
    pub detached_orphans: usize,
    /// Detached HEAD only: local branches whose tip is the HEAD commit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at_head: Vec<String>,
    pub in_progress: Option<InProgress>,
    pub branches: Vec<BranchInfo>,
    /// Linked worktrees (excluding the main one).
    pub worktrees: Vec<PathBuf>,
    /// Records of linked worktrees whose folder is gone (and that aren't
    /// locked), by the folder's path: `git worktree prune` deletes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_worktrees: Vec<PathBuf>,
    /// Submodules whose folder is gone (not merely not checked out, which
    /// leaves it empty), by path: `git submodule update` puts them back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_submodules: Vec<String>,
    /// Checked-out submodule work trees.
    pub submodules: Vec<PathBuf>,
    /// With [`StatusOptions::submodule_content_from_own`]: each checked-out
    /// submodule, where it is and how its changes show.
    #[serde(skip)]
    pub submodules_shown: Vec<SubmoduleShown>,
    /// With [`StatusOptions::submodule_content_from_own`] and submodules:
    /// the changes as read, before any submodule's were added to them.
    #[serde(skip)]
    pub own_changes: Option<Changes>,
    pub last_fetch: Option<Timestamp>,
    pub last_commit: Option<Timestamp>,
    pub bare: bool,
    /// A linked worktree: shares branches with its main checkout.
    pub linked_worktree: bool,
    /// A linked worktree locked against pruning, moving and removal (`git
    /// worktree lock`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
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
    Clean(PullBy),
    /// Paths that would conflict.
    Conflicts(Vec<String>),
    /// `git pull` would refuse, e.g. no `pull.rebase`/`pull.ff` setting.
    Refused(String),
    /// Prediction failed (e.g. git older than 2.38).
    Unknown(String),
}

/// How `git pull` reconciles a diverged branch: a merge adds a merge
/// commit, a rebase replays the branch's commits on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullBy {
    Merge,
    Rebase,
}

impl RepoStatus {
    /// A linked worktree that's done with: nothing uncommitted, its branch
    /// (not the default) merged into the default branch and its upstream
    /// gone, as after a pull request. A branch never pushed isn't: it can't
    /// be told from one not started yet.
    pub fn is_finished_worktree(&self) -> bool {
        let Head::Branch(b) = &self.head else {
            return false;
        };
        self.linked_worktree
            && self.in_progress.is_none()
            && self.changes.uncommitted() == 0
            && self.changes.conflicted == 0
            && self.default_branch.as_ref().is_some_and(|d| d != b)
            && self.upstream.as_ref().is_some_and(|t| t.gone)
            && self.branches.iter().any(|i| &i.name == b && i.landed())
    }

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
    /// With `submodules`, leave what's changed inside each submodule for
    /// its own status to say (the workspace reads it anyway): the status
    /// shows only a submodule at another commit, and says where each one
    /// is and how its changes show ([`RepoStatus::submodules_shown`]), for
    /// [`Changes::add_submodule_content`].
    pub submodule_content_from_own: bool,
    /// How far back to look for a tag to describe a detached HEAD by (see
    /// `describe_limit` in the config).
    pub describe_limit: usize,
    /// Check only HEAD's branch, the default branch and this many others,
    /// those with the newest tips: the rest are listed
    /// [unchecked](BranchInfo::unchecked), for [`branches_of`] to check.
    /// `None` checks every branch.
    pub branches_first: Option<usize>,
}

impl Default for StatusOptions {
    fn default() -> Self {
        Self {
            submodules: true,
            submodule_content_from_own: false,
            describe_limit: crate::config::DESCRIBE_LIMIT,
            branches_first: None,
        }
    }
}

/// Errors leave out `path`: front-ends show it next to the error already.
pub fn status(path: &Path, opts: StatusOptions) -> Result<RepoStatus, StatusError> {
    traced(path, || {
        crate::trace::part("open", || crate::isolation::open(path))
            .map_err(err)
            .and_then(|repo| status_of(&repo, opts))
    })
    .map_err(|e| StatusError(without_path(&e.0, path)))
}

/// `read` as a step, its parts steps in it.
fn traced(
    path: &Path,
    read: impl FnOnce() -> Result<RepoStatus, StatusError>,
) -> Result<RepoStatus, StatusError> {
    let what = || format!("status {}", crate::paths::contract_tilde(path));
    crate::trace::step("status", what, || {
        let r = read();
        if r.is_err() {
            crate::trace::amend(|| format!("{}, failed", what()));
        }
        r
    })
}

/// Repos kept open between status reads (the TUI's refreshes). Opening is
/// mostly reading config files, so a repo is reused until one of the files
/// its config came from changes. Everything else (refs, index, packs) gix
/// re-reads as needed.
#[derive(Default)]
pub struct KeptRepos {
    kept: std::sync::Mutex<HashMap<PathBuf, Kept>>,
    opens: std::sync::atomic::AtomicUsize,
}

struct Kept {
    repo: gix::ThreadSafeRepository,
    stamps: Vec<(PathBuf, Option<Stamp>)>,
}

/// What changes when a file is rewritten (git writes a new file and renames
/// it over the old one, so the inode changes even within a second).
#[derive(PartialEq, Eq)]
struct Stamp {
    ino: u64,
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp(p: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).ok()?;
    Some(Stamp {
        ino: m.ino(),
        modified: m.modified().ok(),
        len: m.len(),
    })
}

/// What a kept repo is checked against before reuse: the files its config
/// was read from, and the places config could appear that didn't exist yet
/// (the standard ones, and the files its `include` sections name);
/// HEAD when a section depends on the branch (`includeIf "onbranch:…"`);
/// the index, which gix re-reads only when its time is newer (a same-time
/// rewrite on a file system with coarse times would be missed, but git
/// always replaces it, so the inode changes); and the pack directory, so
/// packs a fetch or gc replaced are let go of rather than kept mapped.
fn stamps(repo: &gix::Repository) -> Vec<(PathBuf, Option<Stamp>)> {
    let config = repo.config_snapshot();
    let config = config.plumbing();
    let mut files: Vec<PathBuf> = config
        .sections()
        .filter_map(|s| s.meta().path.clone())
        .collect();
    files.extend(config_candidates(&mut |k| std::env::var_os(k)));
    // files the config asks to include, there or not: one that appears
    // later is read by the next open
    for s in config.sections() {
        let name: &[u8] = s.header().name().as_ref();
        if name != b"include" && name != b"includeIf" {
            continue;
        }
        let from = s.meta().path.as_deref().and_then(Path::parent);
        for value in s.values("path") {
            let Ok(p) = gix::path::from_bstr(value) else {
                continue;
            };
            let p = crate::paths::expand_tilde(&p);
            files.extend(match (p.is_absolute(), from) {
                (true, _) => Some(p),
                (false, Some(dir)) => Some(dir.join(p)),
                (false, None) => None,
            });
        }
    }
    let on_branch = config.sections().any(|s| {
        s.header().name() == "includeIf"
            && s.header()
                .subsection_name()
                .is_some_and(|n| n.starts_with(b"onbranch:"))
    });
    if on_branch {
        files.push(repo.git_dir().join("HEAD"));
    }
    files.push(repo.index_path());
    files.push(repo.common_dir().join("objects/pack"));
    files.sort();
    files.dedup();
    files
        .into_iter()
        .map(|f| {
            let s = stamp(&f);
            (f, s)
        })
        .collect()
}

/// Where repository-independent config is read from, whether or not it
/// exists yet, given the environment `env`.
fn config_candidates(env: &mut dyn FnMut(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    use gix::config::Source;
    [
        Source::GitInstallation,
        Source::System,
        Source::Git,
        Source::User,
    ]
    .into_iter()
    .filter_map(|s| match (s, crate::installation::file()) {
        // as repos are opened: without waiting for git if it's known
        (Source::GitInstallation, Some(known)) => known,
        _ => s.storage_location(env),
    })
    .collect()
}

impl KeptRepos {
    pub fn status(&self, path: &Path, opts: StatusOptions) -> Result<RepoStatus, StatusError> {
        traced(path, || {
            crate::trace::part("open", || self.open(path))
                .map_err(err)
                .and_then(|repo| status_of(&repo, opts))
        })
        .map_err(|e| StatusError(without_path(&e.0, path)))
    }

    /// [`branches_of`], through the kept repo.
    pub fn branches(&self, path: &Path) -> Result<Vec<BranchInfo>, StatusError> {
        traced_branches(path, || {
            self.open(path)
                .map_err(err)
                .and_then(|repo| all_branches(&repo))
        })
    }

    /// Close every kept repo: the next status of each opens it afresh.
    pub fn forget(&self) {
        self.kept.lock().unwrap().clear();
    }

    fn open(&self, path: &Path) -> gix::Result<gix::Repository> {
        let kept = self.kept.lock().unwrap().get(path).map(|k| {
            let current = k.stamps.iter().all(|(f, s)| stamp(f) == *s);
            (current, k.repo.to_thread_local())
        });
        if let Some((true, repo)) = kept {
            return Ok(repo);
        }
        self.opens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let repo = crate::isolation::open(path);
        let mut map = self.kept.lock().unwrap();
        match &repo {
            Ok(r) => {
                let k = Kept {
                    repo: r.clone().into_sync(),
                    stamps: stamps(r),
                };
                map.insert(path.to_path_buf(), k);
            }
            Err(_) => {
                map.remove(path);
            }
        }
        repo
    }

    /// Forget the repos not matching `keep` (e.g. no longer in the workspace).
    pub fn retain(&self, keep: impl Fn(&Path) -> bool) {
        self.kept.lock().unwrap().retain(|p, _| keep(p));
    }

    /// The files a kept repo is checked against, for tests.
    #[doc(hidden)]
    pub fn checked(&self, path: &Path) -> Vec<PathBuf> {
        self.kept
            .lock()
            .unwrap()
            .get(path)
            .map_or_else(Vec::new, |k| {
                k.stamps.iter().map(|(f, _)| f.clone()).collect()
            })
    }

    /// How many times a repo was opened (rather than reused), for tests.
    #[doc(hidden)]
    pub fn opens(&self) -> usize {
        self.opens.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// `msg` without mentions of `path` (quoted or not, with a following
/// `: `), unless nothing would be left.
fn without_path(msg: &str, path: &Path) -> String {
    let p = path.display().to_string();
    let mut out = msg.to_string();
    for form in [
        format!("\"{p}\": "),
        format!("\"{p}\" "),
        format!("'{p}': "),
        format!("'{p}' "),
        format!("{p}: "),
        format!("{p} "),
    ] {
        out = out.replace(&form, "");
    }
    let out = out.trim();
    if out.is_empty() {
        msg.to_string()
    } else {
        out.to_string()
    }
}

/// The fetch remote (the default one, else the first) and its URL as
/// configured, before `insteadOf` rewrites: that is the repo's identity.
pub fn remote_and_url(repo: &gix::Repository) -> (Option<String>, Option<String>) {
    let remote = repo
        .remote_default_name(Direction::Fetch)
        .map(|n| n.to_str_lossy().into_owned())
        .or_else(|| {
            repo.remote_names()
                .into_iter()
                .next()
                .map(|n| n.to_str_lossy().into_owned())
        });
    let url = remote.as_deref().and_then(|r| {
        let r = repo.try_find_remote_without_url_rewrite(r)?.ok()?;
        r.url(Direction::Fetch)
            .map(|u| u.to_bstring().to_str_lossy().into_owned())
    });
    (remote, url)
}

/// The URLs, as configured, of the remotes other than `remote`, by name.
fn other_remotes(repo: &gix::Repository, remote: Option<&str>) -> Vec<String> {
    repo.remote_names()
        .into_iter()
        .filter(|n| Some(n.to_str_lossy().as_ref()) != remote)
        .filter_map(|n| {
            let name: &gix::bstr::BStr = n.as_ref();
            let r = repo.try_find_remote_without_url_rewrite(name)?.ok()?;
            let url = r.url(Direction::Fetch)?;
            Some(url.to_bstring().to_str_lossy().into_owned())
        })
        .collect()
}

/// Every ref of a repo, read once. Looking names up one at a time costs an
/// `open` of the loose file (usually missing: clones pack their refs) and a
/// packed-refs check each, and `open` dominated status's time.
struct Refs<'r> {
    refs: Vec<gix::Reference<'r>>,
    by_name: HashMap<String, usize>,
    peeled: RefCell<HashMap<usize, Option<gix::ObjectId>>>,
}

impl<'r> Refs<'r> {
    /// Branches, remote branches and tags: all that status uses. An
    /// unreadable branch fails, as status can't report on it; an
    /// unreadable tag is skipped.
    fn read(repo: &'r gix::Repository) -> Result<Self, StatusError> {
        let platform = repo.references().map_err(err)?;
        let mut refs = vec![];
        for (prefix, strict) in [
            ("refs/heads/", true),
            ("refs/remotes/", true),
            ("refs/tags/", false),
        ] {
            for r in platform.prefixed(prefix).map_err(err)? {
                match r {
                    Ok(r) => refs.push(r),
                    Err(e) if strict => return Err(err(e)),
                    Err(_) => {}
                }
            }
        }
        let by_name = refs
            .iter()
            .enumerate()
            .map(|(i, r)| (r.name().as_bstr().to_str_lossy().into_owned(), i))
            .collect();
        Ok(Refs {
            refs,
            by_name,
            peeled: RefCell::default(),
        })
    }

    fn get(&self, name: &str) -> Option<&gix::Reference<'r>> {
        self.by_name.get(name).map(|&i| &self.refs[i])
    }

    fn exists(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// Indices of the refs under `prefix` (e.g. `refs/heads/`).
    fn prefixed<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.refs
            .iter()
            .enumerate()
            .filter(move |(_, r)| r.name().as_bstr().starts_with(prefix.as_bytes()))
            .map(|(i, _)| i)
    }

    /// The object ref `i` leads to, through symbolic refs and tags.
    fn peel_at(&self, i: usize) -> Option<gix::ObjectId> {
        if let Some(&id) = self.peeled.borrow().get(&i) {
            return id;
        }
        let id = self.refs[i].clone().peel_to_id().ok().map(|id| id.detach());
        self.peeled.borrow_mut().insert(i, id);
        id
    }

    fn peel(&self, name: &str) -> Option<gix::ObjectId> {
        self.by_name.get(name).and_then(|&i| self.peel_at(i))
    }
}

/// A repo's status, each part of it a step.
pub fn status_of(repo: &gix::Repository, opts: StatusOptions) -> Result<RepoStatus, StatusError> {
    let h = repo.head().map_err(err)?;
    let head = head_of(&h);
    let head_id = h.id().map(|id| id.detach());
    let head_commit = head_id.and_then(|id| repo.find_commit(id).ok());
    let refs = crate::trace::part("refs", || Refs::read(repo))?;
    let (remote, remote_url) = remote_and_url(repo);
    let default_branch = default_branch(repo, &refs, remote.as_deref());
    let default_tip = default_branch
        .as_deref()
        .and_then(|d| default_tip(&refs, remote.as_deref(), d));

    let mut remembered = crate::remember::Remembered::of(repo);
    let branches = crate::trace::part("branches", || {
        branches(
            repo,
            &refs,
            head.branch(),
            default_branch.as_deref(),
            default_tip,
            opts.branches_first,
            &mut remembered,
        )
    })?;
    let upstream = head
        .branch()
        .and_then(|b| branches.iter().find(|bi| bi.name == b))
        .and_then(|bi| bi.upstream.clone());

    let bare = repo.workdir().is_none();
    // (one read of the submodules for both: it costs about as much as a
    // small repo's whole status)
    let (submodules, shown) = crate::trace::timed(
        "submodules",
        |(paths, _): &(Vec<PathBuf>, _)| format!("submodules: {} checked out", paths.len()),
        || checked_out_submodules(repo),
    );
    let submodules_shown = match opts.submodules && opts.submodule_content_from_own && !bare {
        true => shown,
        false => vec![],
    };
    let changes = if bare {
        Changes::default()
    } else {
        crate::trace::part("files", || {
            changes(
                repo,
                opts,
                head_commit.as_ref().and_then(|c| c.tree_id().ok()),
                &submodules_shown,
            )
        })?
    };
    let own_changes = (!submodules_shown.is_empty()).then(|| changes.clone());
    let moved_submodules = crate::trace::part("moved submodules", || {
        moved_submodules(repo, &changes.moved)
    });
    let missing_submodules = changes.missing.clone();
    let integration =
        crate::trace::part("upstream", || integration(repo, &head, upstream.as_ref()));
    let detached_orphans = match &head {
        Head::Detached(_) if repo.kind() != gix::repository::Kind::Submodule => {
            crate::trace::part("detached commits", || {
                detached_orphans(repo, &refs, head_id, &mut remembered)
            })?
        }
        _ => 0,
    };
    if branches.iter().any(|b| b.unchecked) {
        remembered.keep_had_too();
    }
    remembered.keep();
    let at_head = match (&head, head_id) {
        (Head::Detached(_), Some(id)) => refs
            .prefixed("refs/heads/")
            .filter(|&i| refs.peel_at(i) == Some(id))
            .map(|i| refs.refs[i].name().shorten().to_str_lossy().into_owned())
            .collect(),
        _ => vec![],
    };

    let (stash_ids, stashes) = crate::trace::part("stashes", || stash_entries(repo.common_dir()))
        .into_iter()
        .unzip();
    let tags = crate::trace::part("tags", || head_tags(repo, &refs, head_id));
    let (describe, describe_gave_up) = match (&head, head_id) {
        (Head::Detached(_), Some(id)) if tags.is_empty() => crate::trace::part("describe", || {
            describe(repo, &refs, id, opts.describe_limit)
        }),
        _ => (None, false),
    };
    let worktrees = crate::trace::part("worktrees", || worktrees(repo));
    Ok(RepoStatus {
        describe,
        describe_gave_up,
        head,
        head_commit: head_id.map(|id| id.to_string()),
        tags,
        default_branch,
        other_remotes: other_remotes(repo, remote.as_deref()),
        remote,
        remote_url,
        upstream,
        changes,
        stashes,
        stash_ids,
        detached_orphans,
        at_head,
        in_progress: repo.state().map(map_state),
        branches,
        worktrees: worktrees.0,
        stale_worktrees: worktrees.1,
        missing_submodules,
        submodules,
        submodules_shown,
        own_changes,
        moved_submodules,
        last_fetch: mtime(&repo.git_dir().join("FETCH_HEAD")).or_else(|| {
            (repo.common_dir() != repo.git_dir())
                .then(|| mtime(&repo.common_dir().join("FETCH_HEAD")))
                .flatten()
        }),
        last_commit: head_commit
            .and_then(|c| c.time().ok())
            .and_then(|t| Timestamp::from_second(t.seconds).ok()),
        bare,
        linked_worktree: repo.kind() == gix::repository::Kind::LinkedWorkTree,
        locked: repo.kind() == gix::repository::Kind::LinkedWorkTree
            && repo.git_dir().join("locked").symlink_metadata().is_ok(),
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
    let by = match pull_by(repo, branch) {
        Ok(by) => by,
        Err(why) => return Some(Integration::Refused(why)),
    };
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
        Some(0) => Integration::Clean(by),
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

/// How `git pull` would reconcile a diverged branch, or why it would
/// refuse: without `branch.<b>.rebase`, `pull.rebase` or `pull.ff` it asks
/// how to, and `pull.ff = only` refuses outright, even alongside a rebase
/// setting, as git does (only `--rebase` on its command line overrides
/// it, and argus doesn't pass that). The branch's
/// setting wins over `pull.rebase`; any non-boolean value (`merges`,
/// `interactive`) is a kind of rebase.
fn pull_by(repo: &gix::Repository, branch: &str) -> Result<PullBy, String> {
    let cfg = repo.config_snapshot();
    let ff = cfg
        .string("pull.ff")
        .map(|v| v.to_str_lossy().to_lowercase());
    if ff.as_deref() == Some("only") {
        return Err("pull.ff = only refuses diverged branches".into());
    }
    let rebase = |key: &str| {
        cfg.string(key)
            .map(|_| cfg.try_boolean(key).ok().flatten().unwrap_or(true))
    };
    match rebase(&format!("branch.{branch}.rebase")).or_else(|| rebase("pull.rebase")) {
        Some(true) => Ok(PullBy::Rebase),
        Some(false) => Ok(PullBy::Merge),
        None if ff.is_some() => Ok(PullBy::Merge),
        None => Err("git pull would refuse: set pull.rebase (or pull.ff) to choose".into()),
    }
}

fn head_of(head: &gix::Head<'_>) -> Head {
    let short = |name: &gix::refs::FullNameRef| name.shorten().to_str_lossy().into_owned();
    match head.referent_name() {
        Some(name) if head.is_unborn() => Head::Unborn(short(name)),
        Some(name) => Head::Branch(short(name)),
        None => Head::Detached(
            head.id()
                .map(|id| id.shorten_or_id().to_string())
                .unwrap_or_default(),
        ),
    }
}

/// The remote's HEAD if known (`refs/remotes/origin/HEAD`), otherwise a
/// local `main`/`master`, otherwise `init.defaultBranch`.
fn default_branch(repo: &gix::Repository, refs: &Refs, remote: Option<&str>) -> Option<String> {
    if let Some(remote) = remote
        && let Some(r) = refs.get(&format!("refs/remotes/{remote}/HEAD"))
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
        if refs.exists(&local) || tracked.is_some_and(|t| refs.exists(&t)) {
            return Some(candidate.to_string());
        }
    }
    repo.config_snapshot()
        .string("init.defaultBranch")
        .map(|s| s.to_str_lossy().into_owned())
}

/// Commit the default branch points to, and whether that's a remote's:
/// remote-tracking ref preferred, as it is what "pushed/merged" means;
/// local branch as a fallback.
fn default_tip(refs: &Refs, remote: Option<&str>, default: &str) -> Option<(gix::ObjectId, bool)> {
    let on_remote = remote.and_then(|r| refs.peel(&format!("refs/remotes/{r}/{default}")));
    match on_remote {
        Some(id) => Some((id, true)),
        None => Some((refs.peel(&format!("refs/heads/{default}"))?, false)),
    }
}

/// Whether merging `tip` into `default` would change nothing (see
/// [`BranchInfo::absorbed`]): a trial merge in memory, nothing written to
/// the repo. A conflict, or any failure, is a no; so is a branch with no
/// net change since it split off (empty commits, a change and its
/// revert), which nothing absorbed. The answer is the two commits' alone,
/// so it's remembered ([`crate::remember`]): a repo with many stale
/// branches would pay for each of them at every status.
fn absorbed(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    default: gix::ObjectId,
    remembered: &mut crate::remember::Remembered,
) -> bool {
    remembered
        .absorbed(tip, default, || {
            TRIAL_MERGES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // (a merge that couldn't be tried at all, objects missing say,
            // isn't an answer to keep)
            trial_merge(repo, tip, default)
        })
        .unwrap_or(false)
}

static TRIAL_MERGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many trial merges this process has run (for tests of the memory).
#[doc(hidden)]
pub fn trial_merges() -> usize {
    TRIAL_MERGES.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`absorbed`], worked out: `None` when the merge couldn't be tried.
fn trial_merge(repo: &gix::Repository, tip: gix::ObjectId, default: gix::ObjectId) -> Option<bool> {
    use gix::merge::tree::TreatAsUnresolved;
    let repo = repo.clone().with_object_memory();
    let options = repo
        .tree_merge_options()
        .ok()?
        .with_fail_on_conflict(Some(TreatAsUnresolved::git()));
    let mut out = repo
        .merge_commits(default, tip, Default::default(), options.into())
        .ok()?;
    if out
        .tree_merge
        .has_unresolved_conflicts(TreatAsUnresolved::git())
    {
        return Some(false);
    }
    let theirs = repo.find_commit(tip).ok()?.tree_id().ok()?.detach();
    if theirs == out.merge_base_tree_id {
        return Some(false);
    }
    let merged = out.tree_merge.tree.write().ok()?.detach();
    let ours = repo.find_commit(default).ok()?.tree_id().ok()?.detach();
    Some(merged == ours)
}

/// Number of commits reachable from `a` but not from `b`: remembered,
/// as the two commits alone decide it.
fn count_exclusive(
    repo: &gix::Repository,
    a: gix::ObjectId,
    b: gix::ObjectId,
    remembered: &mut crate::remember::Remembered,
) -> Result<usize, StatusError> {
    if a == b {
        return Ok(0);
    }
    remembered.between(a, b, || {
        let walk = repo.rev_walk([a]).with_hidden([b]).all().map_err(err)?;
        let mut n = 0;
        for info in walk {
            info.map_err(err)?;
            n += 1;
        }
        Ok(n)
    })
}

/// The remote-tracking ref of the branch `git push` would push `branch`
/// to, when that's the same-named branch (`push.default` `current`, say);
/// `None` when it's another (`upstream` to a feature branch's `main`) or
/// `git push` would refuse (`simple` with a differently named upstream).
fn own_push_ref(
    repo: &gix::Repository,
    branch: &gix::refs::FullNameRef,
) -> Option<gix::refs::FullName> {
    match repo.branch_remote_ref_name(branch, Direction::Push)? {
        Ok(dest) if dest.as_ref() == branch => {}
        _ => return None,
    }
    repo.branch_remote_tracking_ref_name(branch, Direction::Push)?
        .ok()
}

/// Every branch, all checked or (with `first`, see
/// [`StatusOptions::branches_first`]) some.
fn branches(
    repo: &gix::Repository,
    refs: &Refs,
    head_branch: Option<&str>,
    default_branch: Option<&str>,
    default_tip: Option<(gix::ObjectId, bool)>,
    first: Option<usize>,
    remembered: &mut crate::remember::Remembered,
) -> Result<Vec<BranchInfo>, StatusError> {
    let checked = first.map(|n| checked_first(repo, refs, [head_branch, default_branch], n));
    let remote_tips = crate::trace::timed(
        "remote tips",
        |tips: &Vec<gix::ObjectId>| format!("remote tips: {}", tips.len()),
        || {
            let mut remote_tips: Vec<gix::ObjectId> = vec![];
            for i in refs.prefixed("refs/remotes/") {
                if let gix::refs::TargetRef::Symbolic(_) = refs.refs[i].target() {
                    continue; // origin/HEAD
                }
                if let Some(id) = refs.peel_at(i) {
                    remote_tips.push(id);
                }
            }
            remote_tips.sort();
            remote_tips.dedup();
            remote_tips
        },
    );
    let mut out = vec![];
    for i in refs.prefixed("refs/heads/") {
        let full = refs.refs[i].name().to_owned();
        let name = full.shorten().to_str_lossy().into_owned();
        let said = name.clone();
        let Some(tip) = refs.peel_at(i) else { continue };
        let is_head = head_branch == Some(name.as_str());
        if checked.as_ref().is_some_and(|c| !c.contains(&i)) {
            out.push(BranchInfo {
                is_head,
                name,
                tip: tip.to_string(),
                upstream: None,
                merged: false,
                absorbed: false,
                unpushed: None,
                own: None,
                unchecked: true,
            });
            continue;
        }
        let info = crate::trace::step(
            "branch",
            || {
                let at = repo.workdir().unwrap_or_else(|| repo.git_dir());
                format!("branch {said} in {}", crate::paths::contract_tilde(at))
            },
            || {
                // remote branches this one's commits are likeliest to be on, and
                // whether any is known to have them all
                let mut likely: Vec<gix::ObjectId> = vec![];
                let mut all_on_remote = false;
                let mut on_remote =
                    |name: &gix::refs::FullNameRef, at: gix::ObjectId, ahead: usize| {
                        if name.as_bstr().starts_with(b"refs/remotes/") {
                            likely.push(at);
                            all_on_remote |= ahead == 0;
                        }
                    };
                let upstream = crate::trace::part("upstream", || -> Result<_, StatusError> {
                    Ok(
                        match repo.branch_remote_tracking_ref_name(full.as_ref(), Direction::Fetch)
                        {
                            Some(Ok(tracking)) => {
                                let short = tracking.shorten().to_str_lossy().into_owned();
                                match refs.peel(&tracking.as_bstr().to_str_lossy()) {
                                    Some(up) => Some(Tracking {
                                        name: short,
                                        ahead: {
                                            let ahead = count_exclusive(repo, tip, up, remembered)?;
                                            on_remote(tracking.as_ref(), up, ahead);
                                            ahead
                                        },
                                        behind: count_exclusive(repo, up, tip, remembered)?,
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
                        },
                    )
                })?;
                let own = crate::trace::part("push branch", || -> Result<_, StatusError> {
                    Ok(match (&upstream, own_push_ref(repo, full.as_ref())) {
                        (Some(t), Some(tracking))
                            if t.name != tracking.shorten().to_str_lossy() =>
                        {
                            let short = tracking.shorten().to_str_lossy().into_owned();
                            Some(match refs.peel(&tracking.as_bstr().to_str_lossy()) {
                                Some(o) => Tracking {
                                    ahead: {
                                        let ahead = count_exclusive(repo, tip, o, remembered)?;
                                        on_remote(tracking.as_ref(), o, ahead);
                                        ahead
                                    },
                                    behind: count_exclusive(repo, o, tip, remembered)?,
                                    name: short,
                                    gone: false,
                                },
                                None => Tracking {
                                    name: short,
                                    ahead: 0,
                                    behind: 0,
                                    gone: true,
                                },
                            })
                        }
                        _ => None,
                    })
                })?;
                let merged = crate::trace::part("merged", || match default_tip {
                    Some((d, _)) => {
                        Ok::<_, StatusError>(count_exclusive(repo, tip, d, remembered)? == 0)
                    }
                    None => Ok(false),
                })?;
                let absorbed = !merged
                    && crate::trace::part("absorbed", || {
                        default_tip.is_some_and(|(d, _)| absorbed(repo, tip, d, remembered))
                    });
                // merged into the remote's default branch: all on a remote
                if let Some((d, true)) = default_tip {
                    all_on_remote |= merged;
                    likely.push(d);
                }
                // and a remote branch of the same name
                let same_name = format!("/{name}");
                likely.extend(
                    refs.prefixed("refs/remotes/")
                        .filter(|&i| {
                            refs.refs[i]
                                .name()
                                .as_bstr()
                                .ends_with(same_name.as_bytes())
                        })
                        .filter_map(|i| refs.peel_at(i)),
                );
                let unpushed = if remote_tips.is_empty() {
                    None
                } else if all_on_remote {
                    Some(0)
                } else {
                    let counted = crate::trace::timed(
                        "unpushed",
                        |c: &Result<crate::reach::Counted, _>| match c {
                            Ok(c) => format!("unpushed: {}, {}", c.n, c.how),
                            Err(_) => "unpushed, failed".into(),
                        },
                        || crate::reach::exclusive(repo, tip, &likely, &remote_tips, remembered),
                    )
                    .map_err(err)?;
                    Some(counted.n)
                };
                // absorbed into a remote's default branch: nothing of it is unpushed
                let unpushed = match default_tip {
                    Some((_, true)) if absorbed => Some(0),
                    _ => unpushed,
                };
                Ok(BranchInfo {
                    is_head,
                    name,
                    tip: tip.to_string(),
                    upstream,
                    merged,
                    absorbed,
                    unpushed,
                    own,
                    unchecked: false,
                })
            },
        )?;
        out.push(info);
    }
    out.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// The branches (indices into `refs`) to check first: those named in
/// `always` (HEAD's, the default), and `n` of the rest, the newest tips.
fn checked_first(
    repo: &gix::Repository,
    refs: &Refs,
    always: [Option<&str>; 2],
    n: usize,
) -> std::collections::HashSet<usize> {
    let mut first = std::collections::HashSet::new();
    let mut rest = vec![];
    for i in refs.prefixed("refs/heads/") {
        let name = refs.refs[i].name().shorten();
        if always.iter().flatten().any(|a| name == a.as_bytes()) {
            first.insert(i);
            continue;
        }
        // (a tip that's no commit, or can't be read, counts as oldest)
        let when = refs
            .peel_at(i)
            .and_then(|id| repo.find_commit(id).ok())
            .and_then(|c| c.time().ok())
            .map_or(i64::MIN, |t| t.seconds);
        rest.push((std::cmp::Reverse(when), i));
    }
    rest.sort();
    first.extend(rest.into_iter().take(n).map(|(_, i)| i));
    first
}

/// Every branch of the repo at `path`, checked: those a status with
/// [`StatusOptions::branches_first`] left unchecked, for one to show.
pub fn branches_of(path: &Path) -> Result<Vec<BranchInfo>, StatusError> {
    traced_branches(path, || {
        crate::isolation::open(path)
            .map_err(err)
            .and_then(|repo| all_branches(&repo))
    })
}

/// `read` as a step.
fn traced_branches(
    path: &Path,
    read: impl FnOnce() -> Result<Vec<BranchInfo>, StatusError>,
) -> Result<Vec<BranchInfo>, StatusError> {
    let what = || format!("branches {}", crate::paths::contract_tilde(path));
    crate::trace::step("branches", what, read).map_err(|e| StatusError(without_path(&e.0, path)))
}

fn all_branches(repo: &gix::Repository) -> Result<Vec<BranchInfo>, StatusError> {
    let h = repo.head().map_err(err)?;
    let head = head_of(&h);
    let refs = Refs::read(repo)?;
    let (remote, _) = remote_and_url(repo);
    let default_branch = default_branch(repo, &refs, remote.as_deref());
    let default_tip = default_branch
        .as_deref()
        .and_then(|d| default_tip(&refs, remote.as_deref(), d));
    let mut remembered = crate::remember::Remembered::of(repo);
    let branches = branches(
        repo,
        &refs,
        head.branch(),
        default_branch.as_deref(),
        default_tip,
        None,
        &mut remembered,
    )?;
    remembered.keep();
    Ok(branches)
}

/// The untracked files in `dir` (repo-relative, ending in `/`), which
/// status shows collapsed, up to `limit`, sorted, each a file or a nested
/// repo; and whether there are more. Ignored files are left out, as git does.
pub fn untracked_in(
    path: &Path,
    dir: &str,
    limit: usize,
) -> Result<(Vec<(String, PathKind)>, bool), StatusError> {
    let mut v = untracked(path, Some(dir))?;
    let more = v.len() > limit;
    v.truncate(limit);
    Ok((v, more))
}

/// Remote branches (`origin/x`) with no local branch of the same name, for
/// a list of branches to switch to; a remote's `HEAD` isn't one.
pub fn remote_only_branches(path: &Path) -> Result<Vec<String>, StatusError> {
    let repo = crate::isolation::open(path).map_err(err)?;
    let refs = repo.references().map_err(err)?;
    let names = |prefix: &str| -> Result<Vec<String>, StatusError> {
        Ok(refs
            .prefixed(prefix)
            .map_err(err)?
            .filter_map(|r| r.ok())
            .map(|r| r.name().shorten().to_str_lossy().into_owned())
            .collect())
    };
    let local = names("refs/heads/")?;
    let mut v: Vec<String> = names("refs/remotes/")?
        .into_iter()
        .filter(|n| {
            let short = n.split_once('/').map_or(n.as_str(), |(_, b)| b);
            short != "HEAD" && !local.iter().any(|l| l == short)
        })
        .collect();
    v.sort();
    Ok(v)
}

/// The repos nested untracked in the work tree, repo-relative.
pub fn nested_repos(path: &Path) -> Result<Vec<String>, StatusError> {
    Ok(untracked(path, None)?
        .into_iter()
        .filter(|(_, k)| *k == PathKind::Repo)
        .map(|(p, _)| p)
        .collect())
}

/// Every untracked file (in `dir`), sorted.
fn untracked(path: &Path, dir: Option<&str>) -> Result<Vec<(String, PathKind)>, StatusError> {
    use gix::status::index_worktree::Item as IW;
    let repo = crate::isolation::open(path).map_err(err)?;
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(err)?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        })
        .into_iter(dir.map(gix::bstr::BString::from))
        .map_err(err)?;
    let mut v = vec![];
    for item in iter {
        if let gix::status::Item::IndexWorktree(IW::DirectoryContents { entry, .. }) =
            item.map_err(err)?
            && entry.status == gix::dir::entry::Status::Untracked
        {
            let p = entry.rela_path.to_str_lossy().into_owned();
            if p.starts_with(dir.unwrap_or("")) {
                let kind = match entry.disk_kind {
                    Some(gix::dir::entry::Kind::Repository) => PathKind::Repo,
                    _ => PathKind::File,
                };
                v.push((p, kind));
            }
        }
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(v)
}

/// What a status reported of one path, from several threads in no order:
/// what's staged, what's changed in its tracked file (or submodule),
/// whether there's something untracked there, whether it's conflicted.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Seen {
    index: char,
    worktree: char,
    untracked: bool,
    conflicted: bool,
}

impl Seen {
    /// Its (index, worktree) codes: conflicted is `U` both; a change to
    /// what's tracked shows over something untracked in its place (a file
    /// deleted, a folder there); untracked shows beside what's staged (a
    /// file staged for deletion and still there, `git rm --cached`), else
    /// is `?` both.
    fn codes(self) -> (char, char) {
        let blank = |c: char| if c == '\0' { ' ' } else { c };
        let (index, worktree) = (blank(self.index), blank(self.worktree));
        match self {
            _ if self.conflicted => ('U', 'U'),
            _ if worktree != ' ' => (index, worktree),
            _ if self.untracked && index != ' ' => (index, '?'),
            _ if self.untracked => ('?', '?'),
            _ => (index, worktree),
        }
    }
}

/// Of the checked-out submodules `shown` (but those `ignoring_all`), the
/// paths of those at another commit than `index` records, as git finds
/// them for a parent's status: from each one's HEAD, through its refs
/// alone. One whose HEAD can't be read is taken as where it should be, as
/// git takes it.
fn moved_submodules_of(
    index: &gix::index::File,
    shown: &[SubmoduleShown],
    ignoring_all: &std::collections::HashSet<&str>,
) -> Vec<String> {
    use gix::bstr::ByteSlice;
    use rayon::prelude::*;
    let hash = index.object_hash();
    let mut moved: Vec<String> = shown
        .par_iter()
        .filter(|sm| !ignoring_all.contains(sm.rela.as_str()))
        .filter_map(|sm| {
            let entry = index.entry_by_path(sm.rela.as_bytes().as_bstr())?;
            let head = checked_out(&sm.path, hash)?;
            (entry.mode.is_submodule() && head != entry.id).then(|| sm.rela.clone())
        })
        .collect();
    moved.sort();
    moved
}

/// The commit the work tree at `worktree` has checked out, read from its
/// refs alone (not opening it as a repo): `None` if that can't be read.
fn checked_out(worktree: &Path, hash: gix::hash::Kind) -> Option<gix::ObjectId> {
    let dot_git = worktree.join(".git");
    let git_dir = match dot_git.is_dir() {
        true => dot_git,
        false => {
            let text = std::fs::read_to_string(&dot_git).ok()?;
            worktree.join(text.strip_prefix("gitdir:")?.trim())
        }
    };
    let refs = gix::refs::file::Store::at(git_dir, hash);
    let mut r = refs.find("HEAD").ok()?;
    // (HEAD to a branch, at most a few names deep)
    for _ in 0..5 {
        match r.target {
            gix::refs::Target::Object(id) => return Some(id),
            gix::refs::Target::Symbolic(name) => r = refs.find(name.as_ref()).ok()?,
        }
    }
    None
}

/// Whether the submodule at `path` has changes to tracked content. The
/// status walk stops at a submodule's first change (`check_dirty`), and
/// that may be an untracked file while tracked ones changed too: asked
/// only then, and without looking for untracked files again.
fn tracked_content_changed(repo: &gix::Repository, path: &str) -> bool {
    let Ok(Some(mut subs)) = repo.submodules() else {
        return false;
    };
    subs.find(|sm| sm.path().is_ok_and(|p| p.as_slice() == path.as_bytes()))
        .and_then(|sm| {
            sm.status(gix::submodule::config::Ignore::Untracked, true)
                .ok()
        })
        .is_some_and(|st| st.changes.is_some_and(|c| !c.is_empty()))
}

/// `head_tree`, when known, saves gix reading HEAD again; `shown`, the
/// submodules when their own statuses say what's changed inside them
/// ([`StatusOptions::submodule_content_from_own`]).
fn changes(
    repo: &gix::Repository,
    opts: StatusOptions,
    head_tree: Option<gix::Id<'_>>,
    shown: &[SubmoduleShown],
) -> Result<Changes, StatusError> {
    use gix::status::index_worktree::Item as IW;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    use gix::submodule::config::Ignore;
    // what's changed inside each submodule is its own status's to say: only
    // one at another commit shows here, and not one that ignores all
    let ignoring_all: std::collections::HashSet<&str> = shown
        .iter()
        .filter(|sm| sm.ignore == Ignore::All)
        .map(|sm| sm.rela.as_str())
        .collect();
    // the commit each submodule is at, argus reads itself (below): gix's
    // check searches every submodule's config for each, so a parent of
    // thousands takes minutes (TODO.md: to remove once gix is fixed)
    let heads_ours = opts.submodules && opts.submodule_content_from_own;
    let submodules = if heads_ours {
        gix::status::Submodule::Given {
            ignore: Ignore::All,
            check_dirty: false,
        }
    } else if opts.submodules {
        gix::status::Submodule::AsConfigured { check_dirty: true }
    } else {
        gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        }
    };
    let mut platform = repo.status(gix::progress::Discard).map_err(err)?;
    if let Some(t) = head_tree {
        platform = platform.head_tree(t.detach());
    }
    let mut iter = platform
        // Per-file, then collapsed below like git does. gix's own collapsing
        // reports directories holding only ignored files/empty dirs as untracked.
        .untracked_files(gix::status::UntrackedFiles::Files)
        .index_worktree_submodules(submodules)
        .into_iter(None)
        .map_err(err)?;

    // what's reported of each path, in whatever order: its codes after
    let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
    let mut kinds: HashMap<String, PathKind> = HashMap::new();
    let mut moved = vec![];
    let mut missing = vec![];
    for item in iter.by_ref() {
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
                let path = change.location().to_str_lossy().into_owned();
                if change.entry_mode().is_submodule() {
                    kinds.insert(path.clone(), PathKind::Submodule);
                }
                seen.entry(path).or_default().index = code;
            }
            gix::status::Item::IndexWorktree(iw) => {
                let path = iw.rela_path().to_str_lossy().into_owned();
                // files only (see above); directories are collapsed below
                if let IW::DirectoryContents { entry, .. } = &iw
                    && entry.disk_kind == Some(gix::dir::entry::Kind::Repository)
                {
                    kinds.insert(path.clone(), PathKind::Repo);
                }
                let code = match &iw {
                    IW::Modification { status, .. } => match status {
                        EntryStatus::Conflict { .. } => 'U',
                        EntryStatus::Change(Change::SubmoduleModification(sm)) => {
                            if ignoring_all.contains(path.as_str()) {
                                continue;
                            }
                            kinds.insert(path.clone(), PathKind::Submodule);
                            let code = if sm.checked_out_head_id != sm.index_id {
                                moved.push(path.clone());
                                'M'
                            } else if sm.changes.iter().flatten().any(|i| {
                                !matches!(
                                    i,
                                    gix::status::Item::IndexWorktree(IW::DirectoryContents { .. })
                                )
                            }) || tracked_content_changed(repo, &path)
                            {
                                'm'
                            } else {
                                '?'
                            };
                            seen.entry(path).or_default().worktree = code;
                            continue;
                        }
                        EntryStatus::Change(Change::Removed) => {
                            // a submodule's folder deleted: still a submodule
                            if let IW::Modification { entry, .. } = &iw
                                && entry.mode.is_submodule()
                            {
                                kinds.insert(path.clone(), PathKind::Submodule);
                                missing.push(path.clone());
                            }
                            'D'
                        }
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
                let s = seen.entry(path).or_default();
                match code {
                    '?' => s.untracked = true,
                    'U' => s.conflicted = true,
                    c => s.worktree = c,
                }
            }
        }
    }

    let mut stale = 0;
    if let Some(out) = iter.outcome_mut() {
        if heads_ours {
            for path in moved_submodules_of(&out.worktree_index, shown, &ignoring_all) {
                kinds.insert(path.clone(), PathKind::Submodule);
                moved.push(path.clone());
                seen.entry(path).or_default().worktree = 'M';
            }
        }
        stale = out
            .index_worktree
            .tracked_file_modification
            .entries_to_update;
        crate::trace::amend(|| files_read(out));
    }
    // path -> (index code, worktree code)
    let mut files: BTreeMap<String, (char, char)> =
        seen.into_iter().map(|(p, s)| (p, s.codes())).collect();
    collapse_untracked(repo, &mut files, &mut kinds)?;
    let mut c = Changes {
        moved,
        missing,
        stale,
        ..Default::default()
    };
    for (path, (x, y)) in files {
        let kind = kinds.get(&path).copied().unwrap_or_default();
        let sub = kind == PathKind::Submodule;
        match (x, y) {
            ('?', _) => {
                c.untracked += 1;
                c.untracked_repos += usize::from(kind == PathKind::Repo);
            }
            ('U', _) => c.conflicted += 1,
            // a staged deletion whose file is still there, untracked
            (_, '?') if !sub => {
                c.staged += 1;
                c.untracked += 1;
            }
            _ => {
                // changes inside a submodule aren't the parent's
                let inside = sub && matches!(y, 'm' | '?');
                if x != ' ' {
                    c.staged += 1;
                    c.staged_submodules += usize::from(sub);
                } else if inside {
                    c.submodule_content += 1;
                }
                if y != ' ' && !inside {
                    c.unstaged += 1;
                    c.unstaged_submodules += usize::from(sub);
                }
            }
        }
        if c.files.len() < MAX_FILES {
            c.files.push(FileChange {
                path,
                index: x,
                worktree: y,
                kind,
            });
        }
    }
    Ok(c)
}

/// What a status had to do, said as its `files` step: the files it read
/// (the rest it took as unchanged from what the index says of them), of
/// those the ones that hadn't changed after all, though the index said
/// they might have (stale: see [`Changes::stale`]), and how far it walked
/// for untracked files; and, when it looked for renames, how many pairs
/// of files it compared, and how many it left uncompared, past the limit
/// (`diff.renameLimit`), where it compares none.
fn files_read(out: &gix::status::Outcome) -> String {
    let tracked = &out.index_worktree.tracked_file_modification;
    let mut said = format!(
        "files: {} tracked, {} read ({}): {} unchanged, {} too new to trust",
        tracked.entries_to_process,
        tracked.worktree_files_read,
        size(tracked.worktree_bytes),
        tracked.entries_to_update,
        tracked.racy_clean,
    );
    if let Some(walk) = &out.index_worktree.dirwalk {
        said += &format!(
            "; {} folders walked, {} entries",
            walk.read_dir_calls, walk.seen_entries
        );
    }
    let checks = |r: Option<&gix::diff::rewrites::Outcome>| {
        r.map_or((0, 0), |r| {
            (
                r.num_similarity_checks,
                r.num_similarity_checks_skipped_for_rename_tracking_due_to_limit
                    + r.num_similarity_checks_skipped_for_copy_tracking_due_to_limit,
            )
        })
    };
    // (renames among staged changes only: gix looks for unstaged ones
    // only when asked, and argus doesn't ask)
    let (compared, skipped) = checks(out.tree_index.as_ref().and_then(|t| t.rewrite.as_ref()));
    if compared > 0 {
        said += &format!("; {compared} compared for renames");
    }
    if skipped > 0 {
        said += &format!("; {skipped} left uncompared (rename limit)");
    }
    said
}

/// `bytes` in the largest unit that leaves at least 1 of it.
fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{}GB", b >> 30),
        b if b >= 1 << 20 => format!("{}MB", b >> 20),
        b if b >= 1 << 10 => format!("{}KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

/// Replace untracked files inside directories with no tracked content by the
/// outermost such directory (`dir/`), matching `git status` "normal" mode.
/// A directory holding only nested repos is a [`PathKind::Repo`] too.
fn collapse_untracked(
    repo: &gix::Repository,
    files: &mut BTreeMap<String, (char, char)>,
    kinds: &mut HashMap<String, PathKind>,
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
            let all_repos = kinds.get(&path) == Some(&PathKind::Repo)
                && (!files.contains_key(&dir) || kinds.get(&dir) == Some(&PathKind::Repo));
            if all_repos {
                kinds.insert(dir.clone(), PathKind::Repo);
            } else {
                kinds.remove(&dir);
            }
            files.insert(dir, ('?', '?'));
        }
    }
    Ok(())
}

/// Tags pointing at HEAD's commit. Packed refs record where annotated tags
/// lead, so only loose annotated tags cost an object read.
fn head_tags(repo: &gix::Repository, refs: &Refs, head: Option<gix::ObjectId>) -> Vec<String> {
    let Some(head) = head else {
        return vec![];
    };
    let mut found: Vec<(bool, String)> = vec![];
    for i in refs.prefixed("refs/tags/") {
        let r = &refs.refs[i];
        let gix::refs::Target::Object(target) = r.inner().target.clone() else {
            continue;
        };
        let peeled = match r.inner().peeled {
            Some(p) => p,
            None if target == head => head,
            None => match repo.find_header(target) {
                Ok(h) if h.kind() == gix::object::Kind::Tag => match refs.peel_at(i) {
                    Some(id) => id,
                    None => continue,
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

/// Per repo (git dir): HEAD, a fingerprint of the tags, the answer, and
/// the limit the walk gave up at, if it did (so the answer isn't final).
type Described = HashMap<PathBuf, (gix::ObjectId, u64, Option<String>, Option<usize>)>;

fn described() -> &'static std::sync::Mutex<Described> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Described>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The tagged commits, and a fingerprint of the tags for [`Described`]:
/// names count too, or a renamed tag would keep its old name.
fn tags_fingerprint(refs: &Refs) -> (std::collections::HashSet<gix::ObjectId>, u64) {
    use std::hash::{Hash, Hasher};
    let mut tags: Vec<(String, gix::ObjectId)> = refs
        .prefixed("refs/tags/")
        .filter_map(|i| Some((refs.refs[i].name().as_bstr().to_string(), refs.peel_at(i)?)))
        .collect();
    tags.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tags.hash(&mut h);
    (tags.into_iter().map(|t| t.1).collect(), h.finish())
}

/// `head` as `git describe --tags` names it.
fn describe_unbounded(repo: &gix::Repository, head: gix::ObjectId) -> Option<String> {
    let found = repo
        .find_commit(head)
        .ok()?
        .describe()
        .names(gix::commit::describe::SelectRef::AllTags)
        .try_resolve()
        .ok()??;
    Some(found.format().ok()?.to_string())
}

/// `head` as `git describe --tags` names it, if a tag is among its last
/// `limit` ancestors; and whether the walk gave up at the limit. gix's
/// describe can't be bounded, and with no tag in reach walks all of
/// history (2.4 ms per 1,000 commits without a commit-graph, measured), so
/// a bounded walk checks first. Answers are remembered per repo, HEAD and
/// tags; [`describe_fully`] replaces one that gave up.
fn describe(
    repo: &gix::Repository,
    refs: &Refs,
    head: gix::ObjectId,
    limit: usize,
) -> (Option<String>, bool) {
    let (tagged, fingerprint) = tags_fingerprint(refs);
    if tagged.is_empty() {
        return (None, false);
    }
    let dir = repo.git_dir().to_path_buf();
    if let Ok(cache) = described().lock()
        && let Some((id, fp, answer, gave_up_at)) = cache.get(&dir)
        && (*id, *fp) == (head, fingerprint)
        // a walk that gave up says nothing about a longer one
        && gave_up_at.is_none_or(|at| at >= limit)
    {
        return (answer.clone(), gave_up_at.is_some());
    }
    let (mut seen, mut found) = (0, false);
    // with limit 0, only HEAD (which has no tag) is seen
    if let Ok(walk) = repo.rev_walk([head]).all() {
        for info in walk.take(limit + 1).filter_map(Result::ok) {
            seen += 1;
            if tagged.contains(&info.id) {
                found = true;
                break;
            }
        }
    }
    // a walk that ran out of history before the limit is final too
    let (answer, gave_up) = match found {
        true => (describe_unbounded(repo, head), false),
        false => (None, limit == 0 || seen > limit),
    };
    if let Ok(mut c) = described().lock() {
        c.insert(
            dir,
            (head, fingerprint, answer.clone(), gave_up.then_some(limit)),
        );
    }
    (answer, gave_up)
}

/// Describe the repo's detached HEAD however far back its nearest tag is,
/// and remember the answer as final, so later statuses use it. For when
/// the bounded walk gave up (`describe_gave_up`) and the answer is wanted:
/// with no tag in history it walks all of it.
pub fn describe_fully(path: &Path) -> Option<String> {
    crate::trace::timed(
        "describe",
        |_| {
            format!(
                "describe HEAD fully in {}",
                crate::paths::contract_tilde(path)
            )
        },
        || read_description(path),
    )
}

fn read_description(path: &Path) -> Option<String> {
    let repo = crate::isolation::open(path).ok()?;
    let head = repo.head_id().ok()?.detach();
    let refs = Refs::read(&repo).ok()?;
    let (_, fingerprint) = tags_fingerprint(&refs);
    let answer = describe_unbounded(&repo, head);
    if let Ok(mut c) = described().lock() {
        c.insert(
            repo.git_dir().to_path_buf(),
            (head, fingerprint, answer.clone(), None),
        );
    }
    answer
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

fn detached_orphans(
    repo: &gix::Repository,
    refs: &Refs,
    head: Option<gix::ObjectId>,
    remembered: &mut crate::remember::Remembered,
) -> Result<usize, StatusError> {
    let Some(head) = head else {
        return Ok(0);
    };
    let mut tips = vec![];
    // not every ref: stash commits have HEAD as a parent and would hide everything
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        tips.extend(refs.prefixed(prefix).filter_map(|i| refs.peel_at(i)));
    }
    // HEAD is likeliest to have been left on a branch's history
    let branches: Vec<gix::ObjectId> = refs
        .prefixed("refs/heads/")
        .filter_map(|i| refs.peel_at(i))
        .collect();
    let counted = crate::reach::exclusive(repo, head, &branches, &tips, remembered).map_err(err)?;
    Ok(counted.n)
}

/// From the stash's reflog: one line per stash, oldest first, the message
/// after a tab.
fn stash_entries(common_dir: &Path) -> Vec<(String, String)> {
    let Ok(log) = std::fs::read(common_dir.join("logs/refs/stash")) else {
        return vec![];
    };
    // `<old id> <new id> <who> <when>\t<message>`
    let mut v: Vec<(String, String)> = log
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = String::from_utf8_lossy(l);
            let (head, message) = l.split_once('\t').unwrap_or((&l, ""));
            let id = head.split(' ').nth(1).unwrap_or("").to_string();
            (id, stash_message(message).to_string())
        })
        .collect();
    v.reverse();
    v
}

/// A stash message without `On <branch>: ` or `WIP on <branch>: ` (branch
/// names can't contain `:`).
fn stash_message(full: &str) -> &str {
    full.strip_prefix("On ")
        .or_else(|| full.strip_prefix("WIP on "))
        .and_then(|rest| rest.split_once(": "))
        .map_or(full, |(_, message)| message)
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

/// The linked worktrees, and the stale records' paths (folder gone, not
/// locked).
fn worktrees(repo: &gix::Repository) -> (Vec<PathBuf>, Vec<PathBuf>) {
    // A linked worktree sees all of its siblings (and itself); only the main
    // work tree reports them, so that each is listed exactly once.
    if repo.kind() == gix::repository::Kind::LinkedWorkTree {
        return (vec![], vec![]);
    }
    let Ok(wts) = repo.worktrees() else {
        return (vec![], vec![]);
    };
    let (stale, live): (Vec<_>, Vec<_>) = wts.iter().partition(|p| p.is_prunable());
    let mut v: Vec<PathBuf> = live.iter().filter_map(|p| p.base().ok()).collect();
    v.sort();
    let mut gone: Vec<PathBuf> = stale.iter().filter_map(|p| p.base().ok()).collect();
    gone.sort();
    (v, gone)
}

/// A submodule checked out at another commit than its repo records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedSubmodule {
    /// Repo-relative.
    pub path: String,
    /// Commits at its HEAD that no branch, remote branch or tag holds:
    /// what checking it out at the recorded commit would lose.
    pub unreferenced: usize,
    /// Changes to its tracked files, which a checkout could clash with.
    pub changed: bool,
}

/// The submodules at `moved` (gix doesn't look inside a submodule once its
/// commit differs, so its changes are asked here).
fn moved_submodules(repo: &gix::Repository, moved: &[String]) -> Vec<MovedSubmodule> {
    if moved.is_empty() {
        return vec![];
    }
    let Ok(Some(sms)) = repo.submodules() else {
        return vec![];
    };
    let mut v: Vec<MovedSubmodule> = sms
        .filter_map(|sm| {
            let path = sm.path().ok()?.to_str_lossy().into_owned();
            moved.contains(&path).then_some(())?;
            let r = sm.open().ok()??;
            let changed = r.is_dirty().unwrap_or(true);
            let head = r.head_id().ok().map(|id| id.detach());
            // unreadable counts as unsafe
            let mut remembered = crate::remember::Remembered::of(&r);
            let unreferenced = Refs::read(&r)
                .and_then(|refs| detached_orphans(&r, &refs, head, &mut remembered))
                .unwrap_or(usize::MAX);
            remembered.keep();
            Some(MovedSubmodule {
                path,
                unreferenced,
                changed,
            })
        })
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// Each checked-out submodule's work tree, sorted, and of those, where
/// each is in the parent's and how its changes show.
fn checked_out_submodules(repo: &gix::Repository) -> (Vec<PathBuf>, Vec<SubmoduleShown>) {
    let Ok(Some(sms)) = repo.submodules() else {
        return (vec![], vec![]);
    };
    let mut paths = vec![];
    let mut shown = vec![];
    // (not `sm.state()`: it searches every submodule's config section for
    // each, so a parent of thousands took seconds; TODO.md)
    for sm in sms.filter(|sm| has_checkout(repo, sm)) {
        let Some(path) = sm.work_dir().ok().flatten() else {
            continue;
        };
        if let Ok(rela) = sm.path() {
            shown.push(SubmoduleShown {
                path: path.clone(),
                rela: rela.to_str_lossy().into_owned(),
                ignore: sm.ignore().ok().flatten().unwrap_or_default(),
            });
        }
        paths.push(path);
    }
    paths.sort();
    (paths, shown)
}

/// Whether `sm` is checked out: its folder in the parent's work tree, no
/// symbolic link on the way, holds its `.git` (as gix's
/// `State::worktree_checkout`).
pub(crate) fn has_checkout(repo: &gix::Repository, sm: &gix::Submodule<'_>) -> bool {
    use gix::bstr::ByteSlice;
    use std::path::Component;
    let (Some(work_dir), Ok(rela)) = (repo.workdir(), sm.path()) else {
        return false;
    };
    let Ok(rela) = gix::path::from_bstr(rela.as_bstr()) else {
        return false;
    };
    let mut dir = work_dir.to_path_buf();
    for c in rela.components() {
        let Component::Normal(name) = c else {
            // (`..`, `/`: out of the work tree)
            return false;
        };
        dir.push(name);
        if dir
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return false;
        }
    }
    dir.join(".git").exists()
}

fn mtime(p: &Path) -> Option<Timestamp> {
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Timestamp::from_second(d.as_secs() as i64).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_in_a_collapsed_directory_finds_the_directory() {
        let change = |path: &str, index| FileChange {
            path: path.into(),
            index,
            worktree: index,
            kind: PathKind::File,
        };
        let c = Changes {
            files: vec![change("build/", '?'), change("src/", 'M')],
            ..Default::default()
        };
        let found = |p| c.entry(p).map(|f| f.path.as_str());
        assert_eq!(found("build/"), Some("build/"));
        assert_eq!(found("build/out/x"), Some("build/"));
        // only untracked directories are collapsed
        assert_eq!(found("src/x"), None);
        assert_eq!(found("other"), None);
    }

    #[test]
    fn config_candidates_include_files_that_dont_exist_yet() {
        let mut env = |k: &str| match k {
            "HOME" => Some("/nohome".into()),
            "XDG_CONFIG_HOME" => Some("/noxdg".into()),
            "GIT_CONFIG_SYSTEM" => Some("/nosystem".into()),
            _ => None,
        };
        let c = config_candidates(&mut env);
        assert!(c.contains(&PathBuf::from("/nosystem")), "{c:?}");
        assert!(c.contains(&PathBuf::from("/nohome/.gitconfig")), "{c:?}");
        assert!(c.contains(&PathBuf::from("/noxdg/git/config")), "{c:?}");
    }

    #[test]
    fn config_candidates_follow_git_config_global() {
        let mut env = |k: &str| match k {
            "HOME" => Some("/nohome".into()),
            "GIT_CONFIG_GLOBAL" => Some("/elsewhere".into()),
            _ => None,
        };
        let c = config_candidates(&mut env);
        assert!(c.contains(&PathBuf::from("/elsewhere")), "{c:?}");
        assert!(!c.contains(&PathBuf::from("/nohome/.gitconfig")), "{c:?}");
        #[cfg(unix)]
        assert!(c.contains(&PathBuf::from("/etc/gitconfig")), "{c:?}");
    }

    #[test]
    fn errors_drop_the_repo_path_only() {
        let p = Path::new("/r/app");
        for (msg, want) in [
            (
                "\"/r/app\" does not appear to be a git repository",
                "does not appear to be a git repository",
            ),
            ("/r/app: permission denied", "permission denied"),
            ("bad ref in '/r/app' HEAD", "bad ref in HEAD"),
            // inverse: other paths, including longer ones, stay
            ("/r/app2: missing", "/r/app2: missing"),
            ("/r/other: missing", "/r/other: missing"),
            // nothing left: keep the original
            ("/r/app", "/r/app"),
        ] {
            assert_eq!(without_path(msg, p), want, "{msg}");
        }
    }

    #[test]
    fn sizes_are_in_the_largest_unit_with_one_of_it() {
        let said: Vec<String> = [0, 1023, 1024, (1 << 20) - 1, 1 << 20, 5 << 30]
            .into_iter()
            .map(size)
            .collect();
        assert_eq!(
            said,
            ["0 bytes", "1023 bytes", "1KB", "1023KB", "1MB", "5GB"]
        );
    }

    /// A path's codes follow from what was reported of it, in whatever
    /// order: conflicted over all; a tracked change over something
    /// untracked in its place; untracked beside what's staged, else alone.
    #[test]
    fn a_paths_codes_follow_from_what_was_seen_of_it() {
        let seen = |index, worktree, untracked, conflicted| {
            Seen {
                index,
                worktree,
                untracked,
                conflicted,
            }
            .codes()
        };
        let none = '\0';
        assert_eq!(seen('M', 'D', true, true), ('U', 'U'));
        // a tracked file deleted, a folder in its place: with what's
        // staged, and without
        assert_eq!(seen('M', 'D', true, false), ('M', 'D'));
        assert_eq!(seen(none, 'D', true, false), (' ', 'D'));
        // staged for deletion and still there
        assert_eq!(seen('D', none, true, false), ('D', '?'));
        assert_eq!(seen(none, none, true, false), ('?', '?'));
        assert_eq!(seen('A', 'M', false, false), ('A', 'M'));
        assert_eq!(seen('A', none, false, false), ('A', ' '));
        assert_eq!(seen(none, 'M', false, false), (' ', 'M'));
    }

    /// A work tree's checked-out commit is read through its `.git`,
    /// whether a folder or a file naming one: on a branch, detached, and
    /// none while its branch has no commit yet, nor where there's no repo.
    #[test]
    fn the_commit_a_work_tree_has_checked_out_is_read_from_its_refs() {
        use crate::testkit::{Sandbox, commit, git};
        let sb = Sandbox::new();
        let r = sb.init("r");
        commit(&r, "one");
        let head = || git(&r, &["rev-parse", "HEAD"]).trim().to_string();
        let sha1 = gix::hash::Kind::Sha1;
        let read = |p: &Path| checked_out(p, sha1).map(|id| id.to_string());
        assert_eq!(read(&r), Some(head()), "on a branch, .git a folder");
        let first = head();
        commit(&r, "two");
        git(&r, &["switch", "-q", "--detach", &first]);
        assert_eq!(read(&r), Some(first.clone()), "detached");
        git(&r, &["switch", "-q", "--orphan", "unborn"]);
        assert_eq!(read(&r), None, "a branch with no commit yet");
        git(&r, &["switch", "-q", "main"]);
        // a work tree whose .git is a file naming the repo's folder
        let w = sb.path("w");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(
            w.join(".git"),
            format!("gitdir: {}\n", r.join(".git").display()),
        )
        .unwrap();
        assert_eq!(read(&w), Some(head()), ".git a file");
        std::fs::write(w.join(".git"), "not a gitdir line\n").unwrap();
        assert_eq!(read(&w), None);
        assert_eq!(read(&sb.path("nowhere")), None);
    }

    #[test]
    fn tracked_content_changed_is_asked_of_the_submodule_named() {
        use crate::testkit::{Sandbox, commit, git, write};
        let sb = Sandbox::new();
        let src = sb.init("libsrc");
        commit(&src, "l");
        let r = sb.init("r");
        for name in ["lib", "other"] {
            let allow = ["-c", "protocol.file.allow=always"];
            let add = ["submodule", "add", "-q", src.to_str().unwrap(), name];
            git(&r, &[&allow[..], &add[..]].concat());
        }
        commit(&r, "subs");
        let repo = crate::isolation::open(&r).unwrap();
        assert!(!tracked_content_changed(&repo, "lib"));
        // untracked alone isn't it
        write(&r.join("lib"), "new", "x");
        assert!(!tracked_content_changed(&repo, "lib"));
        write(&r.join("lib"), "README", "changed\n");
        assert!(tracked_content_changed(&repo, "lib"));
        // the one asked about, not another; nor one there isn't
        assert!(!tracked_content_changed(&repo, "other"));
        assert!(!tracked_content_changed(&repo, "nope"));
    }
}
