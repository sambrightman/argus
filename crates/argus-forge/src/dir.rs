//! A directory as a forge: `<host>/<owner>/<name>[.git]`, each a bare or
//! plain repo. For mirrors and file servers; also how tests run a forge
//! offline.

use std::path::{Path, PathBuf};

use argus_core::forge::{self, Forge, ForgeError, RemoteRepo};
use argus_core::identity::RepoId;

pub struct Dir {
    host: String,
    path: PathBuf,
}

impl Dir {
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_string(),
            path: forge::dir_path(host),
        }
    }
}

impl Forge for Dir {
    fn kind(&self) -> &str {
        forge::DIR
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn list(&self, owner: &str, team: Option<&str>) -> Result<Vec<RemoteRepo>, ForgeError> {
        if team.is_some() {
            return Err(ForgeError::Other("a directory forge has no teams".into()));
        }
        let dir = self.path.join(owner);
        let entries = std::fs::read_dir(&dir).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ForgeError::NotFound(dir.display().to_string()),
            _ => ForgeError::Other(format!("{}: {e}", dir.display())),
        })?;
        let mut repos: Vec<RemoteRepo> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let file = e.file_name().into_string().ok()?;
                let name = file.strip_suffix(".git").unwrap_or(&file);
                let head = head_branch(&path)?;
                let url = path.display().to_string();
                Some(RemoteRepo {
                    id: RepoId::new(&self.host, owner, name),
                    ssh_url: url.clone(),
                    https_url: url,
                    archived: false,
                    fork: false,
                    default_branch: Some(head),
                    description: None,
                })
            })
            .collect();
        repos.sort_by(|a, b| a.id.name.cmp(&b.id.name));
        Ok(repos)
    }
}

/// The branch HEAD names, if `path` is a repo: bare, or with a `.git` dir.
fn head_branch(path: &Path) -> Option<String> {
    let git = path.join(".git");
    let git = if git.is_dir() {
        git
    } else {
        path.to_path_buf()
    };
    if !git.join("objects").is_dir() {
        return None;
    }
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    Some(branch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argus_core::testkit::{Sandbox, git};

    #[test]
    fn lists_an_owners_repos_bare_or_plain() {
        let sb = Sandbox::new();
        sb.remote("acme", "w");
        let plain = sb.init("forge/acme/plain");
        git(&plain, &["branch", "-q", "-m", "trunk"]);
        std::fs::create_dir_all(sb.path("forge/acme/not-a-repo")).unwrap();
        std::fs::write(sb.path("forge/acme/file"), "").unwrap();
        sb.remote("other", "x");
        let host = sb.path("forge").display().to_string();
        let repos = Dir::new(&host).list("acme", None).unwrap();
        let got: Vec<_> = repos
            .iter()
            .map(|r| (r.id.to_string(), r.default_branch.clone().unwrap()))
            .collect();
        let id = |n: &str| format!("{}/acme/{n}", host.to_lowercase());
        assert_eq!(
            got,
            [(id("plain"), "trunk".into()), (id("w"), "main".into())]
        );
        assert_eq!(
            repos[1].clone_url(forge::Protocol::Ssh),
            sb.path("forge/acme/w.git").display().to_string()
        );
        // what a clone's remote says is what the listing says
        let ids = forge::LocalForges::new([host.as_str()]);
        assert_eq!(ids.id(&repos[1].https_url), Some(repos[1].id.clone()));
    }

    #[test]
    fn a_missing_owner_or_a_team_is_an_error() {
        let sb = Sandbox::new();
        sb.remote("acme", "w");
        let d = Dir::new(sb.path("forge").to_str().unwrap());
        assert!(matches!(
            d.list("nobody", None),
            Err(ForgeError::NotFound(_))
        ));
        assert!(matches!(
            d.list("acme", Some("core")),
            Err(ForgeError::Other(_))
        ));
        assert_eq!((d.kind(), d.list("acme", None).unwrap().len()), ("dir", 1));
        assert_eq!(d.host(), sb.path("forge").to_str().unwrap());
    }
}
