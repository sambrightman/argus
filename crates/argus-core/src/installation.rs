//! Git's installation config (Homebrew's `/opt/homebrew/etc/gitconfig`,
//! Apple's under the developer tools): repos are opened with it, as git
//! reads it. Only git knows where it is, and asking means running `git`,
//! which every repo opened meanwhile waits for (80ms or more when `git`
//! isn't cached). So the answer is remembered between runs and repos are
//! opened with it at once, while git is asked again alongside: if it says
//! otherwise (another git first on `PATH`, `xcode-select`, an upgrade),
//! [`settle`] says so, and what was read is read again.

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, RwLock};
use std::thread::JoinHandle;

/// What git says of its installation config: the file, if any, and
/// whether it's in the system scope (which `GIT_CONFIG_SYSTEM` replaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub path: Option<PathBuf>,
    pub system: bool,
}

struct Known {
    /// What repos are opened with: remembered, or settled. None: not
    /// known yet, so gix asks git itself as it opens one.
    used: RwLock<Option<Installation>>,
    /// Git being asked, until [`settle`] takes the answer.
    asking: Mutex<Option<JoinHandle<Installation>>>,
}

static KNOWN: LazyLock<Known> = LazyLock::new(|| Known {
    used: RwLock::new(remembered()),
    asking: Mutex::new(Some(std::thread::spawn(ask))),
});

fn ask() -> Installation {
    crate::trace::timed(
        "git config",
        |i: &Installation| match &i.path {
            Some(p) => format!("git's installation config: {}", p.display()),
            None => "git's installation config: none".into(),
        },
        || Installation {
            path: gix::path::env::installation_config().map(Into::into),
            system: gix::path::env::installation_config_is_system(),
        },
    )
}

/// Start asking git now, to have the answer sooner (repos opened later
/// start it anyway).
pub fn start() {
    LazyLock::force(&KNOWN);
}

/// The installation config file repos are opened with, if it's known:
/// none if there's none.
pub(crate) fn file() -> Option<Option<PathBuf>> {
    let used = KNOWN
        .used
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;
    Some(used.path.map(|path| {
        // (as gix does: GIT_CONFIG_SYSTEM replaces a system-scoped one)
        used.system
            .then(|| std::env::var_os("GIT_CONFIG_SYSTEM"))
            .flatten()
            .map_or(path, PathBuf::from)
    }))
}

/// Set `options` to open a repo with git's installation config as known.
pub(crate) fn apply(mut options: gix::open::Options) -> gix::open::Options {
    options.permissions.config.git_binary = true;
    match file() {
        Some(Some(path)) => options = options.git_installation_config_path(path),
        Some(None) => options.permissions.config.git_binary = false,
        None => {}
    }
    options
}

/// Wait for git's answer, and remember it. Whether repos were opened with
/// another, so what was read from them should be read again.
pub fn settle() -> bool {
    let asking = KNOWN
        .asking
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(asking) = asking else {
        return false;
    };
    let Ok(asked) = asking.join() else {
        return false;
    };
    let mut used = KNOWN.used.write().unwrap_or_else(|e| e.into_inner());
    // (with nothing known, gix asked git itself: the same answer)
    let changed = used.as_ref().is_some_and(|u| *u != asked);
    if used.as_ref() != Some(&asked) {
        remember(&asked);
    }
    *used = Some(asked);
    changed
}

/// Where the answer is kept: beside the state file.
fn kept_at() -> PathBuf {
    crate::paths::state_file().with_file_name("git-installation")
}

fn remembered() -> Option<Installation> {
    parse(&std::fs::read_to_string(kept_at()).ok()?)
}

fn remember(i: &Installation) {
    let _ = crate::paths::write_atomically(&kept_at(), &format(i));
}

/// One line: `none`, or the scope (`system` or `other`), a tab, the path.
fn format(i: &Installation) -> String {
    match &i.path {
        None => "none\n".into(),
        Some(p) => {
            let scope = if i.system { "system" } else { "other" };
            format!("{scope}\t{}\n", p.display())
        }
    }
}

fn parse(s: &str) -> Option<Installation> {
    let line = s.strip_suffix('\n').unwrap_or(s);
    if line == "none" {
        return Some(Installation {
            path: None,
            system: false,
        });
    }
    let (scope, path) = line.split_once('\t')?;
    let system = match scope {
        "system" => true,
        "other" => false,
        _ => return None,
    };
    (!path.is_empty()).then(|| Installation {
        path: Some(path.into()),
        system,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_kept_as_one_line_and_read_back() {
        for i in [
            Installation {
                path: None,
                system: false,
            },
            Installation {
                path: Some("/opt/homebrew/etc/gitconfig".into()),
                system: true,
            },
            Installation {
                path: Some(
                    "/Library/Developer/CommandLineTools/usr/share/git-core/gitconfig".into(),
                ),
                system: false,
            },
        ] {
            assert_eq!(parse(&format(&i)), Some(i));
        }
        // inverse: anything else is no answer
        for bad in ["", "system\t", "other", "user\t/x", "nothing"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
