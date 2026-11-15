//! Pull planning: which repos to pull/fetch and which forge repos to clone.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{CloneMode, Track};
use crate::discover::RepoKind;
use crate::forge::{Protocol, RemoteRepo};
use crate::identity::RepoId;
use crate::ops::Op;
use crate::status::{Head, Integration};
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
pub struct PullPlan {
    pub jobs: Vec<Job>,
    /// Forge repos not cloned locally, for tracks with `clone = "notify"`.
    pub missing: Vec<RemoteRepo>,
    /// Local repos of a tracked owner that the forge no longer lists
    /// (deleted, renamed or transferred).
    pub orphans: Vec<Orphan>,
}

/// A local repo of a tracked owner that the owner's listing hasn't got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Orphan {
    pub path: PathBuf,
    pub id: RepoId,
    /// The kind of forge to ask about it.
    #[serde(skip)]
    pub forge: String,
    pub state: OrphanState,
}

/// What became of an orphan, once its forge was asked
/// ([`locate_orphans`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrphanState {
    /// Not asked, or the forge couldn't say (why).
    Unknown(String),
    /// Still there under its name: only the listing leaves it out.
    Unlisted,
    /// Renamed or moved to another owner: what it is now.
    Renamed(RepoId),
    /// Deleted, or out of the token's reach. The clone may be the only
    /// copy left: nothing is ever done to it.
    Gone,
}

impl Orphan {
    /// What became of it, in words.
    pub fn describe(&self) -> String {
        match &self.state {
            OrphanState::Unknown(why) if why.is_empty() => {
                "no longer listed by the forge".to_string()
            }
            OrphanState::Unknown(why) => format!("no longer listed by the forge ({why})"),
            OrphanState::Unlisted => {
                format!("still on the forge, but not in {}'s listing", self.id.owner)
            }
            OrphanState::Renamed(to) => format!("now {}", to.slug()),
            OrphanState::Gone => "gone from the forge, or out of reach".to_string(),
        }
    }
}

/// Ask each orphan's forge what became of it (in parallel, one client
/// per forge).
pub fn locate_orphans(plan: &mut PullPlan, factory: ForgeFactory) {
    use rayon::prelude::*;
    let mut clients = BTreeMap::new();
    for o in &plan.orphans {
        clients
            .entry((o.forge.clone(), o.id.host.clone()))
            .or_insert_with(|| factory(&o.forge, &o.id.host).map_err(|e| e.to_string()));
    }
    let clients = &clients;
    plan.orphans.par_iter_mut().for_each(|o| {
        use crate::forge::Whereabouts;
        let said = match &clients[&(o.forge.clone(), o.id.host.clone())] {
            Ok(f) => f.whereabouts(&o.id).map_err(|e| e.to_string()),
            Err(e) => Err(e.clone()),
        };
        o.state = match said {
            Ok(Whereabouts::Here) => OrphanState::Unlisted,
            Ok(Whereabouts::Moved(to)) => OrphanState::Renamed(to),
            Ok(Whereabouts::Gone) => OrphanState::Gone,
            Err(why) => OrphanState::Unknown(why),
        };
    });
}

/// For each orphan that was renamed or moved: pointing its clone's remote
/// at what it is now, the URL otherwise as it was. Never run unasked.
pub fn follow_renames(ws: &Workspace, orphans: &[Orphan]) -> Vec<Job> {
    orphans
        .iter()
        .filter_map(|o| {
            let OrphanState::Renamed(to) = &o.state else {
                return None;
            };
            let s = ws.get(&o.path)?.status()?;
            let (remote, url) = (s.remote.clone()?, s.remote_url.as_deref()?);
            Some(Job {
                path: o.path.clone(),
                family: o.path.clone(),
                label: format!("follow {} to {}", o.id.slug(), to.slug()),
                op: Op::SetRemoteUrl {
                    remote,
                    url: o.id.renamed_url(url, to)?,
                },
            })
        })
        .collect()
}

impl PullPlan {
    pub fn clones(&self) -> impl Iterator<Item = &Job> {
        self.jobs
            .iter()
            .filter(|j| matches!(j.op, Op::Clone { .. }))
    }
}

/// Pull/fetch every repo marked for pull: snoozed ones too (hidden for a
/// while, still kept up to date); an ignored one never is.
pub fn plan_updates(ws: &Workspace) -> Vec<Job> {
    let mut jobs = vec![];
    for e in &ws.entries {
        if !e.flags.pull {
            continue;
        }
        let Some(s) = e.status() else { continue };
        if s.remote.is_none() {
            continue;
        }
        let op = match (&s.head, &s.upstream, e.kind) {
            (_, _, RepoKind::Bare) => Op::Fetch,
            // Only pull what can integrate cleanly; fetch the rest so the
            // repo is still up to date with its remote.
            (Head::Branch(_), Some(t), _)
                if !t.gone
                    && s.in_progress.is_none()
                    && matches!(s.integration, None | Some(Integration::Clean(_))) =>
            {
                Op::Pull
            }
            _ => Op::Fetch,
        };
        let family = e.parent.clone().unwrap_or_else(|| e.path.clone());
        jobs.push(Job {
            label: format!("{} {}", op.label(), e.display_name()),
            path: e.path.clone(),
            op,
            family,
        });
    }
    jobs
}

/// Fetch the repos at `indices` that have a remote: one job per family,
/// since worktrees and their main repo share refs.
pub fn plan_fetches(ws: &Workspace, indices: &[usize]) -> Vec<Job> {
    let mut seen = HashSet::new();
    let mut jobs = vec![];
    for &i in indices {
        let e = &ws.entries[i];
        if !e.status().is_some_and(|s| s.remote.is_some()) {
            continue;
        }
        let family = e.parent.clone().unwrap_or_else(|| e.path.clone());
        if !seen.insert(family.clone()) {
            continue;
        }
        jobs.push(Job {
            label: format!("fetch {}", e.display_name()),
            path: e.path.clone(),
            op: Op::Fetch,
            family,
        });
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
) -> PullPlan {
    let mut plan = PullPlan::default();
    let local: HashSet<String> = ws
        .entries
        .iter()
        .filter_map(|e| e.id.as_ref())
        .map(|i| i.to_string().to_lowercase())
        .collect();
    let mut planned: HashSet<String> = HashSet::new();
    for track in &ws.config.tracks {
        let Some(listing) = listings.get(&track.key()) else {
            continue;
        };
        let listed: HashSet<String> = listing
            .iter()
            .map(|r| r.id.to_string().to_lowercase())
            .collect();
        for r in listing.iter().filter(|r| wanted(track, r)) {
            let key = r.id.to_string().to_lowercase();
            if local.contains(&key) || !planned.insert(key) {
                continue;
            }
            let Some(dir) = ws.config.clone_dir(track) else {
                continue;
            };
            // one folder inside it, whatever the forge calls the repo
            let name = Path::new(&r.id.name);
            let plain = matches!(
                name.components().collect::<Vec<_>>().as_slice(),
                [std::path::Component::Normal(_)]
            );
            if !plain {
                continue;
            }
            let dest = dir.join(name);
            if ws.rules().ignores(&r.id, &dest) {
                continue;
            }
            match track.clone {
                CloneMode::Auto if !dest.exists() => plan.jobs.push(Job {
                    label: format!("clone {}", r.id.slug()),
                    op: Op::Clone {
                        url: r.clone_url(protocol).to_string(),
                        dest: dest.clone(),
                    },
                    family: dest.clone(),
                    path: dest,
                }),
                // Destination occupied by something else: report rather than clobber.
                _ => plan.missing.push(r.clone()),
            }
        }
        if track.team.is_none() {
            for e in ws
                .entries
                .iter()
                .filter(|e| e.kind == RepoKind::Main && !e.nested)
            {
                if let Some(id) = &e.id
                    && id.host.eq_ignore_ascii_case(&track.host)
                    && id.owner.eq_ignore_ascii_case(&track.owner)
                    && !listed.contains(&id.to_string().to_lowercase())
                {
                    plan.orphans.push(Orphan {
                        path: e.path.clone(),
                        id: id.clone(),
                        forge: track.forge.clone(),
                        state: OrphanState::Unknown(String::new()),
                    });
                }
            }
        }
    }
    plan
}

fn wanted(track: &Track, r: &RemoteRepo) -> bool {
    (track.include_archived || !r.archived) && (track.include_forks || !r.fork)
}

/// Creates a forge client for a track's `(forge kind, host)`.
pub type ForgeFactory<'a> = &'a (
        dyn Fn(&str, &str) -> Result<Box<dyn crate::forge::Forge>, crate::forge::ForgeError> + Sync
    );

#[derive(Debug, Default)]
pub struct Listings {
    pub repos: BTreeMap<String, Vec<RemoteRepo>>,
    /// Track key and error message for listings that failed.
    pub errors: Vec<(String, String)>,
    pub protocol: Protocol,
}

/// List every tracked owner (in parallel) and remember team membership in
/// state so that flags resolve offline.
pub fn list_tracks(ws: &mut Workspace, factory: ForgeFactory) -> Listings {
    use rayon::prelude::*;
    let results: Vec<_> = ws
        .config
        .tracks
        .par_iter()
        .map(|t| {
            let r = factory(&t.forge, &t.host).and_then(|f| {
                let p = f.protocol();
                f.list(&t.owner, t.team.as_deref()).map(|l| (l, p))
            });
            (t.key(), r)
        })
        .collect();
    let mut out = Listings::default();
    let now = jiff::Timestamp::now();
    for (key, r) in results {
        match r {
            Ok((list, protocol)) => {
                out.protocol = protocol;
                let ts = ws.state.tracks.entry(key.clone()).or_default();
                ts.repos = list.iter().map(|r| r.id.to_string()).collect();
                ts.listed_at = Some(now);
                out.repos.insert(key, list);
            }
            Err(e) => out.errors.push((key, e.to_string())),
        }
    }
    if let Some(p) = ws.config.clone_protocol {
        out.protocol = p;
    }
    ws.resolve_flags();
    out
}

/// After running clone jobs: add successful clones to the workspace and flag
/// them as new. Returns the paths added.
pub fn record_clones(
    ws: &mut Workspace,
    jobs: &[Job],
    outcomes: &[Option<crate::ops::OpOutcome>],
) -> Vec<PathBuf> {
    let mut added = vec![];
    for (j, o) in jobs.iter().zip(outcomes) {
        if !matches!(j.op, Op::Clone { .. }) || !o.as_ref().is_some_and(|o| o.success) {
            continue;
        }
        let path = std::fs::canonicalize(&j.path).unwrap_or_else(|_| j.path.clone());
        ws.add(path.clone());
        if let Some(e) = ws.get(&path) {
            ws.state.new.insert(e.key.clone());
        }
        added.push(path);
    }
    ws.resolve_flags();
    added
}
