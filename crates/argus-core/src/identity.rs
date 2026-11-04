//! Forge-level identity of a repository, derived from its remote URL.

use std::fmt;

use serde::{Deserialize, Serialize};

/// `host/owner/name`, e.g. `github.com/rust-lang/cargo`.
///
/// `owner` may contain slashes for forges with nested groups (GitLab).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoId {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoId {
    pub fn new(host: impl Into<String>, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self { host: host.into().to_lowercase(), owner: owner.into(), name: name.into() }
    }

    /// `owner/name`
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Parse a git remote URL. Supports scp-like (`git@host:o/n.git`),
    /// `ssh://`, `git://`, `http(s)://` forms. Returns `None` for local paths
    /// and URLs without at least an owner and a name.
    pub fn from_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some((_, rest)) = url.split_once("://") {
            if url.starts_with("file://") {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            let host = strip_port(host);
            (host, path)
        } else if let Some((left, path)) = url.split_once(':') {
            // scp-like syntax requires no slash before the colon.
            if left.contains('/') || left.is_empty() || left.len() == 1 {
                // len 1: Windows drive letter "C:\..."
                return None;
            }
            let host = left.rsplit('@').next()?;
            (host, path)
        } else {
            return None;
        };
        if host.is_empty() {
            return None;
        }
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
        let (owner, name) = path.rsplit_once('/')?;
        let owner = owner.trim_start_matches('~').trim_matches('/');
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self::new(host, owner, name))
    }
}

fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        // IPv6 literal
        return host.split(']').next().map(|h| h.trim_start_matches('[')).unwrap_or(host);
    }
    host.split(':').next().unwrap_or(host)
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.host, self.owner, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Option<String> {
        RepoId::from_url(s).map(|i| i.to_string())
    }

    #[test]
    fn parses_common_forms() {
        let want = Some("github.com/acme/widget".to_string());
        assert_eq!(id("git@github.com:acme/widget.git"), want);
        assert_eq!(id("git@github.com:acme/widget"), want);
        assert_eq!(id("https://github.com/acme/widget.git"), want);
        assert_eq!(id("https://github.com/acme/widget"), want);
        assert_eq!(id("https://github.com/acme/widget/"), want);
        assert_eq!(id("https://user:tok@github.com/acme/widget.git"), want);
        assert_eq!(id("ssh://git@github.com/acme/widget.git"), want);
        assert_eq!(id("ssh://git@github.com:22/acme/widget.git"), want);
        assert_eq!(id("git://github.com/acme/widget"), want);
        assert_eq!(id("  git@GitHub.com:acme/widget.git\n"), want);
    }

    #[test]
    fn nested_groups_go_into_owner() {
        assert_eq!(id("git@gitlab.com:group/sub/proj.git"), Some("gitlab.com/group/sub/proj".into()));
    }

    #[test]
    fn rejects_local_and_incomplete() {
        assert_eq!(id("/srv/git/widget.git"), None);
        assert_eq!(id("../widget"), None);
        assert_eq!(id("file:///srv/git/acme/widget.git"), None);
        assert_eq!(id("C:\\repos\\widget"), None);
        assert_eq!(id("https://github.com/widget"), None);
        assert_eq!(id("git@github.com:widget.git"), None);
        assert_eq!(id(""), None);
        assert_eq!(id("https:///acme/widget"), None);
    }
}
