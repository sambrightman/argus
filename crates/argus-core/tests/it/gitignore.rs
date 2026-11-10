//! Ignore rules, checked against git: it must really ignore what we add.

use std::path::Path;

use argus_core::gitignore::IgnoreFile;
use argus_core::ops::{self, Op};
use argus_core::status::{StatusOptions, status};
use argus_core::testkit::{Sandbox, git, write};

fn untracked(r: &Path) -> Vec<String> {
    let s = status(r, StatusOptions::default()).unwrap();
    s.changes
        .files
        .iter()
        .filter(|f| f.index == '?')
        .map(|f| f.path.clone())
        .collect()
}

fn ignore(r: &Path, file: IgnoreFile, paths: &[&str], anchored: bool) -> String {
    let op = Op::Ignore {
        file,
        paths: paths.iter().map(|p| p.to_string()).collect(),
        anchored,
    };
    let out = ops::run(r, &op);
    assert!(out.success, "{}", out.output);
    out.output
}

/// `build/` and `sub/build/` (directories), `sub/out.log`, `notes.txt`.
fn repo(sb: &Sandbox) -> std::path::PathBuf {
    let r = sb.init("r");
    for f in [
        "build/x",
        "sub/build/y",
        "sub/out.log",
        "notes.txt",
        "sub/keep.txt",
    ] {
        write(&r, f, "x\n");
    }
    git(&r, &["add", "sub/keep.txt"]);
    git(&r, &["commit", "-q", "-m", "keep"]);
    r
}

#[test]
fn anchored_rules_ignore_only_that_path() {
    let sb = Sandbox::new();
    let r = repo(&sb);
    assert_eq!(
        untracked(&r),
        ["build/", "notes.txt", "sub/build/", "sub/out.log"]
    );
    let out = ignore(&r, IgnoreFile::TopLevel, &["build/", "notes.txt"], true);
    assert!(out.contains("/build/ in "), "{out}");
    assert_eq!(
        std::fs::read_to_string(r.join(".gitignore")).unwrap(),
        "/build/\n/notes.txt\n"
    );
    // sub/build/ isn't the top-level build/
    assert_eq!(untracked(&r), [".gitignore", "sub/build/", "sub/out.log"]);
    // added again: nothing new
    assert_eq!(ignore(&r, IgnoreFile::TopLevel, &["build/"], true), "");
    assert_eq!(
        std::fs::read_to_string(r.join(".gitignore")).unwrap(),
        "/build/\n/notes.txt\n"
    );
}

#[test]
fn unanchored_rules_ignore_the_name_anywhere() {
    let sb = Sandbox::new();
    let r = repo(&sb);
    ignore(&r, IgnoreFile::TopLevel, &["build/"], false);
    assert_eq!(
        std::fs::read_to_string(r.join(".gitignore")).unwrap(),
        "build/\n"
    );
    assert_eq!(untracked(&r), [".gitignore", "notes.txt", "sub/out.log"]);
}

#[test]
fn a_directorys_own_gitignore_is_relative_to_it() {
    let sb = Sandbox::new();
    let r = repo(&sb);
    // a file without a final newline gets one first
    std::fs::write(r.join("sub/.gitignore"), "*.tmp").unwrap();
    ignore(
        &r,
        IgnoreFile::Directory,
        &["sub/out.log", "notes.txt"],
        true,
    );
    assert_eq!(
        std::fs::read_to_string(r.join("sub/.gitignore")).unwrap(),
        "*.tmp\n/out.log\n"
    );
    assert_eq!(
        std::fs::read_to_string(r.join(".gitignore")).unwrap(),
        "/notes.txt\n"
    );
    assert_eq!(
        untracked(&r),
        [".gitignore", "build/", "sub/.gitignore", "sub/build/"]
    );
}

#[test]
fn private_and_global_files_are_where_git_reads_them() {
    let sb = Sandbox::new();
    let r = repo(&sb);
    ignore(&r, IgnoreFile::Private, &["notes.txt"], true);
    assert_eq!(
        std::fs::read_to_string(r.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .last(),
        Some("/notes.txt")
    );
    let global = sb.path("global/ignore");
    git(
        &r,
        &["config", "core.excludesFile", global.to_str().unwrap()],
    );
    ignore(&r, IgnoreFile::Global, &["sub/out.log"], false);
    assert_eq!(std::fs::read_to_string(&global).unwrap(), "out.log\n");
    // nothing shared changed
    assert!(!r.join(".gitignore").exists());
    assert_eq!(untracked(&r), ["build/", "sub/build/"]);
}

#[test]
fn awkward_names_are_ignored_exactly() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    for f in ["#hash", "!bang", "star*.txt", "starry.txt"] {
        write(&r, f, "x\n");
    }
    ignore(
        &r,
        IgnoreFile::TopLevel,
        &["#hash", "!bang", "star*.txt"],
        false,
    );
    // the glob was taken literally: starry.txt is still there
    assert_eq!(untracked(&r), [".gitignore", "starry.txt"]);
}
