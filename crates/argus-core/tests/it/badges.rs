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
fn flag_badges_show_favourite_new_ignored_or_snoozed_and_pull() {
    let sb = Sandbox::new();
    let r = sb.init("dev/r");
    let cfg = |extra: &str| format!("roots = ['{}']\n{extra}", r.parent().unwrap().display());
    let texts = |extra: &str| {
        let mut ws = Workspace::new(Config::parse(&cfg(extra)).unwrap(), State::default()).unwrap();
        ws.scan();
        badges::flags(&ws.entries[0], &UNICODE, &ws.config.mark_icons())
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
    };
    assert!(texts("").is_empty());
    let path = r.display().to_string();
    assert_eq!(
        texts(&format!("marks.favourite.repos = ['{path}']")),
        ["◆".to_string()]
    );
    assert_eq!(
        texts(&format!("ignore.repos = ['{path}']")),
        [UNICODE.ignored.to_string()]
    );
    // pulled by `argus pull`: after the others, being the most common
    assert_eq!(
        texts(&format!("pull = ['{path}']")),
        [UNICODE.pull.to_string()]
    );
    assert_eq!(
        texts(&format!(
            "pull = ['{path}']\nmarks.favourite.repos = ['{path}']"
        )),
        ["◆".to_string(), UNICODE.pull.to_string()]
    );
    // an ignored repo isn't pulled, so it shows no pull mark
    assert_eq!(
        texts(&format!("pull = ['{path}']\nignore.repos = ['{path}']")),
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

#[test]
fn change_badges_count_each_kind_and_only_when_there_are_some() {
    let sb = Sandbox::new();
    let r = sb.init("dev/w");
    let i = &UNICODE;
    // (a local repo: its having no remote isn't what's counted here)
    let local = "[ignore]\nno_remote = ['**']";
    assert_eq!(state_badges(&r, local), Vec::<String>::new(), "clean");
    argus_core::testkit::write(&r, "README", "staged\n");
    git(&r, &["add", "README"]);
    argus_core::testkit::write(&r, "a", "x");
    argus_core::testkit::write(&r, "b", "x");
    assert_eq!(
        state_badges(&r, local),
        [format!("{}1", i.staged), format!("{}2", i.untracked),]
    );
    git(&r, &["add", "a", "b"]);
    commit(&r, "ab");
    argus_core::testkit::write(&r, "a", "changed");
    assert_eq!(state_badges(&r, local), [format!("{}1", i.unstaged)]);
}

#[test]
fn conflicts_are_counted() {
    let sb = Sandbox::new();
    let r = sb.init("dev/w");
    git(&r, &["switch", "-q", "-c", "other"]);
    argus_core::testkit::write(&r, "README", "theirs\n");
    commit(&r, "theirs");
    git(&r, &["switch", "-q", "main"]);
    argus_core::testkit::write(&r, "README", "ours\n");
    commit(&r, "ours");
    assert!(!argus_core::testkit::git_may_fail(
        &r,
        &["merge", "-q", "other"]
    ));
    let b = state_badges(&r, "");
    assert!(b.contains(&format!("{}1", UNICODE.conflict)), "{b:?}");
}

#[test]
fn ahead_and_behind_badges_show_only_when_not_zero() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    let r = sb.clone_as_github("acme", "w", "dev/w");
    assert_eq!(state_badges(&r, ""), Vec::<String>::new(), "level");
    commit(&r, "mine");
    assert_eq!(state_badges(&r, ""), [format!("{}1", UNICODE.ahead)]);
}

#[test]
fn a_repo_without_a_remote_says_so_unless_that_is_ignored() {
    let sb = Sandbox::new();
    let r = sb.init("dev/local");
    assert_eq!(state_badges(&r, ""), [UNICODE.no_remote.to_string()]);
    assert_eq!(
        state_badges(&r, "[ignore]\nno_remote = ['**']"),
        Vec::<String>::new()
    );
    // inverse: with a remote, nothing
    sb.remote("acme", "w");
    let w = sb.clone_as_github("acme", "w", "other/w");
    assert!(!state_badges(&w, "").contains(&UNICODE.no_remote.to_string()));
}
