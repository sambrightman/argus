//! How many commits are reachable from one tip but from none of some
//! others: a branch's commits on no remote, a detached HEAD's on no ref.
//!
//! A walk hiding every other tip costs in proportion to all their
//! histories down to where they meet the tip's, so with hundreds of remote
//! branches it's slow even when the answer is 0. Exact ways round it:
//!
//! 1. The answer for a tip and a set of others is remembered (in the
//!    repo's [`crate::remember::Remembered`]): commits don't change, so
//!    until a ref moves it's the same.
//! 2. The likely tips (a branch's upstream, the default branch) are hidden
//!    first. If that leaves nothing, nothing it is; the others could only
//!    hide more.
//! 3. Otherwise, of the commits left, generation numbers (from git's
//!    commit-graph) say which other tips could contain any: a tip reaching
//!    a commit has a generation at least the commit's, and a tip in the
//!    graph reaches nothing outside it (git writes it closed under
//!    parents). Those alone are hidden in a second walk. With no
//!    commit-graph, every tip is hidden, as before.

use std::sync::atomic::{AtomicUsize, Ordering};

use gix::ObjectId;

/// How [`exclusive`] came to its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The tip is one of the others: nothing.
    IsAnother,
    /// Known from an earlier count for the same tip and others.
    Remembered,
    /// Hiding the likely tips left nothing.
    Likely,
    /// The others that could contain what was left were hidden: `hidden`
    /// tips in all, likely ones included.
    Pruned { hidden: usize },
    /// Every other tip was hidden.
    Full,
}

impl std::fmt::Display for How {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            How::IsAnother => write!(f, "the tip is another's"),
            How::Remembered => write!(f, "remembered"),
            How::Likely => write!(f, "on the likely remote branches"),
            How::Pruned { hidden } => write!(f, "walked hiding {hidden} tips"),
            How::Full => write!(f, "walked hiding every tip"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counted {
    pub n: usize,
    pub how: How,
}

/// Commits reachable from `tip` and from none of `others`, hiding
/// `likely` (best among `others`) first.
pub fn exclusive(
    repo: &gix::Repository,
    tip: ObjectId,
    likely: &[ObjectId],
    others: &[ObjectId],
    remembered: &mut crate::remember::Remembered,
) -> Result<Counted, String> {
    let done = |n, how| Ok(Counted { n, how });
    let all = sorted(others);
    if all.binary_search(&tip).is_ok() {
        return done(0, How::IsAnother);
    }
    let of = crate::remember::fingerprint(&all);
    if let Some(n) = remembered.unpushed(tip, of) {
        return done(n, How::Remembered);
    }
    let first = among(likely, &all);
    let left = walk(repo, tip, &first)?;
    let counted = if left.is_empty() {
        Counted {
            n: 0,
            how: How::Likely,
        }
    } else if first.len() == all.len() {
        Counted {
            n: left.len(),
            how: How::Full,
        }
    } else {
        match could_contain(repo, &left, &all, &first) {
            Some(more) if more.is_empty() => Counted {
                n: left.len(),
                how: How::Pruned {
                    hidden: first.len(),
                },
            },
            Some(more) => {
                let hidden: Vec<ObjectId> = first.iter().chain(&more).copied().collect();
                Counted {
                    n: walk(repo, tip, &hidden)?.len(),
                    how: How::Pruned {
                        hidden: hidden.len(),
                    },
                }
            }
            None => Counted {
                n: walk(repo, tip, &all)?.len(),
                how: How::Full,
            },
        }
    };
    remembered.counted(tip, of, counted.n);
    Ok(counted)
}

/// Of `others`, the tips a walk from `tip` needs to hide to leave the
/// same commits as hiding them all: the likely ones, and those that could
/// contain what they leave (see [`exclusive`]). All of them when more
/// than `cap` commits are left after hiding the likely ones: finding what
/// could contain those would cost more than a walk that stops early.
pub fn hiding(
    repo: &gix::Repository,
    tip: ObjectId,
    likely: &[ObjectId],
    others: &[ObjectId],
    cap: usize,
) -> Result<Vec<ObjectId>, String> {
    let all = sorted(others);
    if all.binary_search(&tip).is_ok() {
        return Ok(vec![tip]);
    }
    let first = among(likely, &all);
    if first.len() == all.len() {
        return Ok(all);
    }
    WALKS.fetch_add(1, Ordering::Relaxed);
    let walk = repo
        .rev_walk([tip])
        .with_hidden(first.iter().copied())
        .all()
        .map_err(|e| e.to_string())?;
    let mut left = vec![];
    for info in walk {
        if left.len() == cap {
            return Ok(all);
        }
        left.push(info.map_err(|e| e.to_string())?.id);
    }
    if left.is_empty() {
        return Ok(first);
    }
    Ok(match could_contain(repo, &left, &all, &first) {
        Some(more) => first.into_iter().chain(more).collect(),
        None => all,
    })
}

fn sorted(tips: &[ObjectId]) -> Vec<ObjectId> {
    let mut v = tips.to_vec();
    v.sort();
    v.dedup();
    v
}

/// The `likely` tips that are among `all` (sorted), sorted.
fn among(likely: &[ObjectId], all: &[ObjectId]) -> Vec<ObjectId> {
    let v: Vec<ObjectId> = likely
        .iter()
        .filter(|l| all.binary_search(l).is_ok())
        .copied()
        .collect();
    sorted(&v)
}

/// The commits reachable from `tip` and from none of `hidden`.
fn walk(
    repo: &gix::Repository,
    tip: ObjectId,
    hidden: &[ObjectId],
) -> Result<Vec<ObjectId>, String> {
    WALKS.fetch_add(1, Ordering::Relaxed);
    let walk = repo
        .rev_walk([tip])
        .with_hidden(hidden.iter().copied())
        .all()
        .map_err(|e| e.to_string())?;
    walk.map(|info| info.map(|i| i.id).map_err(|e| e.to_string()))
        .collect()
}

/// Of `all` but `first`, the tips that could contain any of `left`: none
/// if there's no commit-graph. A tip reaching a commit has a generation
/// at least the commit's; and the graph holds every ancestor of a commit
/// in it, so a tip it has reaches none of `left` that it lacks. So: tips
/// the graph lacks, and those at or above the least generation in `left`.
fn could_contain(
    repo: &gix::Repository,
    left: &[ObjectId],
    all: &[ObjectId],
    first: &[ObjectId],
) -> Option<Vec<ObjectId>> {
    // (git doesn't use one in a shallow clone, whose parents are cut)
    if repo.is_shallow().unwrap_or(true) {
        return None;
    }
    let graph = repo.commit_graph_if_enabled().ok().flatten()?;
    let generation = |id: &ObjectId| graph.commit_by_id(id).map(|c| c.generation());
    let least = left.iter().filter_map(generation).min().unwrap_or(u32::MAX);
    Some(
        all.iter()
            .filter(|t| first.binary_search(t).is_err())
            .filter(|t| generation(t).is_none_or(|g| g >= least))
            .copied()
            .collect(),
    )
}

static WALKS: AtomicUsize = AtomicUsize::new(0);

/// How many walks this process has made (for tests of what's skipped).
pub fn walks() -> usize {
    WALKS.load(Ordering::Relaxed)
}
