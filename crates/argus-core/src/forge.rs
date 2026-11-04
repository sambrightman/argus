//! Forge abstraction (GitHub now; GitLab/Gitea/… later). Implementations live
//! in other crates so that core has no HTTP dependencies.

use serde::{Deserialize, Serialize};

use crate::identity::RepoId;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
}
