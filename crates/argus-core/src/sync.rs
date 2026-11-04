//! Sync planning: which repos to pull/fetch and which forge repos to clone.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{CloneMode, Track};
use crate::discover::RepoKind;
use crate::forge::{Protocol, RemoteRepo};
use crate::ops::Op;
use crate::status::Head;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Job {
    pub path: PathBuf,
    pub op: Op,
    /// Jobs sharing a family run sequentially (same object store / refs).
    pub family: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncPlan {
    pub jobs: Vec<Job>,
    /// Forge repos not cloned locally, for tracks with `clone = "notify"`.
    pub missing: Vec<RemoteRepo>,
    /// Local repos of a tracked owner that the forge no longer lists
    /// (deleted, renamed or transferred).
    pub orphans: Vec<PathBuf>,
}

impl SyncPlan {
    pub fn clones(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|j| matches!(j.op, Op::Clone { .. }))
    }
}

/// Pull/fetch every syncable, visible repo.
pub fn plan_updates(ws: &Workspace) -> Vec<Job> {
    let mut jobs = vec![];
    for e in &ws.entries {
        if !e.flags.sync || e.flags.hidden() {
            continue;
        }
        let Some(s) = e.status() else { continue };
        if s.remote.is_none() {
            continue;
        }
        let op = match (&s.head, &s.upstream, e.kind) {
            (_, _, RepoKind::Bare) => Op::Fetch,
            (Head::Branch(_), Some(t), _) if !t.gone => Op::Pull,
            _ => Op::Fetch,
        };
        let family = e.parent.clone().unwrap_or_else(|| e.path.clone());
        jobs.push(Job { label: format!("{} {}", op.label(), e.display_name()), path: e.path.clone(), op, family });
    }
    jobs
}

/// Compare forge listings with local clones.
///
/// `listings` maps track key to that track's listing; tracks whose listing
/// failed should simply be absent.
pub fn plan_tracks(
    ws: &Workspace,
    listings: &BTreeMap<String, Vec<RemoteRepo>>,
    protocol: Protocol,
) -> SyncPlan {
    let mut plan = SyncPlan::default();
    let local: HashSet<String> =
        ws.entries.iter().filter_map(|e| e.id.as_ref()).map(|i| i.to_string().to_lowercase()).collect();
    let mut planned: HashSet<String> = HashSet::new();
    for track in &ws.config.tracks {
        let Some(listing) = listings.get(&track.key()) else { continue };
        let listed: HashSet<String> = listing.iter().map(|r| r.id.to_string().to_lowercase()).collect();
        for r in listing.iter().filter(|r| wanted(track, r)) {
            let key = r.id.to_string().to_lowercase();
            if local.contains(&key) || !planned.insert(key) {
                continue;
            }
            let Some(dir) = ws.config.clone_dir(track) else { continue };
            let dest = dir.join(&r.id.name);
            if ws.rules().excludes(&r.id, &dest) {
                continue;
            }
            match track.clone {
                CloneMode::Auto if !dest.exists() => plan.jobs.push(Job {
                    label: format!("clone {}", r.id.slug()),
                    op: Op::Clone { url: r.clone_url(protocol).to_string(), dest: dest.clone() },
                    family: dest.clone(),
                    path: dest,
                }),
                // Destination occupied by something else: report rather than clobber.
                _ => plan.missing.push(r.clone()),
            }
        }
        if track.team.is_none() {
            for e in ws.entries.iter().filter(|e| e.kind == RepoKind::Main) {
                if let Some(id) = &e.id {
                    if id.host.eq_ignore_ascii_case(&track.host)
                        && id.owner.eq_ignore_ascii_case(&track.owner)
                        && !listed.contains(&id.to_string().to_lowercase())
                    {
                        plan.orphans.push(e.path.clone());
                    }
                }
            }
        }
    }
    plan
}

fn wanted(track: &Track, r: &RemoteRepo) -> bool {
    (track.include_archived || !r.archived) && (track.include_forks || !r.fork)
}
