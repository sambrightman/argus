//! Resolution of per-repo flags from config rules, tracked owners and state.

use std::path::Path;

use globset::GlobMatcher;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigError, Rule, Track};
use crate::identity::RepoId;
use crate::paths::expand_tilde;
use crate::state::State;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    pub favourite: bool,
    pub sync: bool,
    pub excluded: bool,
    pub snoozed: bool,
    /// Recently cloned/discovered by argus and not acknowledged.
    pub new: bool,
    /// Key of the tracked owner this repo belongs to.
    pub tracked: Option<String>,
}

impl Flags {
    /// Hidden from the default view and skipped by bulk operations.
    pub fn hidden(&self) -> bool {
        self.excluded || self.snoozed
    }
}

#[derive(Clone)]
struct CompiledRule {
    matcher: GlobMatcher,
    rule: Rule,
}

#[derive(Clone)]
pub struct RuleSet {
    rules: Vec<CompiledRule>,
}

impl RuleSet {
    pub fn new(rules: &[Rule]) -> Result<Self, ConfigError> {
        let rules = rules
            .iter()
            .map(|r| {
                let pattern = expand_tilde(Path::new(&r.pattern)).display().to_string();
                let glob = globset::GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .case_insensitive(true)
                    .build()
                    .map_err(|source| ConfigError::Pattern {
                        pattern: r.pattern.clone(),
                        source,
                    })?;
                Ok(CompiledRule {
                    matcher: glob.compile_matcher(),
                    rule: r.clone(),
                })
            })
            .collect::<Result<_, ConfigError>>()?;
        Ok(Self { rules })
    }

    fn matching<'a>(&'a self, id: Option<&RepoId>, path: &Path) -> impl Iterator<Item = &'a Rule> {
        let full = id.map(|i| i.to_string());
        let slug = id.map(|i| i.slug());
        let path = path.to_path_buf();
        self.rules.iter().filter_map(move |c| {
            let hit = full.as_deref().is_some_and(|s| c.matcher.is_match(s))
                || slug.as_deref().is_some_and(|s| c.matcher.is_match(s))
                || c.matcher.is_match(&path);
            hit.then_some(&c.rule)
        })
    }

    /// Whether a (possibly not yet cloned) repo is excluded by rules alone.
    pub fn excludes(&self, id: &RepoId, path: &Path) -> bool {
        self.matching(Some(id), path)
            .fold(false, |acc, r| r.exclude.unwrap_or(acc))
    }
}

/// The tracked owner a repo belongs to (first match), using the last forge
/// listing for team-restricted tracks.
pub fn track_for<'a>(config: &'a Config, state: &State, id: &RepoId) -> Option<&'a Track> {
    config.tracks.iter().find(|t| {
        if !t.host.eq_ignore_ascii_case(&id.host) || !t.owner.eq_ignore_ascii_case(&id.owner) {
            return false;
        }
        match &t.team {
            None => true,
            Some(_) => state.tracks.get(&t.key()).is_some_and(|ts| {
                ts.repos
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(&id.to_string()))
            }),
        }
    })
}

/// Precedence (lowest to highest): track membership, config rules in order,
/// interactive marks from state. Snoozes and "new" come from state only.
pub fn resolve(
    config: &Config,
    rules: &RuleSet,
    state: &State,
    id: Option<&RepoId>,
    key: &str,
    path: &Path,
    now: Timestamp,
) -> Flags {
    let mut f = Flags::default();
    if let Some(t) = id.and_then(|id| track_for(config, state, id)) {
        f.sync = t.sync;
        f.tracked = Some(t.key());
    }
    for r in rules.matching(id, path) {
        if let Some(v) = r.favourite {
            f.favourite = v;
        }
        if let Some(v) = r.sync {
            f.sync = v;
        }
        if let Some(v) = r.exclude {
            f.excluded = v;
        }
    }
    if let Some(m) = state.marks.get(key) {
        if let Some(v) = m.favourite {
            f.favourite = v;
        }
        if let Some(v) = m.sync {
            f.sync = v;
        }
        if let Some(v) = m.exclude {
            f.excluded = v;
        }
    }
    f.snoozed = state.is_snoozed(key, now);
    f.new = state.new.contains(key);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TrackState;

    fn now() -> Timestamp {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    fn flags(cfg: &str, state: &State, id: &str, path: &str) -> Flags {
        let c = Config::parse(cfg).unwrap();
        let rs = RuleSet::new(&c.rules).unwrap();
        let id = RepoId::from_url(id);
        let key = id
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or(path.to_string());
        resolve(&c, &rs, state, id.as_ref(), &key, Path::new(path), now())
    }

    const URL: &str = "git@github.com:acme/widget.git";

    #[test]
    fn nothing_configured_means_no_flags() {
        assert_eq!(
            flags("", &State::default(), URL, "/r/widget"),
            Flags::default()
        );
    }

    #[test]
    fn rules_match_identity_slug_and_path() {
        let s = State::default();
        assert!(
            flags(
                "[[rule]]\nmatch='github.com/acme/*'\nfavourite=true",
                &s,
                URL,
                "/x"
            )
            .favourite
        );
        assert!(
            flags(
                "[[rule]]\nmatch='acme/widget'\nfavourite=true",
                &s,
                URL,
                "/x"
            )
            .favourite
        );
        assert!(
            flags(
                "[[rule]]\nmatch='/r/**'\nfavourite=true",
                &s,
                URL,
                "/r/a/widget"
            )
            .favourite
        );
        assert!(
            flags("[[rule]]\nmatch='ACME/*'\nfavourite=true", &s, URL, "/x").favourite,
            "case-insensitive"
        );
        // inverse: other owners, and `*` must not cross `/`
        assert!(
            !flags(
                "[[rule]]\nmatch='github.com/other/*'\nfavourite=true",
                &s,
                URL,
                "/x"
            )
            .favourite
        );
        assert!(
            !flags(
                "[[rule]]\nmatch='github.com/*'\nfavourite=true",
                &s,
                URL,
                "/x"
            )
            .favourite
        );
        assert!(
            !flags(
                "[[rule]]\nmatch='/r/*'\nfavourite=true",
                &s,
                URL,
                "/r/a/widget"
            )
            .favourite
        );
    }

    #[test]
    fn later_rules_override_earlier() {
        let s = State::default();
        let cfg = "[[rule]]\nmatch='acme/*'\nsync=true\n[[rule]]\nmatch='acme/widget'\nsync=false";
        assert!(!flags(cfg, &s, URL, "/x").sync);
        assert!(flags(cfg, &s, "git@github.com:acme/other.git", "/x").sync);
    }

    #[test]
    fn tracked_owner_implies_sync_unless_overridden() {
        let s = State::default();
        let f = flags("[[track]]\nowner='acme'", &s, URL, "/x");
        assert!(f.sync);
        assert_eq!(f.tracked.as_deref(), Some("github.com/acme"));
        assert!(!flags("[[track]]\nowner='acme'\nsync=false", &s, URL, "/x").sync);
        assert!(
            !flags(
                "[[track]]\nowner='acme'\n[[rule]]\nmatch='acme/widget'\nsync=false",
                &s,
                URL,
                "/x"
            )
            .sync
        );
        assert_eq!(
            flags("[[track]]\nowner='other'", &s, URL, "/x").tracked,
            None
        );
        assert_eq!(
            flags("[[track]]\nowner='acme'\nhost='gitlab.com'", &s, URL, "/x").tracked,
            None
        );
    }

    #[test]
    fn team_track_uses_last_listing() {
        let cfg = "[[track]]\nowner='acme'\nteam='core'";
        assert_eq!(
            flags(cfg, &State::default(), URL, "/x").tracked,
            None,
            "unknown membership"
        );
        let mut s = State::default();
        s.tracks.insert(
            "github.com/acme#core".into(),
            TrackState {
                repos: ["github.com/acme/widget".to_string()].into(),
                listed_at: None,
            },
        );
        assert!(flags(cfg, &s, URL, "/x").sync);
        assert!(!flags(cfg, &s, "git@github.com:acme/notinteam.git", "/x").sync);
    }

    #[test]
    fn state_marks_win_over_rules() {
        let mut s = State::default();
        s.mark("github.com/acme/widget", |m| m.exclude = Some(false));
        assert!(!flags("[[rule]]\nmatch='acme/*'\nexclude=true", &s, URL, "/x").excluded);
        s.mark("github.com/acme/widget", |m| m.exclude = Some(true));
        assert!(flags("", &s, URL, "/x").excluded);
    }

    #[test]
    fn snooze_and_new_from_state() {
        let mut s = State::default();
        s.snoozed.insert(
            "github.com/acme/widget".into(),
            "2026-02-01T00:00:00Z".parse().unwrap(),
        );
        s.new.insert("github.com/acme/widget".into());
        let f = flags("", &s, URL, "/x");
        assert!(f.snoozed && f.new && f.hidden());
        s.snoozed.insert(
            "github.com/acme/widget".into(),
            "2025-12-01T00:00:00Z".parse().unwrap(),
        );
        assert!(!flags("", &s, URL, "/x").snoozed, "expired snooze");
    }

    #[test]
    fn repos_without_identity_match_by_path_only() {
        let f = flags(
            "[[rule]]\nmatch='/local/**'\nexclude=true",
            &State::default(),
            "",
            "/local/thing",
        );
        assert!(f.excluded);
    }

    #[test]
    fn bad_pattern_is_an_error() {
        let c = Config::parse("[[rule]]\nmatch='a[b'\nfavourite=true").unwrap();
        assert!(RuleSet::new(&c.rules).is_err());
    }
}
