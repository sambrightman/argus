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
}
