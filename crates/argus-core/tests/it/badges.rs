//! Badge text built from icons and repo state.

use argus_core::badges;
use argus_core::icons::UNICODE;
use argus_core::testkit::{Sandbox, commit, git};
use argus_core::{Config, State, Workspace};

fn entry_badge(r: &std::path::Path) -> String {
    let cfg = format!("roots = ['{}']", r.parent().unwrap().display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    badges::branch(&ws.entries[0], &UNICODE).unwrap().text
}

#[test]
fn detached_head_shows_its_tag_or_commit() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    commit(&r, "second");
    git(&r, &["tag", "v1.9"]);
    git(&r, &["tag", "v1.10"]);
    git(&r, &["tag", "-a", "-m", "r", "release"]);
    assert_eq!(entry_badge(&r), "main", "a branch shows the branch");
    git(&r, &["switch", "-q", "--detach", "v1.9"]);
    assert_eq!(entry_badge(&r), "@release (& 2 more)");
    git(&r, &["tag", "-d", "release", "v1.10"]);
    assert_eq!(entry_badge(&r), "@v1.9");
    // before every tag: the commit
    git(&r, &["switch", "-q", "--detach", "HEAD~1"]);
    let id = git(&r, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(entry_badge(&r), format!("@{}", id.trim()));
    // after a tag: where it is relative to the nearest one, as git describe says
    git(&r, &["switch", "-q", "--detach", "v1.9"]);
    commit(&r, "third");
    let described = git(&r, &["describe", "--tags"]);
    assert_eq!(entry_badge(&r), format!("@{}", described.trim()));
    assert!(described.starts_with("v1.9-1-g"), "{described}");
}

#[test]
fn commits_only_on_a_detached_head_have_their_own_icon() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    git(&r, &["switch", "-q", "--detach"]);
    commit(&r, "experiment");
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let texts: Vec<String> = badges::state(&ws.entries[0], &UNICODE)
        .into_iter()
        .map(|b| b.text)
        .collect();
    let want = format!("{}1", UNICODE.detached_commits);
    assert!(texts.contains(&want), "{texts:?}");
    // inverse: not the detached icon, which already starts the branch badge
    assert!(
        !texts.contains(&format!("{}1", UNICODE.detached)),
        "{texts:?}"
    );
    assert_ne!(UNICODE.detached_commits, UNICODE.detached);
}

#[test]
fn flag_badges_show_favourite_new_and_ignored_or_snoozed() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    let cfg = |extra: &str| format!("roots = ['{}']\n{extra}", r.parent().unwrap().display());
    let texts = |extra: &str| {
        let mut ws = Workspace::new(Config::parse(&cfg(extra)).unwrap(), State::default()).unwrap();
        ws.scan();
        badges::flags(&ws.entries[0], &UNICODE)
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
    };
    assert!(texts("").is_empty());
    let path = r.display().to_string();
    assert_eq!(
        texts(&format!("favourite = ['{path}']")),
        [UNICODE.favourite.to_string()]
    );
    assert_eq!(
        texts(&format!("ignore.repos = ['{path}']")),
        [UNICODE.ignored.to_string()]
    );
}

fn state_badges(r: &std::path::Path, extra: &str) -> Vec<String> {
    let cfg = format!("roots = ['{}']\n{extra}", r.parent().unwrap().display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    badges::state(&ws.entries[0], &UNICODE)
        .into_iter()
        .map(|b| b.text)
        .collect()
}

#[test]
fn ignoring_keeps_badges_stating_facts_and_drops_warning_only_ones() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    sb.advance_remote("acme", "w", "new.txt");
    git(&r, &["fetch", "-q"]);
    argus_core::testkit::write(&r, "README", "stashed\n");
    git(&r, &["stash", "push", "-q", "-m", "mine"]);
    let facts = ["↓1", "$1"];
    for extra in [
        "[ignore]\nstashes = []",
        "[ignore]\nbehind = ['**']\nstashes = ['**']",
    ] {
        let b = state_badges(&r, extra);
        assert!(
            facts.iter().all(|f| b.contains(&f.to_string())),
            "{extra}: {b:?}"
        );
    }
    // warnings: no upstream for this branch, and another unpushed branch
    git(&r, &["switch", "-q", "-c", "other"]);
    commit(&r, "other work");
    git(&r, &["switch", "-q", "-c", "feat"]);
    commit(&r, "feat work");
    let warnings = ["↑?", "»1"];
    let b = state_badges(&r, "");
    assert!(warnings.iter().all(|w| b.contains(&w.to_string())), "{b:?}");
    let b = state_badges(
        &r,
        "[ignore]\nno_upstream = ['**']\nunpushed_branches = ['**']",
    );
    assert!(
        !warnings.iter().any(|w| b.contains(&w.to_string())),
        "{b:?}"
    );
}
