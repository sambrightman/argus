//! Commits behind a status, read on demand with gix for drilling down in
//! the details: the commits a branch has on no remote, those its upstream
//! has that it doesn't, those only a detached HEAD holds, and one commit's
//! message and changed files. Status itself only counts them.

use std::path::Path;

use gix::bstr::ByteSlice;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Which commits to list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Range {
    /// Reachable from `tip` (a ref name) but from no remote-tracking ref.
    NotOnRemote { tip: String },
    /// Reachable from `tip` but not from `from` (ref names or `HEAD`):
    /// what a pull would bring in, with `tip` the upstream.
    Missing { tip: String, from: String },
    /// Reachable from HEAD but from no branch, tag or remote ref.
    Orphans,
    /// Everything reachable from `tip`: its history.
    Ancestors { tip: String },
}

/// One commit, as a list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitLine {
    pub id: String,
    /// The message's first line.
    pub summary: String,
    pub author: String,
    pub time: Option<Timestamp>,
}

/// One commit in full: its message, and the files it changed against its
/// first parent (everything, for a root commit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitDetail {
    pub id: String,
    pub author: String,
    pub email: String,
    pub time: Option<Timestamp>,
    pub message: String,
    pub parents: Vec<String>,
    /// `A`dded, `D`eleted, `M`odified or `R`enamed (`old -> new`), by path.
    pub files: Vec<(char, String)>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Up to `limit` commits of `range`, newest first, and whether there are
/// more.
pub fn commits(
    repo: &Path,
    range: &Range,
    limit: usize,
) -> Result<(Vec<CommitLine>, bool), String> {
    crate::trace::timed(
        "commits",
        |r: &Result<(Vec<CommitLine>, bool), String>| {
            let what = match range {
                Range::NotOnRemote { tip } => format!("commits of {tip} on no remote"),
                Range::Missing { tip, from } => format!("commits of {tip} missing from {from}"),
                Range::Orphans => "commits on no branch".into(),
                Range::Ancestors { tip } => format!("history of {tip}"),
            };
            let n = r.as_ref().map_or(0, |(v, _)| v.len());
            format!(
                "{what} in {}: {n} (up to {limit})",
                crate::paths::contract_tilde(repo)
            )
        },
        || read_commits(repo, range, limit),
    )
}

fn read_commits(
    repo: &Path,
    range: &Range,
    limit: usize,
) -> Result<(Vec<CommitLine>, bool), String> {
    let repo = crate::isolation::open(repo).map_err(err)?;
    let id_of = |spec: &str| -> Result<gix::ObjectId, String> {
        Ok(repo.rev_parse_single(spec).map_err(err)?.detach())
    };
    let tips = |prefixes: &[&str]| -> Result<Vec<gix::ObjectId>, String> {
        let platform = repo.references().map_err(err)?;
        let mut v = vec![];
        for p in prefixes {
            for r in platform.prefixed(*p).map_err(err)? {
                let Ok(mut r) = r else { continue };
                if let Ok(id) = r.peel_to_id() {
                    v.push(id.detach());
                }
            }
        }
        Ok(v)
    };
    // hiding every ref can cost far more than the list: only those that
    // could hold any of it are hidden (see `reach`)
    let hiding = |tip, likely: &[gix::ObjectId], others: &[gix::ObjectId]| {
        let hidden = crate::reach::hiding(&repo, tip, likely, others, PRUNE_CAP)?;
        crate::trace::note(|| format!("hiding {} of {} tips", hidden.len(), others.len()));
        Ok::<_, String>(hidden)
    };
    let (tip, hidden) = match range {
        Range::NotOnRemote { tip: spec } => {
            let tip = id_of(spec)?;
            let others = tips(&["refs/remotes/"])?;
            (tip, hiding(tip, &likely_on_remote(&repo, spec), &others)?)
        }
        Range::Missing { tip, from } => (id_of(tip)?, vec![id_of(from)?]),
        Range::Orphans => {
            let head = id_of("HEAD")?;
            let others = tips(&["refs/heads/", "refs/remotes/", "refs/tags/"])?;
            (head, hiding(head, &tips(&["refs/heads/"])?, &others)?)
        }
        Range::Ancestors { tip } => (id_of(tip)?, vec![]),
    };
    let walk = repo
        .rev_walk([tip])
        .with_hidden(hidden)
        .all()
        .map_err(err)?;
    let mut v = vec![];
    for info in walk {
        let info = info.map_err(err)?;
        if v.len() == limit {
            return Ok((v, true));
        }
        let c = info.object().map_err(err)?;
        v.push(line(&c));
    }
    Ok((v, false))
}

/// Past this many commits on none of the likely refs, every ref is hidden
/// in a walk that stops at the list's end, as cheap as working out which
/// could matter.
const PRUNE_CAP: usize = 2000;

/// The remote branches `spec`'s commits are likeliest to be on: its
/// branch's upstream and push branch, and each remote's default branch.
fn likely_on_remote(repo: &gix::Repository, spec: &str) -> Vec<gix::ObjectId> {
    let peel = |name: &gix::refs::FullNameRef| {
        let mut r = repo.find_reference(name).ok()?;
        Some(r.peel_to_id().ok()?.detach())
    };
    let branch = match spec {
        "HEAD" => repo.head_name().ok().flatten(),
        _ => repo.find_reference(spec).ok().map(|r| r.name().to_owned()),
    };
    let mut v = vec![];
    if let Some(b) = &branch {
        use gix::remote::Direction::{Fetch, Push};
        for dir in [Fetch, Push] {
            if let Some(Ok(tracking)) = repo.branch_remote_tracking_ref_name(b.as_ref(), dir) {
                v.extend(peel(tracking.as_ref()));
            }
        }
    }
    if let Ok(platform) = repo.references()
        && let Ok(remotes) = platform.prefixed("refs/remotes/")
    {
        for mut r in remotes.flatten() {
            if r.name().as_bstr().ends_with(b"/HEAD")
                && let Ok(id) = r.peel_to_id()
            {
                v.push(id.detach());
            }
        }
    }
    v
}

fn line(c: &gix::Commit<'_>) -> CommitLine {
    let author = c.author().ok();
    CommitLine {
        id: c.id.to_string(),
        summary: c
            .message()
            .map(|m| m.summary().to_str_lossy().into_owned())
            .unwrap_or_default(),
        author: author
            .as_ref()
            .map(|a| a.name.to_str_lossy().into_owned())
            .unwrap_or_default(),
        time: author
            .and_then(|a| a.time().ok())
            .and_then(|t| Timestamp::from_second(t.seconds).ok()),
    }
}

/// The commit `id` (or any revision) in full.
pub fn commit_detail(repo: &Path, id: &str) -> Result<CommitDetail, String> {
    crate::trace::timed(
        "commit",
        |_| format!("commit {id} in {}", crate::paths::contract_tilde(repo)),
        || read_commit_detail(repo, id),
    )
}

fn read_commit_detail(repo: &Path, id: &str) -> Result<CommitDetail, String> {
    let repo = crate::isolation::open(repo).map_err(err)?;
    let c = repo
        .rev_parse_single(id)
        .map_err(err)?
        .object()
        .map_err(err)?
        .try_into_commit()
        .map_err(err)?;
    let author = c.author().map_err(err)?;
    let parents: Vec<gix::ObjectId> = c.parent_ids().map(|p| p.detach()).collect();
    let new = c.tree().map_err(err)?;
    let old = match parents.first() {
        Some(p) => Some(repo.find_commit(*p).map_err(err)?.tree().map_err(err)?),
        None => None,
    };
    let mut files = vec![];
    for change in repo
        .diff_tree_to_tree(old.as_ref(), Some(&new), None)
        .map_err(err)?
    {
        use gix::object::tree::diff::ChangeDetached as C;
        if change.entry_mode().is_tree() {
            continue;
        }
        files.push(match &change {
            C::Addition { location, .. } => ('A', location.to_str_lossy().into_owned()),
            C::Deletion { location, .. } => ('D', location.to_str_lossy().into_owned()),
            C::Modification { location, .. } => ('M', location.to_str_lossy().into_owned()),
            C::Rewrite {
                source_location,
                location,
                ..
            } => (
                'R',
                format!(
                    "{} -> {}",
                    source_location.to_str_lossy(),
                    location.to_str_lossy()
                ),
            ),
        });
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(CommitDetail {
        id: c.id.to_string(),
        author: author.name.to_str_lossy().into_owned(),
        email: author.email.to_str_lossy().into_owned(),
        time: author
            .time()
            .ok()
            .and_then(|t| Timestamp::from_second(t.seconds).ok()),
        message: c
            .message_raw()
            .map(|m| m.to_str_lossy().trim_end().to_string())
            .unwrap_or_default(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        files,
    })
}
