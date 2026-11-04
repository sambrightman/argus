//! Process-wide isolation from the user's git configuration, for tests only.
//!
//! When set, `git` subprocesses get the given environment (e.g. a private
//! `GIT_CONFIG_GLOBAL`) and gix opens repositories ignoring global/system
//! config. Production code never sets this.

use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;

static GIT_ENV: OnceLock<Vec<(OsString, OsString)>> = OnceLock::new();

#[doc(hidden)]
pub fn isolate(env: Vec<(OsString, OsString)>) {
    let _ = GIT_ENV.set(env);
}

pub(crate) fn git_env() -> &'static [(OsString, OsString)] {
    GIT_ENV.get().map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn open(path: &Path) -> gix::Result<gix::Repository> {
    if GIT_ENV.get().is_some() {
        gix::open_opts(path, gix::open::Options::isolated())
    } else {
        gix::open(path)
    }
}
