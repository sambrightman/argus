//! Forge abstraction (GitHub now; GitLab/Gitea/… later). Implementations live
//! in other crates so that core has no HTTP dependencies.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity::RepoId;

/// The forge kind that lists a directory: `host` is its path, holding a
/// directory per owner, holding the repos (`<host>/<owner>/<name>[.git]`).
pub const DIR: &str = "dir";

/// Identities for repos cloned from directory forges, whose remotes are
/// paths: `<host>/<owner>/<name>`, as the forge lists them. Other remotes
/// as [`RepoId::from_url`].
#[derive(Debug, Clone, Default)]
pub struct LocalForges(Vec<(String, PathBuf)>);

impl LocalForges {
    /// From each directory forge's `host`, as written in its `[[track]]`.
    pub fn new<'h>(hosts: impl IntoIterator<Item = &'h str>) -> Self {
        Self(
            hosts
                .into_iter()
                .map(|h| (h.to_string(), real(&dir_path(h))))
                .collect(),
        )
    }

    pub fn id(&self, url: &str) -> Option<RepoId> {
        RepoId::from_url(url).or_else(|| self.local(url))
    }

    fn local(&self, url: &str) -> Option<RepoId> {
        let path = real(Path::new(url.strip_prefix("file://").unwrap_or(url)));
        let owner_dir = path.parent()?;
        let (host, _) = self
            .0
            .iter()
            .find(|(_, base)| owner_dir.parent() == Some(base.as_path()))?;
        let name = path.file_name()?.to_str()?;
        let owner = owner_dir.file_name()?.to_str()?;
        Some(RepoId::new(
            host,
            owner,
            name.strip_suffix(".git").unwrap_or(name),
        ))
    }
}

/// A directory forge's directory, from its `host`.
pub fn dir_path(host: &str) -> PathBuf {
    crate::paths::expand_tilde(Path::new(host))
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRepo {
    pub id: RepoId,
    pub ssh_url: String,
    pub https_url: String,
    pub archived: bool,
    pub fork: bool,
    pub default_branch: Option<String>,
    pub description: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ssh,
    #[default]
    Https,
}

impl RemoteRepo {
    pub fn clone_url(&self, p: Protocol) -> &str {
        match p {
            Protocol::Ssh => &self.ssh_url,
            Protocol::Https => &self.https_url,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("authentication: {0}")]
    Auth(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

/// What a forge says of a repo beyond what git has: for showing, never
/// what argus decides by.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RepoInfo {
    pub open_issues: usize,
    pub open_pulls: usize,
    /// A pull request from each of the branches asked about that has one:
    /// its open one, else its newest.
    pub pulls: Vec<PullRequest>,
}

impl RepoInfo {
    pub fn pull(&self, branch: &str) -> Option<&PullRequest> {
        self.pulls.iter().find(|p| p.branch == branch)
    }
}

/// The branches of a repo worth asking a forge about: the local ones
/// that aren't the default branch, the checked-out one first (a forge
/// may ask about only so many).
pub fn branches_to_ask(s: &crate::status::RepoStatus) -> Vec<String> {
    let mut v: Vec<&crate::status::BranchInfo> = s
        .branches
        .iter()
        .filter(|b| s.default_branch.as_deref() != Some(b.name.as_str()))
        .collect();
    v.sort_by_key(|b| !b.is_head);
    v.into_iter().map(|b| b.name.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    /// The local branch it's from.
    pub branch: String,
    pub number: u64,
    pub title: String,
    pub state: PullState,
    pub draft: bool,
    pub url: String,
    pub review: Option<Review>,
    pub checks: Option<Checks>,
    /// The repo it asks to be merged into, when that's another one (the
    /// repo this one is a fork of): `owner/name`.
    pub into: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    Approved,
    ChangesRequested,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Checks {
    Passing,
    Failing,
    Pending,
}

impl PullRequest {
    /// `#12 open, approved, checks failing`: reviews and checks only
    /// while it's open, when they still matter.
    pub fn summary(&self) -> String {
        let mut v = vec![format!(
            "#{} {}",
            self.number,
            match (self.state, self.draft) {
                (PullState::Open, true) => "draft",
                (PullState::Open, false) => "open",
                (PullState::Merged, _) => "merged",
                (PullState::Closed, _) => "closed unmerged",
            }
        )];
        if self.state == PullState::Open {
            v.extend(self.review.map(|r| match r {
                Review::Approved => "approved".to_string(),
                Review::ChangesRequested => "changes requested".into(),
                Review::Required => "review required".into(),
            }));
            v.extend(self.checks.map(|c| match c {
                Checks::Passing => "checks passing".to_string(),
                Checks::Failing => "checks failing".into(),
                Checks::Pending => "checks running".into(),
            }));
        }
        let mut s = v.join(", ");
        if let Some(into) = &self.into {
            s += &format!(" into {into}");
        }
        s
    }
}

pub trait Forge: Send + Sync {
    /// e.g. `github`
    fn kind(&self) -> &str;
    fn host(&self) -> &str;
    /// Repositories of an org or user, optionally restricted to a team.
    fn list(&self, owner: &str, team: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError>;
    /// Preferred clone protocol (e.g. from `gh config get git_protocol`).
    fn protocol(&self) -> Protocol {
        Protocol::Https
    }
    /// What the forge says of the repo, and of pull requests from
    /// `branches`; `None` from a kind of forge with nothing to say.
    fn info(&self, _id: &RepoId, _branches: &[String]) -> Result<Option<RepoInfo>, ForgeError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_in_a_directory_forge_has_an_identity() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("forge");
        for repo in ["acme/w.git", "acme/plain"] {
            std::fs::create_dir_all(base.join(repo)).unwrap();
        }
        let host = base.to_str().unwrap();
        let f = LocalForges::new([host]);
        let id = |url: &str| f.id(url).map(|i| i.to_string());
        let want = |name| Some(format!("{}/acme/{name}", host.to_lowercase()));
        assert_eq!(id(&format!("{host}/acme/w.git")), want("w"));
        assert_eq!(id(&format!("file://{host}/acme/w.git")), want("w"));
        assert_eq!(id(&format!("{host}/acme/plain")), want("plain"));
        // inverse: elsewhere, or not an owner's directory, it's a path
        assert_eq!(id(&format!("{host}/acme")), None);
        assert_eq!(id(&d.path().join("acme/w.git").display().to_string()), None);
        assert_eq!(
            LocalForges::default().id(&format!("{host}/acme/w.git")),
            None
        );
        // and forge URLs as ever
        assert_eq!(
            id("git@github.com:acme/w.git").as_deref(),
            Some("github.com/acme/w")
        );
    }

    #[test]
    fn a_forge_that_doesnt_say_clones_over_https() {
        struct Quiet;
        impl Forge for Quiet {
            fn kind(&self) -> &str {
                "quiet"
            }
            fn host(&self) -> &str {
                "example.com"
            }
            fn list(&self, _: &str, _: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError> {
                Ok(vec![])
            }
        }
        assert_eq!(Quiet.protocol(), Protocol::Https);
    }

    fn pr(state: PullState, draft: bool) -> PullRequest {
        PullRequest {
            branch: "feat".into(),
            number: 12,
            title: "Add it".into(),
            state,
            draft,
            url: "https://github.com/acme/w/pull/12".into(),
            review: Some(Review::Approved),
            checks: Some(Checks::Failing),
            into: None,
        }
    }

    #[test]
    fn a_pull_request_is_summed_up_by_state_then_review_and_checks_while_open() {
        assert_eq!(
            pr(PullState::Open, false).summary(),
            "#12 open, approved, checks failing"
        );
        let mut p = pr(PullState::Open, true);
        (p.review, p.checks) = (Some(Review::ChangesRequested), Some(Checks::Pending));
        assert_eq!(p.summary(), "#12 draft, changes requested, checks running");
        (p.review, p.checks) = (Some(Review::Required), Some(Checks::Passing));
        assert_eq!(p.summary(), "#12 draft, review required, checks passing");
        (p.review, p.checks) = (None, None);
        assert_eq!(p.summary(), "#12 draft");
        // once it's over, only how it ended
        assert_eq!(pr(PullState::Merged, false).summary(), "#12 merged");
        assert_eq!(pr(PullState::Closed, true).summary(), "#12 closed unmerged");
        // to another repo: said
        let mut p = pr(PullState::Merged, false);
        p.into = Some("up/w".into());
        assert_eq!(p.summary(), "#12 merged into up/w");
    }

    #[test]
    fn info_finds_a_branchs_pull_request_and_a_forge_may_have_none() {
        let info = RepoInfo {
            pulls: vec![pr(PullState::Open, false)],
            ..Default::default()
        };
        assert_eq!(info.pull("feat").map(|p| p.number), Some(12));
        assert!(info.pull("other").is_none());
        struct Plain;
        impl Forge for Plain {
            fn kind(&self) -> &str {
                "plain"
            }
            fn host(&self) -> &str {
                "h"
            }
            fn list(&self, _: &str, _: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError> {
                Ok(vec![])
            }
        }
        let id = RepoId::new("h", "o", "n");
        assert_eq!(Plain.info(&id, &[]).unwrap(), None);
    }

    #[test]
    fn the_branches_asked_about_leave_out_the_default_and_start_with_the_current() {
        use crate::testkit::{Sandbox, git};
        let sb = Sandbox::new();
        let r = sb.init("r");
        for b in ["a", "b", "c"] {
            git(&r, &["branch", "-q", b]);
        }
        let ask = || branches_to_ask(&crate::status::status(&r, Default::default()).unwrap());
        assert_eq!(ask(), ["a", "b", "c"]);
        git(&r, &["switch", "-q", "b"]);
        assert_eq!(ask(), ["b", "a", "c"]);
    }
}
