//! Config files: `extend`, `extend_*`, and saving marks across extended
//! files.

use std::path::Path;

use argus_core::Config;
use argus_core::configfile::update_mark_lists;
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
        "max_depth = 3\nconcurrency.disk = 1\nconcurrency.network = 2\nexclude_paths = ['from-base']\nmarks.favourite.repos = ['base/*']\n[[track]]\nowner = 'base-org'\n",
    );
    write(
        d,
        "conf.d/b.toml",
        "concurrency.network = 5\n[[track]]\nowner = 'b-org'\n",
    );
    write(
        d,
        "conf.d/a.toml",
        "concurrency.network = 4\nmarks.favourite.extend_repos = ['a/*']\n[ui]\ntheme = 'dark'\n",
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
    assert_eq!(c.marks["favourite"].repos, ["base/*", "a/*"]);
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
fn marks_are_saved_to_the_file_holding_them() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let inc = write(
        d,
        "marks.toml",
        "# shared\nmarks.favourite.repos = ['acme/a', 'acme/b']\n",
    );
    let main = write(d, "config.toml", "extend = ['marks.toml']\n# mine\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    // drop an entry that lives in the extended file, add a new one
    after.marks["favourite"].repos = vec!["acme/b".into(), "acme/c".into()];
    update_mark_lists(&main, &before, &after).unwrap();
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
    assert_eq!(main_text, "extend = ['marks.toml']\n# mine\n");
    assert_eq!(
        Config::load(&main).unwrap().marks["favourite"].repos,
        ["acme/b", "acme/c"]
    );
}

#[test]
fn an_empty_extend_list_in_the_main_file_keeps_additions_there() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let shared = write(d, "team.toml", "marks.favourite.repos = ['acme/a']\n");
    let main = write(
        d,
        "config.toml",
        "extend = ['team.toml']\nmarks.favourite.extend_repos = []\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.marks["favourite"].repos.push("acme/c".into());
    update_mark_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "marks.favourite.repos = ['acme/a']\n",
        "shared file untouched"
    );
    assert_eq!(
        Config::load(&main).unwrap().marks["favourite"].repos,
        ["acme/a", "acme/c"]
    );
}

#[test]
fn in_a_file_with_both_keys_an_unmark_goes_last_so_it_wins() {
    let t = tempfile::tempdir().unwrap();
    let main = write(
        t.path(),
        "config.toml",
        "pull = []\nextend_pull = ['acme/*']\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.pull.push("!acme/big".into());
    update_mark_lists(&main, &before, &after).unwrap();
    // extend_pull applies after pull, so the ! entry belongs there
    assert_eq!(Config::load(&main).unwrap().pull, ["acme/*", "!acme/big"]);
}

#[test]
fn marks_are_added_to_the_main_files_own_list_when_it_has_one() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write(d, "marks.toml", "pull = ['acme/*']\n");
    let main = write(
        d,
        "config.toml",
        "extend = ['marks.toml']\nextend_pull = ['x/y']\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.pull.push("!acme/big".into());
    update_mark_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.pull, ["acme/*", "x/y", "!acme/big"]);
    let text = std::fs::read_to_string(&main).unwrap();
    assert!(!text.contains("\npull ="), "{text}");
}

#[test]
fn an_unmark_alone_is_saved_in_the_file_holding_the_entry() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let shared = write(
        d,
        "team.toml",
        "marks.favourite.repos = ['acme/a', 'acme/b']\n",
    );
    let main = write(d, "config.toml", "extend = ['team.toml']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.marks["favourite"].repos.retain(|e| e != "acme/a");
    update_mark_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "marks.favourite.repos = ['acme/b']\n"
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
    // none yet: an [ignore] section in the main file
    let main = write(d, "config.toml", "max_depth = 3\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore.detached.push("acme/dep@v1.2".into());
    update_mark_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore.detached, ["acme/dep@v1.2"]);
    assert_eq!(c.max_depth, 3);
    // an extended file has the list: it goes there
    let team = write(d, "team.toml", "[ignore]\nstashes = ['acme/a']\n");
    let main = write(d, "config.toml", "extend = ['team.toml']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore.stashes.push("acme/b".into());
    after.ignore.repos.push("acme/old".into());
    update_mark_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore.stashes, ["acme/a", "acme/b"]);
    assert!(std::fs::read_to_string(&team).unwrap().contains("acme/b"));
    assert_eq!(c.ignore.repos, ["acme/old"], "repos: new, in the main file");
    // an empty extend_ list in the main file's [ignore] keeps additions there
    let team = write(d, "team.toml", "[ignore]\nstashes = ['acme/a']\n");
    let main = write(
        d,
        "config.toml",
        "extend = ['team.toml']\n[ignore]\nextend_stashes = []\n",
    );
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore.stashes.push("acme/b".into());
    update_mark_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&team).unwrap(),
        "[ignore]\nstashes = ['acme/a']\n"
    );
    assert_eq!(
        Config::load(&main).unwrap().ignore.stashes,
        ["acme/a", "acme/b"]
    );
    // dotted keys are edited where they are
    let main = write(d, "config.toml", "ignore.behind = ['acme/x']\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore.behind = vec!["acme/y".into()];
    update_mark_lists(&main, &before, &after).unwrap();
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "ignore.behind = ['acme/y']\n".replace("'acme/y'", "\"acme/y\"")
    );
}

#[test]
fn stashes_default_to_ignoring_all_but_argus_own_and_a_list_replaces_that() {
    let c = Config::default();
    assert_eq!(c.ignore.stashes, ["**", "!**@argus *"]);
    assert_eq!(Config::parse("").unwrap().ignore.stashes, c.ignore.stashes);
    let c = Config::parse("[ignore]\nstashes = ['acme/a']").unwrap();
    assert_eq!(c.ignore.stashes, ["acme/a"]);
    let c = Config::parse("[ignore]\nextend_stashes = ['acme/a']").unwrap();
    assert_eq!(c.ignore.stashes, ["**", "!**@argus *", "acme/a"]);
}

#[test]
fn an_addition_to_a_list_with_a_default_extends_it_rather_than_replacing_it() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let main = write(d, "config.toml", "# mine\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after.ignore.stashes.push("acme/a".into());
    // inverse: a list without a default gets the plain key
    after.ignore.behind.push("acme/b".into());
    update_mark_lists(&main, &before, &after).unwrap();
    let text = std::fs::read_to_string(&main).unwrap();
    assert!(text.contains("extend_stashes = [\"acme/a\"]"), "{text}");
    assert!(
        text.contains("behind = [\"acme/b\"]") && !text.contains("extend_behind"),
        "{text}"
    );
    let c = Config::load(&main).unwrap();
    assert_eq!(c.ignore.stashes, ["**", "!**@argus *", "acme/a"]);
    assert_eq!(c.ignore.behind, ["acme/b"]);
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

/// Marks are tables: favourite's defaults are in code (key, icon, sort),
/// any field can be set; yours follow it, in the order written.
#[test]
fn marks_are_tables_with_defaults_for_favourite() {
    let c = Config::default();
    let names: Vec<&String> = c.marks.keys().collect();
    assert_eq!(names, ["favourite"]);
    let fav = &c.marks["favourite"];
    assert_eq!((fav.key, fav.sort), (Some('f'), 1));
    assert!(fav.repos.is_empty());
    let c = Config::parse(
        "[marks.work]\nkey = 'w'\nicon = 'W'\ncolor = 'cyan'\nrepos = ['acme/*', '!acme/old']\n\
         [marks.home]\nicon = { unicode = '⌂', ascii = 'h' }\nsort = 2\n\
         [marks.favourite]\nrepos = ['acme/app']\n",
    )
    .unwrap();
    let names: Vec<&String> = c.marks.keys().collect();
    assert_eq!(
        names,
        ["favourite", "work", "home"],
        "defaults first, then as written"
    );
    assert_eq!(c.marks["favourite"].repos, ["acme/app"]);
    assert_eq!(
        c.marks["favourite"].sort, 1,
        "the rest of its defaults kept"
    );
    let work = &c.marks["work"];
    assert_eq!(
        (work.key, work.sort, work.color.as_deref()),
        (Some('w'), 0, Some("cyan"))
    );
    assert_eq!(work.repos, ["acme/*", "!acme/old"]);
    // icons by set: one for all, or per set (unicode standing in for a missing one)
    assert_eq!(c.mark_icon("work", "nerd"), "W");
    assert_eq!(c.mark_icon("home", "ascii"), "h");
    assert_eq!(c.mark_icon("home", "nerd"), "⌂");
    assert_eq!(c.mark_icon("favourite", "unicode"), "◆");
    assert_eq!(c.mark_icon("favourite", "ascii"), "*");
    assert_eq!(
        c.mark_icon("favourite", "nerd"),
        "\u{eb59} ",
        "its own for nerd"
    );
    // none: its name's first letter
    let c = Config::parse("[marks.later]\n").unwrap();
    assert_eq!(c.mark_icon("later", "unicode"), "l");
}

/// Mark names are words (they're keys and `#name` filters); keys one
/// character; a top-level `favourite` list says where it went.
#[test]
fn bad_marks_are_explained() {
    // words: letters, digits, - and _
    assert!(Config::parse("[marks.my-work_2]\n").is_ok());
    assert!(err(Config::parse("[marks.\"a b\"]\n")).contains("letters, digits"));
    assert!(
        err(Config::parse("[marks.\"\"]\n")).contains("letters, digits"),
        "empty"
    );
    assert!(err(Config::parse("[marks.w]\nkey = 'ww'\n")).contains("key"));
    for taken in ["pull", "ignore"] {
        let e = err(Config::parse(&format!("[marks.{taken}]\n")));
        assert!(
            e.contains(taken) && e.contains("isn't a mark of its own"),
            "{e}"
        );
    }
    assert!(err(Config::parse("favourite = ['acme/app']\n")).contains("[marks.favourite] repos"));
    assert!(err(Config::parse("[marks.w]\nrepo = []\n")).contains("repo"));
}

/// Adding to a mark of your own writes its table.
#[test]
fn a_new_marks_entries_are_saved_in_its_table() {
    let dir = tempfile::tempdir().unwrap();
    let main = write(dir.path(), "config.toml", "[marks.work]\nkey = 'w'\n");
    let before = Config::load(&main).unwrap();
    let mut after = before.clone();
    after
        .marks
        .get_mut("work")
        .unwrap()
        .repos
        .push("acme/app".into());
    after
        .marks
        .get_mut("favourite")
        .unwrap()
        .repos
        .push("acme/fav".into());
    update_mark_lists(&main, &before, &after).unwrap();
    let c = Config::load(&main).unwrap();
    assert_eq!(c.marks["work"].repos, ["acme/app"]);
    assert_eq!(c.marks["favourite"].repos, ["acme/fav"]);
    assert_eq!(
        c.marks["work"].key,
        Some('w'),
        "the table's other keys kept"
    );
}
