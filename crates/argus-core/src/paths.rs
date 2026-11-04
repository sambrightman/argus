//! Standard file locations. XDG layout on every platform (including macOS),
//! overridable with `ARGUS_CONFIG` / `ARGUS_STATE`.

use std::env;
use std::path::{Path, PathBuf};

pub fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
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

fn xdg(var: &str, fallback: &str) -> PathBuf {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(fallback)))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_file() -> PathBuf {
    env::var_os("ARGUS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_CONFIG_HOME", ".config").join("argus/config.toml"))
}

pub fn state_file() -> PathBuf {
    env::var_os("ARGUS_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join("argus/state.toml"))
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

    #[test]
    fn non_tilde_untouched() {
        assert_eq!(expand_tilde(Path::new("/a/~/b")), PathBuf::from("/a/~/b"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
        assert_eq!(contract_tilde(Path::new("/elsewhere")), "/elsewhere");
    }
}
