//! Forge implementations for argus: GitHub, and a directory of repos.
//!
//! Credentials are reused, never stored: `GH_TOKEN` / `GITHUB_TOKEN`, else
//! `gh auth token`.

pub mod dir;
pub mod github;

use argus_core::forge::Forge;

/// Construct the forge for a `[[track]]` entry.
pub fn for_track(kind: &str, host: &str) -> Result<Box<dyn Forge>, argus_core::forge::ForgeError> {
    match kind {
        "github" => Ok(Box::new(github::GitHub::new(host))),
        argus_core::forge::DIR => Ok(Box::new(dir::Dir::new(host))),
        other => Err(argus_core::forge::ForgeError::Other(format!(
            "unsupported forge kind {other:?}"
        ))),
    }
}
