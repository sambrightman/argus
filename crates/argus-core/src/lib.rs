//! argus-core: multi-repository status, cleanup and pulls, as a library.
//!
//! Reads go through gix in-process; mutations through the `git` CLI (see
//! `DESIGN.md` for why). Nothing here does terminal I/O.

pub mod attention;
pub mod badges;
pub mod config;
pub mod configfile;
pub mod diff;
pub mod discover;
pub mod fix;
pub mod forge;
pub mod gitignore;
pub mod history;
pub mod icons;
pub mod identity;
pub mod isolation;
pub mod ops;
pub mod paths;
pub mod pull;
pub mod rules;
pub mod runner;
pub mod state;
pub mod status;
pub mod workspace;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

#[cfg(feature = "demo")]
pub mod demo;

pub use config::Config;
pub use identity::RepoId;
pub use state::State;
pub use workspace::{Entry, ViewFilter, Workspace};
