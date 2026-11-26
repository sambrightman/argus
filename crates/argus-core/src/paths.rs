//! Standard file locations. XDG layout on every platform (including macOS),
//! overridable with `ARGUS_CONFIG` / `ARGUS_STATE` (the cache only by
//! `XDG_CACHE_HOME`).

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// An environment lookup: the process's, or a test's.
type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn real(k: &str) -> Option<OsString> {
    env::var_os(k)
}

pub fn home() -> Option<PathBuf> {
    home_in(&real)
}

fn home_in(env: Env) -> Option<PathBuf> {
    env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else {
        return p.to_path_buf();
    };
    match home() {
        Some(h) => h.join(rest),
        None => p.to_path_buf(),
    }
}

/// Inverse of [`expand_tilde`] for display.
pub fn contract_tilde(p: &Path) -> String {
    match home() {
        Some(h) if p.starts_with(&h) && h != Path::new("/") => {
            format!("~/{}", p.strip_prefix(&h).unwrap().display())
                .trim_end_matches('/')
                .to_string()
        }
        _ => p.display().to_string(),
    }
}

fn xdg(env: Env, var: &str, fallback: &str) -> PathBuf {
    env(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_in(env).map(|h| h.join(fallback)))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_file() -> PathBuf {
    config_file_in(&real)
}

fn config_file_in(env: Env) -> PathBuf {
    env("ARGUS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg(env, "XDG_CONFIG_HOME", ".config").join("argus/config.toml"))
}

pub fn state_file() -> PathBuf {
    state_file_in(&real)
}

fn state_file_in(env: Env) -> PathBuf {
    env("ARGUS_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg(env, "XDG_STATE_HOME", ".local/state").join("argus/state.toml"))
}

/// Answers worked out once for a repo and kept between runs, a file for
/// each repo (`$XDG_CACHE_HOME/argus/repos`, see [`crate::remember`]).
pub fn repos_cache() -> PathBuf {
    repos_cache_in(&real)
}

fn repos_cache_in(env: Env) -> PathBuf {
    xdg(env, "XDG_CACHE_HOME", ".cache").join("argus/repos")
}

/// Replace `path`'s contents with `text` in one step (a temporary file
/// beside it, renamed over it), making its folder if need be. A symlink
/// stays one: the file it points at is the one replaced.
pub fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let mut target = path.to_path_buf();
    // (a link to a link, and so on; a loop ends at the first rename)
    for _ in 0..16 {
        let Ok(to) = std::fs::read_link(&target) else {
            break;
        };
        target = match target.parent() {
            Some(d) => d.join(to),
            None => to,
        };
    }
    if let Some(d) = target.parent() {
        std::fs::create_dir_all(d)?;
    }
    // a name of this write's own (two at once, in one process or two,
    // never share one), made new and, from the first, as private as the
    // file it replaces
    static WRITES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = target.with_extension(format!("tmp.{}.{n}", std::process::id()));
    let mut new = std::fs::OpenOptions::new();
    new.write(true).create_new(true);
    #[cfg(unix)]
    if let Ok(was) = std::fs::metadata(&target) {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        new.mode(was.permissions().mode());
    }
    let mut file = new.open(&tmp)?;
    let written = std::io::Write::write_all(&mut file, text.as_bytes())
        .and_then(|()| std::fs::rename(&tmp, &target));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_roundtrip() {
        let Some(h) = home() else { return };
        let p = expand_tilde(Path::new("~/x/y"));
        assert_eq!(p, h.join("x/y"));
        assert_eq!(contract_tilde(&p), "~/x/y");
    }

    /// An environment holding just `vars`.
    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.into())
    }

    #[test]
    fn files_go_where_the_environment_says() {
        let p = PathBuf::from;
        // the override, else XDG, else under HOME, else here
        let all = env(&[
            ("ARGUS_CONFIG", "/c.toml"),
            ("ARGUS_STATE", "/s.toml"),
            ("XDG_CONFIG_HOME", "/xc"),
            ("XDG_STATE_HOME", "/xs"),
            ("XDG_CACHE_HOME", "/xk"),
            ("HOME", "/h"),
        ]);
        assert_eq!(repos_cache_in(&all), p("/xk/argus/repos"));
        assert_eq!(
            (config_file_in(&all), state_file_in(&all)),
            (p("/c.toml"), p("/s.toml"))
        );
        let xdg = env(&[
            ("XDG_CONFIG_HOME", "/xc"),
            ("XDG_STATE_HOME", "/xs"),
            ("HOME", "/h"),
        ]);
        assert_eq!(config_file_in(&xdg), p("/xc/argus/config.toml"));
        assert_eq!(state_file_in(&xdg), p("/xs/argus/state.toml"));
        // an empty XDG variable or HOME counts as unset
        let under_home = env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")]);
        assert_eq!(
            config_file_in(&under_home),
            p("/h/.config/argus/config.toml")
        );
        assert_eq!(
            state_file_in(&under_home),
            p("/h/.local/state/argus/state.toml")
        );
        assert_eq!(repos_cache_in(&under_home), p("/h/.cache/argus/repos"));
        assert_eq!(home_in(&under_home), Some(p("/h")));
        let none = env(&[("HOME", "")]);
        assert_eq!(home_in(&none), None);
        assert_eq!(config_file_in(&none), p("./argus/config.toml"));
        assert_eq!(home_in(&env(&[])), None);
        // and the public ones read the process's environment
        assert_eq!(real("HOME"), env::var_os("HOME"));
        assert!(
            real("HOME").is_some_and(|h| !h.is_empty()),
            "tests run with a HOME"
        );
        assert_eq!(real("ARGUS_NO_SUCH_VARIABLE"), None);
        assert_eq!(config_file(), config_file_in(&real));
        assert_eq!(state_file(), state_file_in(&real));
        assert_eq!(repos_cache(), repos_cache_in(&real));
        assert_eq!(home(), home_in(&real));
    }

    #[test]
    fn non_tilde_untouched() {
        assert_eq!(expand_tilde(Path::new("/a/~/b")), PathBuf::from("/a/~/b"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
        assert_eq!(contract_tilde(Path::new("/elsewhere")), "/elsewhere");
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_its_permissions_and_a_failed_write_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let private = d.path().join("state.toml");
        std::fs::write(&private, "old").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomically(&private, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&private).unwrap(), "new");
        assert_eq!(mode(&private), 0o600);
        // inverse: one others could read stays readable
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomically(&private, "newer").unwrap();
        assert_eq!(mode(&private), 0o644);
        // many at once: each its own temporary file, all of them written
        let shared = d.path().join("shared.toml");
        std::thread::scope(|s| {
            for n in 0..16 {
                let shared = &shared;
                s.spawn(move || write_atomically(shared, &format!("writer {n}")).unwrap());
            }
        });
        assert!(
            std::fs::read_to_string(&shared)
                .unwrap()
                .starts_with("writer ")
        );
        // a folder where the file should be: refused, no temporary file left
        let taken = d.path().join("dir.toml");
        std::fs::create_dir_all(taken.join("inside")).unwrap();
        assert!(write_atomically(&taken, "x").is_err());
        let left: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[cfg(unix)]
    #[test]
    fn writing_through_a_symlink_replaces_what_it_points_at() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("dotfiles/argus.toml");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "old").unwrap();
        // a relative link, through a second link
        std::fs::create_dir_all(d.path().join("cfg")).unwrap();
        let mid = d.path().join("cfg/mid.toml");
        std::os::unix::fs::symlink("../dotfiles/argus.toml", &mid).unwrap();
        let link = d.path().join("cfg/config.toml");
        std::os::unix::fs::symlink(&mid, &link).unwrap();
        write_atomically(&link, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        for l in [&link, &mid] {
            assert!(l.symlink_metadata().unwrap().is_symlink(), "{l:?}");
        }
        // nothing left beside either
        for dir in ["cfg", "dotfiles"] {
            let names: Vec<_> = std::fs::read_dir(d.path().join(dir))
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.contains("tmp"))
                .collect();
            assert!(names.is_empty(), "{names:?}");
        }
        // a link to nothing yet: the file appears where it points
        let dangling = d.path().join("cfg/later.toml");
        std::os::unix::fs::symlink("../dotfiles/later.toml", &dangling).unwrap();
        write_atomically(&dangling, "x").unwrap();
        assert!(dangling.symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            std::fs::read_to_string(d.path().join("dotfiles/later.toml")).unwrap(),
            "x"
        );
        // inverse: a plain file in a folder not there yet
        let plain = d.path().join("new/dir/f.toml");
        write_atomically(&plain, "p").unwrap();
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "p");
        assert!(!plain.symlink_metadata().unwrap().is_symlink());
    }
}
