//! Machine-written state: marks made from the UI, snoozes, forge listings and
//! "new" flags. Saved atomically (write temp + rename).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Marks set interactively. Keys are repo keys (identity string, or path for
/// repos without a forge identity). They override config rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Marks {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favourite: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<bool>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        *self == Marks::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackState {
    /// Identities (`host/owner/name`) last listed by the forge.
    pub repos: BTreeSet<String>,
    pub listed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub marks: BTreeMap<String, Marks>,
    /// Temporarily excluded until the timestamp.
    pub snoozed: BTreeMap<String, Timestamp>,
    /// Repos (by key) cloned or discovered by argus and not yet acknowledged.
    pub new: BTreeSet<String>,
    pub tracks: BTreeMap<String, TrackState>,
    pub last_sync: Option<Timestamp>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("state {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("state: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl State {
    pub fn load(path: &Path) -> Result<Self, StateError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|source| StateError::Parse {
                path: path.into(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(StateError::Io {
                path: path.into(),
                source,
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let io = |source| StateError::Io {
            path: path.into(),
            source,
        };
        let s = toml::to_string(self)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, s).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }

    pub fn is_snoozed(&self, key: &str, now: Timestamp) -> bool {
        self.snoozed.get(key).is_some_and(|until| *until > now)
    }

    /// Drop expired snoozes; returns whether anything changed.
    pub fn expire_snoozes(&mut self, now: Timestamp) -> bool {
        let before = self.snoozed.len();
        self.snoozed.retain(|_, until| *until > now);
        before != self.snoozed.len()
    }

    pub fn mark(&mut self, key: &str, f: impl FnOnce(&mut Marks)) {
        let m = self.marks.entry(key.to_string()).or_default();
        f(m);
        if m.is_empty() {
            self.marks.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_missing() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/state.toml");
        assert_eq!(State::load(&p).unwrap(), State::default());
        let mut s = State::default();
        s.mark("github.com/a/b", |m| m.favourite = Some(true));
        s.snoozed
            .insert("x".into(), "2030-01-01T00:00:00Z".parse().unwrap());
        s.new.insert("/p".into());
        s.save(&p).unwrap();
        assert_eq!(State::load(&p).unwrap(), s);
        assert!(
            !p.with_extension(format!("tmp.{}", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn snooze_expiry() {
        let mut s = State::default();
        let now: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
        s.snoozed
            .insert("past".into(), "2026-05-01T00:00:00Z".parse().unwrap());
        s.snoozed
            .insert("future".into(), "2026-07-01T00:00:00Z".parse().unwrap());
        assert!(!s.is_snoozed("past", now));
        assert!(s.is_snoozed("future", now));
        assert!(!s.is_snoozed("unknown", now));
        assert!(s.expire_snoozes(now));
        assert!(!s.expire_snoozes(now), "second expiry is a no-op");
        assert_eq!(s.snoozed.keys().collect::<Vec<_>>(), vec!["future"]);
    }

    #[test]
    fn clearing_marks_removes_entry() {
        let mut s = State::default();
        s.mark("k", |m| m.sync = Some(true));
        assert!(s.marks.contains_key("k"));
        s.mark("k", |m| m.sync = None);
        assert!(!s.marks.contains_key("k"));
    }
}

/// Parse a friendly duration like `90m`, `12h`, `3d`, `2w` into seconds.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok()?;
    let mult = match unit.trim() {
        "m" | "min" | "mins" => 60,
        "h" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 604_800,
        _ => return None,
    };
    (n > 0).then(|| n.checked_mul(mult)).flatten()
}

#[cfg(test)]
mod duration_tests {
    use super::parse_duration;

    #[test]
    fn parses_units_and_rejects_garbage() {
        assert_eq!(parse_duration("90m"), Some(5400));
        assert_eq!(parse_duration("3d"), Some(259_200));
        assert_eq!(parse_duration("2 weeks"), Some(1_209_600));
        assert_eq!(parse_duration("0d"), None);
        assert_eq!(parse_duration("3"), None);
        assert_eq!(parse_duration("d"), None);
        assert_eq!(parse_duration("3y"), None);
        assert_eq!(parse_duration("-3d"), None);
        assert_eq!(parse_duration("99999999999999999w"), None);
    }
}
