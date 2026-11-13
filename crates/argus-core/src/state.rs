//! Machine-written state: marks made from the UI, snoozes, forge listings and
//! "new" flags. Saved atomically (write temp + rename).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::attention::Issue;

/// Marks as older versions of argus kept them, per repo key. They now live
/// in config (`favourite`, `pull`, `ignore`); see
/// [`State::old_marks_as_config`].
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
    /// Marks from older versions, kept unchanged so nothing is lost (they're
    /// no longer used); remove them by hand once copied to config.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, Marks>,
    /// Temporarily hidden until the timestamp.
    pub snoozed: BTreeMap<String, Timestamp>,
    /// Issues snoozed by repo key, by name ([`Issue::name`]), until the
    /// timestamp: they don't count until then.
    ///
    /// [`Issue::name`]: crate::attention::Issue::name
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub snoozed_issues: BTreeMap<String, BTreeMap<String, Timestamp>>,
    /// Repos (by key) cloned or discovered by argus and not yet acknowledged.
    pub new: BTreeSet<String>,
    pub tracks: BTreeMap<String, TrackState>,
    /// When `argus pull` last ran (`last_sync` before it was renamed).
    #[serde(alias = "last_sync")]
    pub last_pull: Option<Timestamp>,
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

    /// Whether `issue` (of that kind) of the repo is snoozed.
    pub fn is_issue_snoozed(&self, key: &str, issue: Issue, now: Timestamp) -> bool {
        self.snoozed_issues
            .get(key)
            .and_then(|m| m.get(issue.name()))
            .is_some_and(|until| *until > now)
    }

    pub fn snooze_issue(&mut self, key: &str, issue: Issue, until: Timestamp) {
        self.snoozed_issues
            .entry(key.to_string())
            .or_default()
            .insert(issue.name().to_string(), until);
    }

    pub fn unsnooze_issue(&mut self, key: &str, issue: Issue) {
        if let Some(m) = self.snoozed_issues.get_mut(key) {
            m.remove(issue.name());
            if m.is_empty() {
                self.snoozed_issues.remove(key);
            }
        }
    }

    /// End the repo's snooze and its issues'.
    pub fn unsnooze_all(&mut self, key: &str) {
        self.snoozed.remove(key);
        self.snoozed_issues.remove(key);
    }

    /// Drop expired snoozes, of repos and of issues; returns whether
    /// anything changed.
    pub fn expire_snoozes(&mut self, now: Timestamp) -> bool {
        let count = |s: &Self| {
            s.snoozed.len() + s.snoozed_issues.values().map(BTreeMap::len).sum::<usize>()
        };
        let before = count(self);
        self.snoozed.retain(|_, until| *until > now);
        for m in self.snoozed_issues.values_mut() {
            m.retain(|_, until| *until > now);
        }
        self.snoozed_issues.retain(|_, m| !m.is_empty());
        before != count(self)
    }

    /// Marks from an older version (see `marks`) as config lines to copy,
    /// if there are any.
    pub fn old_marks_as_config(&self) -> Option<String> {
        if self.marks.is_empty() {
            return None;
        }
        let list = |get: fn(&Marks) -> Option<bool>| {
            let entries: Vec<String> = self
                .marks
                .iter()
                .filter_map(|(k, m)| {
                    get(m).map(|on| format!("{:?}", if on { k.clone() } else { format!("!{k}") }))
                })
                .collect();
            format!("[{}]", entries.join(", "))
        };
        let lines = [
            ("marks.favourite.repos", list(|m| m.favourite)),
            ("pull", list(|m| m.sync)),
            ("ignore.repos", list(|m| m.exclude)),
        ]
        .into_iter()
        .filter(|(_, l)| l != "[]")
        .map(|(k, l)| format!("{k} = {l}"))
        .collect::<Vec<_>>();
        Some(lines.join("\n"))
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
        // a snooze ends at its time exactly
        s.snoozed.insert("now".into(), now);
        assert!(!s.is_snoozed("now", now));
        assert!(s.expire_snoozes(now));
        assert!(!s.snoozed.contains_key("now"));
    }

    #[test]
    fn an_unreadable_state_file_is_an_error_not_a_fresh_state() {
        let d = tempfile::tempdir().unwrap();
        let e = State::load(d.path()).unwrap_err();
        assert!(matches!(e, StateError::Io { .. }), "{e}");
    }

    #[test]
    fn the_last_pull_time_is_read_from_its_old_name_and_saved_under_the_new() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.toml");
        std::fs::write(&p, "last_sync = \"2026-01-01T00:00:00Z\"\n").unwrap();
        let s = State::load(&p).unwrap();
        assert!(s.last_pull.is_some());
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("last_pull") && !text.contains("last_sync"),
            "{text}"
        );
    }

    #[test]
    fn old_marks_are_kept_and_shown_as_config() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.toml");
        std::fs::write(
            &p,
            "[marks.\"github.com/a/b\"]\nfavourite = true\nsync = false\n\
             [marks.\"/home/me/x\"]\nexclude = true\n\
             [seen]\n\"github.com/a/b\" = 2026-01-01T00:00:00Z\n",
        )
        .unwrap();
        let s = State::load(&p).unwrap();
        assert_eq!(
            s.old_marks_as_config().unwrap(),
            "marks.favourite.repos = [\"github.com/a/b\"]\npull = [\"!github.com/a/b\"]\nignore.repos = [\"/home/me/x\"]"
        );
        // saving keeps them (never lost without asking); the old clocks go
        s.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("github.com/a/b") && text.contains("/home/me/x"),
            "{text}"
        );
        assert!(!text.contains("seen"), "{text}");
        assert_eq!(State::load(&p).unwrap().marks, s.marks);
        // inverse: nothing to show
        assert_eq!(State::default().old_marks_as_config(), None);
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
