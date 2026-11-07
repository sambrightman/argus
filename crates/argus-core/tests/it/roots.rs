//! `root add` / `root remove` across extended config files: add to the
//! highest file with `roots` or `extend_roots` (to `extend_roots` if it
//! has both, as that applies last),
//! else start `roots` at the top of the main file; remove from every
//! file that mentions the root.

use std::path::{Path, PathBuf};

use argus_core::config::Config;
use argus_core::configfile::{RootRemoved, add_roots, remove_roots};

/// A temp dir with directories `a`, `b`, `c` to use as roots, and the
/// files given (relative path, text), `{a}` etc. replaced by the real
/// paths. Returns the dir and the main file (`config.toml`).
fn setup(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    for d in ["a", "b", "c"] {
        std::fs::create_dir(t.path().join(d)).unwrap();
    }
    for (name, text) in files {
        let p = t.path().join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, fill(t.path(), text)).unwrap();
    }
    let main = t.path().join("config.toml");
    (t, main)
}

fn fill(dir: &Path, text: &str) -> String {
    let real = std::fs::canonicalize(dir).unwrap();
    ["a", "b", "c"].iter().fold(text.to_string(), |s, d| {
        s.replace(&format!("{{{d}}}"), &real.join(d).display().to_string())
    })
}

fn dir(t: &tempfile::TempDir, d: &str) -> PathBuf {
    std::fs::canonicalize(t.path()).unwrap().join(d)
}

fn read(t: &tempfile::TempDir, name: &str) -> String {
    std::fs::read_to_string(t.path().join(name)).unwrap()
}

/// The effective roots, as the directory names.
fn roots(main: &Path) -> Vec<String> {
    Config::load(main)
        .unwrap()
        .roots
        .iter()
        .map(|r| r.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn same(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

#[test]
fn add_goes_to_roots_in_the_highest_file_that_has_it() {
    // the main file only composes; the extended file holds the roots
    let (t, main) = setup(&[
        ("config.toml", "extend = ['inc.toml']\n"),
        ("inc.toml", "roots = ['{a}']\n"),
    ]);
    let before = read(&t, "config.toml");
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &t.path().join("inc.toml")), "{r:?}");
    assert_eq!(read(&t, "config.toml"), before, "main file untouched");
    assert_eq!(roots(&main), ["a", "b"]);
    assert!(r.replaces.is_empty(), "{r:?}");
    // inverse: the main file has roots too, so it's the highest with them
    let (t, main) = setup(&[
        ("config.toml", "extend = ['inc.toml']\nroots = ['{a}']\n"),
        ("inc.toml", "roots = ['{c}']\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &main), "{r:?}");
    assert_eq!(read(&t, "inc.toml"), fill(t.path(), "roots = ['{c}']\n"));
    assert_eq!(roots(&main), ["a", "b"]);
    // the extended file's roots were already replaced; that's reported
    assert_eq!(r.replaces.len(), 1, "{r:?}");
    assert!(same(&r.replaces[0], &t.path().join("inc.toml")), "{r:?}");
}

#[test]
fn a_later_extended_file_is_higher_than_an_earlier_one() {
    let (t, main) = setup(&[
        ("config.toml", "extend = ['x.toml', 'y.toml']\n"),
        ("x.toml", "roots = ['{a}']\n"),
        ("y.toml", "roots = ['{c}']\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &t.path().join("y.toml")), "{r:?}");
    assert_eq!(roots(&main), ["c", "b"]);
}

#[test]
fn a_file_is_higher_than_the_files_it_extends() {
    let (t, main) = setup(&[
        ("config.toml", "extend = ['mid.toml']\n"),
        ("mid.toml", "extend = ['low.toml']\n"),
        ("low.toml", "roots = ['{a}']\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &t.path().join("low.toml")), "{r:?}");
    // inverse: once the middle file has roots, it's higher. Its roots
    // replace low's, so `a` (only in low) isn't configured, and is added.
    std::fs::write(
        t.path().join("mid.toml"),
        fill(t.path(), "extend = ['low.toml']\nroots = ['{c}']\n"),
    )
    .unwrap();
    assert_eq!(roots(&main), ["c"]);
    let r = add_roots(&main, &[dir(&t, "a")]).unwrap();
    assert!(same(&r.file, &t.path().join("mid.toml")), "{r:?}");
    assert_eq!(roots(&main), ["c", "a"]);
}

#[test]
fn the_highest_file_with_either_key_gets_it() {
    // only extend_roots anywhere: it's appended to
    let (t, main) = setup(&[
        ("config.toml", "extend = ['inc.toml']\n"),
        ("inc.toml", "extend_roots = ['{a}']\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &t.path().join("inc.toml")), "{r:?}");
    let inc: toml::Table = toml::from_str(&read(&t, "inc.toml")).unwrap();
    assert_eq!(inc["extend_roots"].as_array().unwrap().len(), 2, "{inc}");
    assert_eq!(roots(&main), ["a", "b"]);
    // a shared base sets roots, this machine extends them: the addition
    // stays in the main file, where it also takes effect
    let (t, main) = setup(&[
        (
            "config.toml",
            "extend = ['work.toml']\nextend_roots = ['{a}']\n",
        ),
        ("work.toml", "roots = ['{c}']\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert!(same(&r.file, &main), "{r:?}");
    assert_eq!(
        read(&t, "work.toml"),
        fill(t.path(), "roots = ['{c}']\n"),
        "shared file untouched"
    );
    assert_eq!(roots(&main), ["c", "a", "b"]);
    assert!(r.replaces.is_empty(), "{r:?}");
    // a file with both: extend_roots, applied last (as for marks, where
    // order matters)
    let (t, main) = setup(&[("config.toml", "roots = ['{a}']\nextend_roots = ['{c}']\n")]);
    add_roots(&main, &[dir(&t, "b")]).unwrap();
    assert_eq!(roots(&main), ["a", "c", "b"]);
}

#[test]
fn with_neither_key_anywhere_roots_goes_at_the_top_of_the_main_file() {
    let (t, main) = setup(&[
        (
            "config.toml",
            "extend = ['inc.toml']\nmax_depth = 3\n[ui]\ntheme = 'dark'\n",
        ),
        ("inc.toml", "skip_hidden = false\n"),
    ]);
    let r = add_roots(&main, &[dir(&t, "a")]).unwrap();
    assert!(same(&r.file, &main), "{r:?}");
    let text = read(&t, "config.toml");
    assert!(text.starts_with("roots = ["), "{text}");
    let c = Config::load(&main).unwrap();
    assert_eq!(roots(&main), ["a"]);
    assert_eq!(
        (c.max_depth, c.ui.theme.as_str()),
        (3, "dark"),
        "rest intact"
    );
}

#[test]
fn already_configured_counts_every_file_and_every_spelling() {
    let (t, main) = setup(&[
        (
            "config.toml",
            "extend = ['inc.toml']\nextend_roots = ['{b}']\n",
        ),
        ("inc.toml", "roots = ['{a}']\n"),
    ]);
    assert_eq!(roots(&main), ["a", "b"]);
    std::os::unix::fs::symlink(t.path().join("a"), t.path().join("link-to-a")).unwrap();
    let (main_before, inc_before) = (read(&t, "config.toml"), read(&t, "inc.toml"));
    let r = add_roots(&main, &[t.path().join("link-to-a")]).unwrap();
    assert!(r.added.is_empty(), "{r:?}");
    assert_eq!(r.already.len(), 1, "{r:?}");
    assert!(same(&r.already[0].1, &t.path().join("inc.toml")), "{r:?}");
    assert_eq!(
        (read(&t, "config.toml"), read(&t, "inc.toml")),
        (main_before, inc_before)
    );
}

#[test]
fn remove_takes_a_root_out_of_every_file_that_mentions_it() {
    let (t, main) = setup(&[
        (
            "config.toml",
            "extend = ['inc.toml']\nextend_roots = ['{a}', '{b}']\n",
        ),
        ("inc.toml", "roots = ['{a}', '{c}']\n"),
    ]);
    let r = remove_roots(&main, &[dir(&t, "a")]).unwrap();
    assert!(r.not_roots.is_empty(), "{r:?}");
    let [
        RootRemoved {
            files, no_effect, ..
        },
    ] = r.removed.as_slice()
    else {
        panic!("{r:?}")
    };
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(no_effect.is_empty(), "{no_effect:?}");
    assert_eq!(roots(&main), ["c", "b"]);
    for f in ["config.toml", "inc.toml"] {
        let c = Config::parse(&read(&t, f)).unwrap();
        assert!(!c.roots.iter().any(|r| r.ends_with("a")), "{f}");
    }
}

#[test]
fn removing_from_an_overridden_file_is_reported_as_having_no_effect() {
    let (t, main) = setup(&[
        ("config.toml", "extend = ['inc.toml']\nroots = ['{c}']\n"),
        ("inc.toml", "roots = ['{a}', '{b}']\n"),
    ]);
    assert_eq!(
        roots(&main),
        ["c"],
        "the main file's roots replace the extended file's"
    );
    let r = remove_roots(&main, &[dir(&t, "a")]).unwrap();
    let [
        RootRemoved {
            files, no_effect, ..
        },
    ] = r.removed.as_slice()
    else {
        panic!("{r:?}")
    };
    assert!(same(&files[0], &t.path().join("inc.toml")));
    assert_eq!(no_effect, files);
    assert_eq!(read(&t, "inc.toml"), fill(t.path(), "roots = ['{b}']\n"));
    // inverse: the main file's own root is effective
    let r = remove_roots(&main, &[dir(&t, "c")]).unwrap();
    assert!(r.removed[0].no_effect.is_empty(), "{r:?}");
}

#[test]
fn a_directory_that_is_no_root_is_reported_and_the_others_still_removed() {
    let (t, main) = setup(&[("config.toml", "roots = ['{a}', '{b}']\n")]);
    let r = remove_roots(&main, &[dir(&t, "a"), dir(&t, "c")]).unwrap();
    assert_eq!(r.removed.len(), 1, "{r:?}");
    assert_eq!(r.not_roots.len(), 1, "{r:?}");
    assert_eq!(roots(&main), ["b"]);
    // nothing to remove at all is still fine
    let r = remove_roots(&main, &[dir(&t, "c")]).unwrap();
    assert!(r.removed.is_empty() && r.not_roots.len() == 1, "{r:?}");
}

#[test]
fn a_file_that_cant_be_written_stops_the_removal_before_any_change() {
    use std::os::unix::fs::PermissionsExt;
    let (t, main) = setup(&[
        (
            "config.toml",
            "extend = ['inc.toml']\nextend_roots = ['{a}']\n",
        ),
        ("inc.toml", "roots = ['{a}', '{b}']\n"),
    ]);
    let inc = t.path().join("inc.toml");
    std::fs::set_permissions(&inc, std::fs::Permissions::from_mode(0o444)).unwrap();
    let before = read(&t, "config.toml");
    assert!(remove_roots(&main, &[dir(&t, "a")]).is_err());
    assert_eq!(
        read(&t, "config.toml"),
        before,
        "the writable file wasn't changed either"
    );
    std::fs::set_permissions(&inc, std::fs::Permissions::from_mode(0o644)).unwrap();
}
