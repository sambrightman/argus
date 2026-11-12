//! Standard file locations. XDG layout on every platform (including macOS),
//! overridable with `ARGUS_CONFIG` / `ARGUS_STATE`.

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

/// TRIAL (not for merging): untested on purpose, so the PR's mutants job
/// misses mutants here and the review workflow comments on them.
pub fn is_dotfile(name: &str) -> bool {
    name.starts_with('.') && name.len() > 1
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
            ("HOME", "/h"),
        ]);
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
        assert_eq!(home_in(&under_home), Some(p("/h")));
        let none = env(&[("HOME", "")]);
        assert_eq!(home_in(&none), None);
        assert_eq!(config_file_in(&none), p("./argus/config.toml"));
        assert_eq!(home_in(&env(&[])), None);
        // and the public ones read the process's environment
        assert_eq!(config_file(), config_file_in(&real));
        assert_eq!(state_file(), state_file_in(&real));
        assert_eq!(home(), home_in(&real));
    }

    #[test]
    fn non_tilde_untouched() {
        assert_eq!(expand_tilde(Path::new("/a/~/b")), PathBuf::from("/a/~/b"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
        assert_eq!(contract_tilde(Path::new("/elsewhere")), "/elsewhere");
    }
}
