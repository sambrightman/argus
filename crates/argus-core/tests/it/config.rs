//! Config files: `extend`, `extend_*`, and saving tags across extended
//! files.

use std::path::Path;

use argus_core::Config;
use argus_core::configfile::update_tag_lists;
use argus_core::fix::FixKind;

fn write(dir: &Path, rel: &str, text: &str) -> std::path::PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

fn err(r: Result<Config, argus_core::config::ConfigError>) -> String {
    match r {
        Ok(c) => panic!("accepted: {c:?}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn extend_appends_to_any_list_setting() {
    let d = Config::default();
    let c = Config::parse("extend_exclude_paths = ['build-cache']").unwrap();
    assert_eq!(c.exclude_paths.len(), d.exclude_paths.len() + 1);
    assert_eq!(
        c.exclude_paths[..d.exclude_paths.len()],
        d.exclude_paths[..]
    );
    assert_eq!(c.exclude_paths.last().unwrap(), "build-cache");
    // in nested tables too
    let c = Config::parse("[fix]\nextend_auto = ['pull']").unwrap();
    assert_eq!(c.fix.auto.len(), d.fix.auto.len() + 1);
    assert_eq!(c.fix.auto.last(), Some(&FixKind::Pull));
    // in one file, the list is set first, then extended
    let c = Config::parse("exclude_paths = ['a']\nextend_exclude_paths = ['b']").unwrap();
    assert_eq!(c.exclude_paths, ["a", "b"]);
    // inverse: replacing still replaces
    assert_eq!(
        Config::parse("exclude_paths = ['a']")
            .unwrap()
            .exclude_paths,
        ["a"]
    );
}

#[test]
fn extend_only_applies_to_lists() {
    let e = err(Config::parse("extend_max_depth = [1]"));
    assert!(e.contains("extend_max_depth") && e.contains("list"), "{e}");
    let e = err(Config::parse("extend_nope = ['x']"));
    assert!(
        e.contains("extend_nope") && e.contains("no such setting"),
        "{e}"
    );
    let e = err(Config::parse("extend_exclude_paths = 'x'"));
    assert!(
        e.contains("extend_exclude_paths") && e.contains("list"),
        "{e}"
    );
}

#[test]
fn extended_files_are_read_first_and_overridden_by_the_extending_file() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(
        d,
        "base.toml",
        "max_depth = 3\nconcurrency.disk = 1\nconcurrency.network = 2\nexclude_paths = ['from-base']\ntags.favourite.repos = ['base/*']\n[[track]]\nowner = 'base-org'\n",
    );
    write(
        d,
        "conf.d/b.toml",
        "concurrency.network = 5\n[[track]]\nowner = 'b-org'\n",
    );
    write(
        d,
        "conf.d/a.toml",
        "concurrency.network = 4\ntags.favourite.extend_repos = ['a/*']\n[ui]\ntheme = 'dark'\n",
    );
    write(d, "conf.d/notes.txt", "not toml");
    let main = write(
        d,
        "config.toml",
        "extend = ['base.toml', 'conf.d/*.toml', 'none.d/*.toml']\nmax_depth = 9\nextend_exclude_paths = ['from-main']\n[[track]]\nowner = 'main-org'\n",
    );
    let c = Config::load(&main).unwrap();
    // later files win: base, then conf.d/a, conf.d/b (sorted), then main
    assert_eq!(c.max_depth, 9);
    assert_eq!(
        (c.concurrency.network, c.concurrency.disk),
        (5, 1),
        "sections merge across files"
    );
    assert_eq!(c.ui.theme, "dark");
    assert_eq!(c.exclude_paths, ["from-base", "from-main"]);
    assert_eq!(c.tags["favourite"].repos, ["base/*", "a/*"]);
    // lists of tables add up
    let owners: Vec<&str> = c.tracks.iter().map(|t| t.owner.as_str()).collect();
    assert_eq!(owners, ["base-org", "b-org", "main-org"]);
    assert_eq!(c.extend, ["base.toml", "conf.d/*.toml", "none.d/*.toml"]);
}

#[test]
fn extended_files_nest_and_resolve_relative_to_their_file() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(d, "a/inner.toml", "concurrency.network = 7\n");
    write(d, "a/outer.toml", "extend = ['inner.toml']\n");
    let main = write(d, "config.toml", "extend = ['a/outer.toml']\n");
    assert_eq!(Config::load(&main).unwrap().concurrency.network, 7);
}

#[test]
fn extend_errors_name_the_file() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    // a missing file (globs may match nothing)
    let main = write(d, "m1.toml", "extend = ['gone.toml']\n");
    let e = err(Config::load(&main));
    assert!(e.contains("gone.toml") && e.contains("no such file"), "{e}");
    // a cycle
    write(d, "x.toml", "extend = ['y.toml']\n");
    write(d, "y.toml", "extend = ['x.toml']\n");
    let main = write(d, "m2.toml", "extend = ['x.toml']\n");
    let e = err(Config::load(&main));
    assert!(
        e.contains("cycle") && e.contains("x.toml") && e.contains("y.toml"),
        "{e}"
    );
    // bad TOML in an extended file
    write(d, "bad.toml", "max_depth = = 1\n");
    let main = write(d, "m3.toml", "extend = ['bad.toml']\n");
    let e = err(Config::load(&main));
    assert!(e.contains("bad.toml"), "{e}");
    // a glob outside the file name
    let main = write(d, "m4.toml", "extend = ['*/x.toml']\n");
    let e = err(Config::load(&main));
    assert!(e.contains("*/x.toml"), "{e}");
    // extend must be a list of strings
    let main = write(d, "m5.toml", "extend = 'x.toml'\n");
    let e = err(Config::load(&main));
    assert!(e.contains("extend"), "{e}");
}

#[test]
fn tags_are_saved_to_the_file_holding_them() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let inc = write(
        d,
        "tags.toml",
        "# shared\ntags.favourite.repos = ['acme/a', 'acme/b']\n",
    );
    let main = write(d, "config.toml", "extend = ['tags.toml']\n# mine\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    // drop an entry that lives in the extended file, add a new one
    after.tags["favourite"].repos = vec!["acme/b".into(), "acme/c".into()];
    update_tag_lists(&main, &before, &after).unwrap();
    let inc_text = std::fs::read_to_string(&inc).unwrap();
    let main_text = std::fs::read_to_string(&main).unwrap();
    assert!(
        inc_text.contains("# shared") && !inc_text.contains("acme/a"),
        "{inc_text}"
    );
    assert!(inc_text.contains("acme/b"), "{inc_text}");
    // the main file doesn't mention favourite, so the addition goes where
    // the list lives: the highest file with it
    assert!(inc_text.contains("acme/c"), "{inc_text}");
    assert_eq!(main_text, "extend = ['tags.toml']\n# mine\n");
    assert_eq!(
        Config::load(&main).unwrap().tags["favourite"].repos,
        ["acme/b", "acme/c"]
    );
}

#[test]
fn an_empty_extend_list_in_the_main_file_keeps_additions_there() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let shared = write(d, "team.toml", "tags.favourite.repos = ['acme/a']\n");
    let main = write(
        d,
        "config.toml",
        "extend = ['team.toml']\ntags.favourite.extend_repos = []\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.tags["favourite"].repos.push("acme/c".into());
    update_tag_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "tags.favourite.repos = ['acme/a']\n",
        "shared file untouched"
    );
    assert_eq!(
        Config::load(&main).unwrap().tags["favourite"].repos,
        ["acme/a", "acme/c"]
    );
}

#[test]
fn in_a_file_with_both_keys_an_untag_goes_last_so_it_wins() {
    let t = tempfile::tempdir().unwrap();
    let main = write(
        t.path(),
        "config.toml",
        "tags.pull.repos = []\ntags.pull.extend_repos = ['acme/*']\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.tags["pull"].repos.push("!acme/big".into());
    update_tag_lists(&main, &before, &after).unwrap();
    // tags.pull.extend_repos applies after pull, so the ! entry belongs there
    assert_eq!(
        Config::load(&main).unwrap().tags["pull"].repos,
        ["acme/*", "!acme/big"]
    );
}

#[test]
fn tags_are_added_to_the_main_files_own_list_when_it_has_one() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(d, "tags.toml", "tags.pull.repos = ['acme/*']\n");
    let main = write(
        d,
        "config.toml",
        "extend = ['tags.toml']\ntags.pull.extend_repos = ['x/y']\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.tags["pull"].repos.push("!acme/big".into());
    update_tag_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.tags["pull"].repos, ["acme/*", "x/y", "!acme/big"]);
    let text = std::fs::read_to_string(&main).unwrap();
    assert!(!text.contains("\npull ="), "{text}");
}

#[test]
fn an_untag_alone_is_saved_in_the_file_holding_the_entry() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let shared = write(
        d,
        "team.toml",
        "tags.favourite.repos = ['acme/a', 'acme/b']\n",
    );
    let main = write(d, "config.toml", "extend = ['team.toml']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.tags["favourite"].repos.retain(|e| e != "acme/a");
    update_tag_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "tags.favourite.repos = ['acme/b']\n"
    );
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "extend = ['team.toml']\n"
    );
}

#[test]
fn issue_lists_are_saved_into_ignore_by_the_same_rule() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    // none yet: a [tags.ignore.issues] section in the main file
    let main = write(d, "config.toml", "max_depth = 3\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after
        .ignore_issues_mut()
        .detached
        .push("acme/dep@v1.2".into());
    update_tag_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore_issues().detached, ["acme/dep@v1.2"]);
    assert_eq!(c.max_depth, 3);
    // an extended file has the list: it goes there
    let team = write(
        d,
        "team.toml",
        "[tags.ignore.issues]\nstashes = ['acme/a']\n",
    );
    let main = write(d, "config.toml", "extend = ['team.toml']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore_issues_mut().stashes.push("acme/b".into());
    after.tags["ignore"].repos.push("acme/old".into());
    update_tag_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore_issues().stashes, ["acme/a", "acme/b"]);
    assert!(std::fs::read_to_string(&team).unwrap().contains("acme/b"));
    assert_eq!(
        c.tags["ignore"].repos,
        ["acme/old"],
        "repos: new, in the main file"
    );
    // an empty extend_ list in the main file's [tags.ignore.issues] keeps additions there
    let team = write(
        d,
        "team.toml",
        "[tags.ignore.issues]\nstashes = ['acme/a']\n",
    );
    let main = write(
        d,
        "config.toml",
        "extend = ['team.toml']\n[tags.ignore.issues]\nextend_stashes = []\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore_issues_mut().stashes.push("acme/b".into());
    update_tag_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&team).unwrap(),
        "[tags.ignore.issues]\nstashes = ['acme/a']\n"
    );
    assert_eq!(
        Config::load(&main).unwrap().ignore_issues().stashes,
        ["acme/a", "acme/b"]
    );
    // dotted keys are edited where they are
    let main = write(d, "config.toml", "tags.ignore.issues.behind = ['acme/x']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore_issues_mut().behind = vec!["acme/y".into()];
    update_tag_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "tags.ignore.issues.behind = ['acme/y']\n".replace("'acme/y'", "\"acme/y\"")
    );
}

#[test]
fn stashes_default_to_ignoring_all_but_argus_own_and_a_list_replaces_that() {
    let c = Config::default();
    assert_eq!(c.ignore_issues().stashes, ["**", "!**@argus *"]);
    assert_eq!(
        Config::parse("").unwrap().ignore_issues().stashes,
        c.ignore_issues().stashes
    );
    let c = Config::parse("[tags.ignore.issues]\nstashes = ['acme/a']").unwrap();
    assert_eq!(c.ignore_issues().stashes, ["acme/a"]);
    let c = Config::parse("[tags.ignore.issues]\nextend_stashes = ['acme/a']").unwrap();
    assert_eq!(c.ignore_issues().stashes, ["**", "!**@argus *", "acme/a"]);
}

#[test]
fn an_addition_to_a_list_with_a_default_extends_it_rather_than_replacing_it() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let main = write(d, "config.toml", "# mine\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore_issues_mut().stashes.push("acme/a".into());
    // inverse: a list without a default gets the plain key
    after.ignore_issues_mut().behind.push("acme/b".into());
    update_tag_lists(&main, &before, &after).unwrap();
    let text = std::fs::read_to_string(&main).unwrap();
    assert!(text.contains("extend_stashes = [\"acme/a\"]"), "{text}");
    assert!(
        text.contains("behind = [\"acme/b\"]") && !text.contains("extend_behind"),
        "{text}"
    );
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore_issues().stashes, ["**", "!**@argus *", "acme/a"]);
    assert_eq!(c.ignore_issues().behind, ["acme/b"]);
}

#[test]
fn auto_fix_kinds_that_are_never_automatic_are_warned_about() {
    assert!(Config::default().fix.warnings().is_empty());
    let c = Config::parse("[fix]\nauto = ['push', 'stash-pop', 'reattach']").unwrap();
    assert!(c.fix.warnings().is_empty(), "all can be automatic");
    let c = Config::parse("[fix]\nauto = ['commit', 'push', 'abort']").unwrap();
    let w = c.fix.warnings();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("commit, abort") && w[0].contains("never automatic"),
        "{w:?}"
    );
}

/// Tags are tables: favourite's defaults are in code (key, icon, sort),
/// any field can be set; yours follow it, in the order written.
#[test]
fn tags_are_tables_with_defaults_for_favourite() {
    let c = Config::default();
    let names: Vec<&String> = c.tags.keys().collect();
    assert_eq!(names, ["favourite", "pull", "ignore"]);
    let (pull, ignore) = (&c.tags["pull"], &c.tags["ignore"]);
    assert_eq!((pull.key, pull.sort, &pull.icon), (Some('p'), 0, &None));
    assert_eq!((ignore.key, ignore.sort), (Some('I'), 0));
    assert!(pull.repos.is_empty() && ignore.repos.is_empty());
    // only ignore has issues not to flag: by default, stashes not argus's
    assert!(pull.issues.is_empty() && !ignore.issues.is_empty());
    assert_eq!(ignore.issues.stashes.len(), 2);
    assert!(ignore.issues.behind.is_empty());
    let fav = &c.tags["favourite"];
    assert_eq!((fav.key, fav.sort), (Some('f'), 1));
    assert!(fav.repos.is_empty());
    let c = Config::parse(
        "[tags.work]\nkey = 'w'\nicon = 'W'\ncolor = 'cyan'\nrepos = ['acme/*', '!acme/old']\n\
         [tags.home]\nicon = { unicode = '⌂', ascii = 'h' }\nsort = 2\n\
         [tags.favourite]\nrepos = ['acme/app']\n",
    )
    .unwrap();
    let names: Vec<&String> = c.tags.keys().collect();
    assert_eq!(
        names,
        ["favourite", "pull", "ignore", "work", "home"],
        "defaults first, then as written"
    );
    assert_eq!(c.tags["favourite"].repos, ["acme/app"]);
    assert_eq!(c.tags["favourite"].sort, 1, "the rest of its defaults kept");
    let work = &c.tags["work"];
    assert_eq!(
        (work.key, work.sort, work.color.as_deref()),
        (Some('w'), 0, Some("cyan"))
    );
    assert_eq!(work.repos, ["acme/*", "!acme/old"]);
    // icons by set: one for all, or per set (unicode standing in for a missing one)
    assert_eq!(c.tag_icon("work", "nerd"), "W");
    assert_eq!(c.tag_icon("home", "ascii"), "h");
    assert_eq!(c.tag_icon("home", "nerd"), "⌂");
    assert_eq!(c.tag_icon("favourite", "unicode"), "◆");
    assert_eq!(c.tag_icon("favourite", "ascii"), "*");
    assert_eq!(
        c.tag_icon("favourite", "nerd"),
        "\u{eb59} ",
        "its own for nerd"
    );
    // none: its name's first letter
    let c = Config::parse("[tags.later]\n").unwrap();
    assert_eq!(c.tag_icon("later", "unicode"), "l");
}

/// Tag names are words (they're keys and `#name` filters); keys one
/// character; a top-level `favourite` list says where it went.
#[test]
fn bad_tags_are_explained() {
    // words: letters, digits, - and _
    assert!(Config::parse("[tags.my-work_2]\n").is_ok());
    assert!(err(Config::parse("[tags.\"a b\"]\n")).contains("letters, digits"));
    assert!(
        err(Config::parse("[tags.\"\"]\n")).contains("letters, digits"),
        "empty"
    );
    assert!(err(Config::parse("[tags.w]\nkey = 'ww'\n")).contains("key"));
    // pull and ignore are tags like any other, with their own keys to set
    let c = Config::parse("[tags.pull]\nkey = 'P'\nrepos = ['a/b']\n[tags.ignore]\nicon = 'x'\n")
        .unwrap();
    assert_eq!(c.tags["pull"].key, Some('P'));
    assert_eq!(c.tags["pull"].repos, ["a/b"]);
    assert_eq!(c.tags["ignore"].key, Some('I'), "defaults kept");
    // but only ignore lists issues
    let e = err(Config::parse("[tags.work.issues]\nbehind = ['a/b']\n"));
    assert!(e.contains("[tags.work] issues: only [tags.ignore]"), "{e}");
    assert!(Config::parse("[tags.ignore.issues]\nbehind = ['a/b']\n").is_ok());
    assert!(
        err(Config::parse("[tags.ignore.issues]\nnope = []\n")).contains("nope"),
        "an unknown issue"
    );
    assert!(err(Config::parse("favourite = ['acme/app']\n")).contains("[tags.favourite] repos"));
    assert!(err(Config::parse("[tags.w]\nrepo = []\n")).contains("repo"));
}

/// Adding to a tag of your own writes its table.
#[test]
fn a_new_tags_entries_are_saved_in_its_table() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(dir.path(), "config.toml", "[tags.work]\nkey = 'w'\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after
        .tags
        .get_mut("work")
        .unwrap()
        .repos
        .push("acme/app".into());
    after
        .tags
        .get_mut("favourite")
        .unwrap()
        .repos
        .push("acme/fav".into());
    update_tag_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.tags["work"].repos, ["acme/app"]);
    assert_eq!(c.tags["favourite"].repos, ["acme/fav"]);
    assert_eq!(c.tags["work"].key, Some('w'), "the table's other keys kept");
}
