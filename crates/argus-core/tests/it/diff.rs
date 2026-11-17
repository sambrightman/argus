//! File diffs (read with gix) and the file operations: stage, unstage,
//! discard, run for real.

use std::path::Path;

use argus_core::diff::{FileDiff, LineKind, Side, file_diff};
use argus_core::ops;
use argus_core::status::{RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};

fn st(r: &Path) -> RepoStatus {
    status(r, StatusOptions::default()).unwrap()
}

/// The diff as `git diff` would print its body: hunk headers, then lines
/// prefixed with ` `, `-` or `+`.
fn text(d: &FileDiff) -> Vec<String> {
    let (FileDiff::Text(hunks) | FileDiff::Converted(hunks)) = d else {
        panic!("not text: {d:?}")
    };
    let mut v = vec![];
    for h in hunks {
        v.push(h.header());
        for l in &h.lines {
            let c = match l.kind {
                LineKind::Context => ' ',
                LineKind::Add => '+',
                LineKind::Remove => '-',
            };
            v.push(format!("{c}{}", l.text));
        }
    }
    v
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

#[test]
fn unstaged_and_staged_sides() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", &lines(10));
    commit(&r, "f");
    // staged: line 2 changed; unstaged on top: line 9 changed
    write(&r, "f", &lines(10).replace("line 2\n", "line two\n"));
    git(&r, &["add", "f"]);
    write(
        &r,
        "f",
        &lines(10)
            .replace("line 2\n", "line two\n")
            .replace("line 9\n", "line nine\n"),
    );
    let staged = text(&file_diff(&r, "f", Side::Staged).unwrap());
    assert_eq!(
        staged,
        [
            "@@ -1,5 +1,5 @@",
            " line 1",
            "-line 2",
            "+line two",
            " line 3",
            " line 4",
            " line 5"
        ]
    );
    let unstaged = text(&file_diff(&r, "f", Side::Unstaged).unwrap());
    assert_eq!(
        unstaged,
        [
            "@@ -6,5 +6,5 @@",
            " line 6",
            " line 7",
            " line 8",
            "-line 9",
            "+line nine",
            " line 10"
        ]
    );
    // inverse: a file with no changes on a side has no hunks
    git(&r, &["add", "f"]);
    assert_eq!(
        file_diff(&r, "f", Side::Unstaged).unwrap(),
        FileDiff::Text(vec![])
    );
}

#[test]
fn added_deleted_and_untracked_files() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "new", "a\nb\n");
    assert_eq!(
        text(&file_diff(&r, "new", Side::Unstaged).unwrap()),
        ["@@ -0,0 +1,2 @@", "+a", "+b"]
    );
    git(&r, &["add", "new"]);
    assert_eq!(
        text(&file_diff(&r, "new", Side::Staged).unwrap()),
        ["@@ -0,0 +1,2 @@", "+a", "+b"]
    );
    std::fs::remove_file(r.join("README")).unwrap();
    assert_eq!(
        text(&file_diff(&r, "README", Side::Unstaged).unwrap()),
        ["@@ -1 +0,0 @@", "-hello"]
    );
}

#[test]
fn binary_files_and_filters() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    std::fs::write(r.join("bin"), [0u8, 1, 2, 0, 255]).unwrap();
    git(&r, &["add", "bin"]);
    assert_eq!(
        file_diff(&r, "bin", Side::Staged).unwrap(),
        FileDiff::Binary
    );
    // with core.autocrlf, CRLF in the work tree is LF in git: no change
    git(&r, &["config", "core.autocrlf", "true"]);
    write(&r, "README", "hello\r\n");
    assert_eq!(
        file_diff(&r, "README", Side::Unstaged).unwrap(),
        FileDiff::Text(vec![])
    );
    // with autocrlf=input, CRLF is cleaned to LF on the way into git, and
    // never added back: still no change, as `git diff` says
    git(&r, &["config", "core.autocrlf", "input"]);
    assert_eq!(
        file_diff(&r, "README", Side::Unstaged).unwrap(),
        FileDiff::Text(vec![])
    );
    assert!(git_body(&r, &["--", "README"]).is_empty());
    // inverse: a real change still shows
    write(&r, "README", "bye\r\n");
    assert_eq!(
        text(&file_diff(&r, "README", Side::Unstaged).unwrap()),
        ["@@ -1 +1 @@", "-hello", "+bye"]
    );
}

/// A repo with: `README` modified, `staged` modified and staged, `added`
/// newly staged, `gone` deleted, `new` untracked.
fn messy(sb: &Sandbox) -> std::path::PathBuf {
    let r = sb.init("m");
    write(&r, "staged", "s\n");
    write(&r, "gone", "g\n");
    git(&r, &["add", "."]);
    commit(&r, "files");
    write(&r, "README", "changed\n");
    write(&r, "staged", "s2\n");
    write(&r, "added", "a\n");
    git(&r, &["add", "staged", "added"]);
    std::fs::remove_file(r.join("gone")).unwrap();
    write(&r, "new", "n\n");
    r
}

fn codes(r: &Path) -> Vec<String> {
    let mut v: Vec<String> = st(r)
        .changes
        .files
        .iter()
        .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
        .collect();
    v.sort();
    v
}

fn run_all(r: &Path, ops: Vec<ops::Op>) {
    assert!(!ops.is_empty());
    for op in ops {
        let out = ops::run(r, &op);
        assert!(out.success, "{op:?}: {}", out.output);
    }
}

fn all(r: &Path) -> Vec<String> {
    st(r).changes.files.iter().map(|f| f.path.clone()).collect()
}

#[test]
fn stage_and_unstage_files() {
    let sb = Sandbox::new();
    let r = messy(&sb);
    assert_eq!(
        codes(&r),
        [" D gone", " M README", "?? new", "A  added", "M  staged"]
    );
    run_all(&r, ops::stage_ops(&st(&r), &all(&r)));
    assert_eq!(
        codes(&r),
        ["A  added", "A  new", "D  gone", "M  README", "M  staged"]
    );
    // unstage keeps the work tree, for new files too
    run_all(&r, ops::unstage_ops(&st(&r), &all(&r)));
    assert_eq!(
        codes(&r),
        [" D gone", " M README", "?? added", "?? new", " M staged"]
            .map(String::from)
            .to_vec()
            .tap_sort()
    );
    // files with nothing staged (unstaged or untracked) need no unstaging
    assert_eq!(
        ops::unstage_ops(&st(&r), &["README".into(), "new".into()]),
        []
    );
    // only the files given
    run_all(&r, ops::stage_ops(&st(&r), &["README".into()]));
    assert!(codes(&r).contains(&"M  README".into()), "{:?}", codes(&r));
    assert!(codes(&r).contains(&" M staged".into()), "{:?}", codes(&r));
}

/// A file tracked inside a gitignored directory (added with `-f`): plain
/// `git add` stages it but exits 1, refusing the ignored path.
#[test]
fn a_tracked_file_in_an_ignored_directory_stages() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, ".gitignore", ".claude\n");
    write(&r, ".claude/s.sh", "a\n");
    git(&r, &["add", ".gitignore"]);
    git(&r, &["add", "-f", ".claude/s.sh"]);
    commit(&r, "i");
    write(&r, ".claude/gone", "x\n");
    git(&r, &["add", "-f", ".claude/gone"]);
    commit(&r, "gone");
    write(&r, ".claude/s.sh", "b\n");
    std::fs::remove_file(r.join(".claude/gone")).unwrap();
    write(&r, "new", "something else\n");
    assert_eq!(codes(&r), [" D .claude/gone", " M .claude/s.sh", "?? new"]);
    run_all(&r, ops::stage_ops(&st(&r), &all(&r)));
    assert_eq!(codes(&r), ["A  new", "D  .claude/gone", "M  .claude/s.sh"]);
}

#[test]
fn unstaging_works_before_the_first_commit() {
    let sb = Sandbox::new();
    let r = sb.path("empty");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    write(&r, "a", "1\n");
    git(&r, &["add", "a"]);
    run_all(&r, ops::unstage_ops(&st(&r), &["a".into()]));
    assert_eq!(codes(&r), ["?? a"]);
}

#[test]
fn discarding_works_before_the_first_commit() {
    let sb = Sandbox::new();
    let r = sb.path("empty");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    write(&r, "a", "1\n");
    git(&r, &["add", "a"]);
    // staged only: nothing unstaged to discard, and it's left alone
    assert!(ops::discard_ops(&st(&r), &["a".into()]).is_empty());
    write(&r, "a", "2\n");
    run_all(&r, ops::discard_ops(&st(&r), &["a".into()]));
    assert_eq!(codes(&r), ["A  a"]);
    assert_eq!(std::fs::read_to_string(r.join("a")).unwrap(), "1\n");
}

#[test]
fn discard_drops_unstaged_changes_and_never_what_is_staged() {
    let sb = Sandbox::new();
    let r = messy(&sb);
    // more work on top of what's staged
    write(&r, "staged", "s3\n");
    write(&r, "added", "a2\n");
    assert_eq!(
        codes(&r),
        [" D gone", " M README", "?? new", "AM added", "MM staged"]
            .map(String::from)
            .to_vec()
            .tap_sort()
    );
    run_all(&r, ops::discard_ops(&st(&r), &all(&r)));
    // the work tree is as the index has it: staged work kept, to the byte
    assert_eq!(codes(&r), ["A  added", "M  staged"]);
    assert_eq!(std::fs::read_to_string(r.join("staged")).unwrap(), "s2\n");
    assert_eq!(std::fs::read_to_string(r.join("added")).unwrap(), "a\n");
    assert_eq!(
        std::fs::read_to_string(r.join("README")).unwrap(),
        "hello\n"
    );
    assert_eq!(std::fs::read_to_string(r.join("gone")).unwrap(), "g\n");
    assert!(!r.join("new").exists());
    // inverse: other files are untouched
    let sb = Sandbox::new();
    let r = messy(&sb);
    run_all(
        &r,
        ops::discard_ops(&st(&r), &["README".into(), "new".into()]),
    );
    assert_eq!(codes(&r), [" D gone", "A  added", "M  staged"]);
}

#[test]
fn a_conflicted_file_is_discarded_to_head() {
    let sb = Sandbox::new();
    let r = sb.init("c");
    git(&r, &["switch", "-q", "-c", "other"]);
    write(&r, "README", "theirs\n");
    commit(&r, "theirs");
    git(&r, &["switch", "-q", "main"]);
    write(&r, "README", "ours\n");
    commit(&r, "ours");
    assert!(!argus_core::testkit::git_may_fail(
        &r,
        &["merge", "-q", "other"]
    ));
    assert_eq!(codes(&r), ["UU README"]);
    run_all(&r, ops::discard_ops(&st(&r), &["README".into()]));
    assert_eq!(std::fs::read_to_string(r.join("README")).unwrap(), "ours\n");
}

#[test]
fn paths_are_taken_literally_not_as_patterns() {
    let sb = Sandbox::new();
    let r = sb.init("p");
    for f in ["notes1.txt", "notes[1].txt"] {
        write(&r, f, "v1\n");
    }
    git(&r, &["add", "."]);
    commit(&r, "notes");
    for f in ["notes1.txt", "notes[1].txt", "a.bak", "*.bak"] {
        write(&r, f, "v2\n");
    }
    let only = |f: &str| vec![f.to_string()];
    // staging the oddly named one stages only it
    run_all(&r, ops::stage_ops(&st(&r), &only("notes[1].txt")));
    run_all(&r, ops::stage_ops(&st(&r), &only("*.bak")));
    assert_eq!(
        codes(&r),
        [" M notes1.txt", "?? a.bak", "A  *.bak", "M  notes[1].txt"]
    );
    run_all(&r, ops::unstage_ops(&st(&r), &only("notes[1].txt")));
    run_all(&r, ops::unstage_ops(&st(&r), &only("*.bak")));
    // and discarding it leaves the ones a pattern would have matched
    run_all(&r, ops::discard_ops(&st(&r), &only("notes[1].txt")));
    run_all(&r, ops::discard_ops(&st(&r), &only("*.bak")));
    assert_eq!(codes(&r), [" M notes1.txt", "?? a.bak"]);
    assert!(r.join("a.bak").exists() && !r.join("*.bak").exists());
}

#[test]
fn file_op_args_keep_paths_after_double_dash() {
    for op in [
        ops::Op::Add {
            paths: vec!["-f".into()],
        },
        ops::Op::Restore {
            staged: true,
            worktree: true,
            from_head: true,
            paths: vec!["-f".into()],
        },
        ops::Op::Remove {
            cached: true,
            paths: vec!["-f".into()],
        },
        ops::Op::Clean {
            paths: vec!["-f".into()],
        },
    ] {
        let a = op.args();
        let dd = a.iter().position(|s| s == "--").expect("--");
        assert_eq!(a.last().unwrap(), ":(literal)-f");
        assert!(dd < a.len() - 1, "{a:?}");
    }
}

trait TapSort {
    fn tap_sort(self) -> Self;
}

impl TapSort for Vec<String> {
    fn tap_sort(mut self) -> Self {
        self.sort();
        self
    }
}

/// Hunks as `git diff` prints them, without the file headers and "\ No
/// newline" markers.
fn git_body(r: &Path, args: &[&str]) -> Vec<String> {
    let mut a = vec!["diff", "--no-color", "--no-ext-diff", "-U3"];
    a.extend(args);
    git(r, &a)
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .filter(|l| !l.starts_with('\\'))
        .map(|l| match l.find(" @@") {
            // drop the function context git appends to headers
            Some(i) if l.starts_with("@@") => l[..i + 3].to_string(),
            _ => l.to_string(),
        })
        .collect()
}

#[test]
fn hunks_match_git_diff() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", &lines(40));
    commit(&r, "f");
    let cases: Vec<(&str, String)> = vec![
        (
            "two hunks",
            lines(40)
                .replace("line 3\n", "x\n")
                .replace("line 30\n", "y\n"),
        ),
        ("insert at start", format!("new\n{}", lines(40))),
        ("append at end", format!("{}more\nlines\n", lines(40))),
        (
            "delete a run",
            lines(40).replace("line 10\nline 11\nline 12\n", ""),
        ),
        ("no newline at end", lines(40).trim_end().to_string()),
        ("replace all", "other\n".to_string()),
    ];
    for (name, content) in cases {
        write(&r, "f", &content);
        assert_eq!(
            text(&file_diff(&r, "f", Side::Unstaged).unwrap()),
            git_body(&r, &["--", "f"]),
            "{name} (unstaged)"
        );
        git(&r, &["add", "f"]);
        assert_eq!(
            text(&file_diff(&r, "f", Side::Staged).unwrap()),
            git_body(&r, &["--cached", "--", "f"]),
            "{name} (staged)"
        );
        git(&r, &["reset", "-q", "--hard"]);
    }
    // the missing newline is reported on the line that lacks it
    write(&r, "f", lines(40).trim_end());
    let FileDiff::Text(h) = file_diff(&r, "f", Side::Unstaged).unwrap() else {
        panic!()
    };
    let flagged: Vec<(&str, bool)> = h[0]
        .lines
        .iter()
        .map(|l| (l.text.as_str(), l.no_newline))
        .collect();
    assert_eq!(
        &flagged[flagged.len() - 2..],
        [("line 40", false), ("line 40", true)]
    );
}

#[test]
fn a_commits_changes_to_a_file() {
    use argus_core::diff::commit_file_diff;
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a.txt", "one\ntwo\nthree\n");
    write(&r, "old.txt", "moved content\nline 2\nline 3\n");
    git(&r, &["add", "."]);
    git(&r, &["commit", "-q", "-m", "setup"]);
    write(&r, "a.txt", "one\nTWO\nthree\n");
    write(&r, "b.txt", "new\n");
    git(&r, &["mv", "old.txt", "new.txt"]);
    git(&r, &["add", "."]);
    git(&r, &["commit", "-q", "-m", "change"]);
    let lines = |d: FileDiff| -> Vec<String> {
        let FileDiff::Text(h) = d else {
            panic!("{d:?}")
        };
        h.iter()
            .flat_map(|h| {
                std::iter::once(h.header()).chain(h.lines.iter().map(|l| {
                    let m = match l.kind {
                        LineKind::Add => '+',
                        LineKind::Remove => '-',
                        LineKind::Context => ' ',
                    };
                    format!("{m}{}", l.text)
                }))
            })
            .collect()
    };
    assert_eq!(
        lines(commit_file_diff(&r, "HEAD", "a.txt").unwrap()),
        ["@@ -1,3 +1,3 @@", " one", "-two", "+TWO", " three"]
    );
    // added: against nothing
    assert_eq!(
        lines(commit_file_diff(&r, "HEAD", "b.txt").unwrap()),
        ["@@ -0,0 +1 @@", "+new"]
    );
    // renamed unchanged: no hunks
    assert_eq!(
        lines(commit_file_diff(&r, "HEAD", "old.txt -> new.txt").unwrap()),
        Vec::<String>::new()
    );
    // deleted, seen from the commit that deleted it
    git(&r, &["rm", "-q", "b.txt"]);
    git(&r, &["commit", "-q", "-m", "drop"]);
    assert_eq!(
        lines(commit_file_diff(&r, "HEAD", "b.txt").unwrap()),
        ["@@ -1 +0,0 @@", "-new"]
    );
    // inverse: an unknown commit is an error
    assert!(commit_file_diff(&r, "nope", "a.txt").is_err());
}

/// Staging never adds a nested repo (git would embed it): its own line is
/// skipped, and a directory holding it and files stages just the files.
#[test]
fn staging_leaves_nested_repos_out() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    sb.init("r/top");
    write(&r, "mixed/file", "x\n");
    sb.init("r/mixed/inner");
    let s = st(&r);
    assert_eq!(ops::stage_ops(&s, &["top".into()]), []);
    run_all(&r, ops::stage_ops(&s, &["mixed/".into()]));
    assert_eq!(
        git(&r, &["ls-files", "--stage", "--", "mixed", "top"])
            .lines()
            .count(),
        1
    );
    assert!(git(&r, &["ls-files", "--", "mixed"]).contains("mixed/file"));
}

fn hunks(r: &Path, file: &str, side: Side) -> Vec<argus_core::diff::Hunk> {
    match file_diff(r, file, side).unwrap() {
        FileDiff::Text(h) => h,
        other => panic!("{other:?}"),
    }
}

/// What `apply` of part of a hunk does to the index (`index`) or the work
/// tree, then the file's contents in each.
fn apply(
    r: &Path,
    file: &str,
    side: Side,
    hunk: usize,
    only: Option<&[usize]>,
    index: bool,
    reverse: bool,
) {
    let h = &hunks(r, file, side)[hunk];
    let op = ops::Op::Apply {
        patch: argus_core::diff::patch(file, h, only, reverse),
        index,
        reverse,
        what: "test".into(),
    };
    let o = ops::run(r, &op);
    assert!(o.success, "{}", o.output);
}

/// Staging, unstaging and discarding a hunk, or single lines of one.
#[test]
fn hunks_and_lines_stage_unstage_and_discard() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", &lines(30));
    commit(&r, "lines");
    // three changes far enough apart for three hunks
    let changed = lines(30)
        .replace("line 2\n", "line two\n")
        .replace("line 15\n", "line fifteen\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&r, "f", &changed);
    assert_eq!(hunks(&r, "f", Side::Unstaged).len(), 3);
    // stage the middle hunk
    apply(&r, "f", Side::Unstaged, 1, None, true, false);
    let staged = git(&r, &["diff", "--cached", "-U0", "--", "f"]);
    assert!(
        staged.contains("+line fifteen") && !staged.contains("line two"),
        "{staged}"
    );
    assert_eq!(hunks(&r, "f", Side::Unstaged).len(), 2);
    // unstage it again
    apply(&r, "f", Side::Staged, 0, None, true, true);
    assert_eq!(git(&r, &["diff", "--cached", "--", "f"]), "");
    // one line of a replacement: just the added line, the removed one kept
    let h = &hunks(&r, "f", Side::Unstaged)[0];
    let add = h
        .lines
        .iter()
        .position(|l| l.kind == LineKind::Add)
        .unwrap();
    apply(&r, "f", Side::Unstaged, 0, Some(&[add]), true, false);
    let index = git(&r, &["show", ":f"]);
    assert!(index.contains("line 2\nline two\nline 3\n"), "{index}");
    // discard a hunk from the work tree: the last one (the added line)
    let n = hunks(&r, "f", Side::Unstaged).len();
    apply(&r, "f", Side::Unstaged, n - 1, None, false, true);
    let work = std::fs::read_to_string(r.join("f")).unwrap();
    assert!(
        !work.contains("added") && work.contains("line fifteen"),
        "{work}"
    );
    // unstage just the removed line of the staged replacement (none was
    // staged: the index keeps line 2 and gained line two)
    let h = &hunks(&r, "f", Side::Staged)[0];
    let added = h
        .lines
        .iter()
        .position(|l| l.kind == LineKind::Add)
        .unwrap();
    apply(&r, "f", Side::Staged, 0, Some(&[added]), true, true);
    assert_eq!(git(&r, &["diff", "--cached", "--", "f"]), "");
}

#[test]
fn a_hunk_at_the_end_without_a_newline_stages() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", "a\nb");
    commit(&r, "f");
    write(&r, "f", "a\nc");
    apply(&r, "f", Side::Unstaged, 0, None, true, false);
    assert_eq!(git(&r, &["show", ":f"]), "a\nc");
}

/// Several lines, from different hunks, in one patch.
#[test]
fn lines_from_several_hunks_stage_together() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", &lines(30));
    commit(&r, "lines");
    let changed = lines(30)
        .replace("line 2\n", "line two\n")
        .replace("line 3\n", "line three\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&r, "f", &changed);
    let h = hunks(&r, "f", Side::Unstaged);
    assert_eq!(h.len(), 2);
    let pos = |h: &argus_core::diff::Hunk, text: &str| {
        h.lines.iter().position(|l| l.text == text).unwrap()
    };
    // "line three" (keeping line 3's removal out) and "added"
    let first = [pos(&h[0], "line three")];
    let second = [pos(&h[1], "added")];
    let op = ops::Op::Apply {
        patch: argus_core::diff::patch_hunks(
            "f",
            &[(&h[0], Some(&first[..])), (&h[1], Some(&second[..]))],
            false,
        ),
        index: true,
        reverse: false,
        what: "test".into(),
    };
    let o = ops::run(&r, &op);
    assert!(o.success, "{}", o.output);
    let index = git(&r, &["show", ":f"]);
    assert!(
        index.contains("line 2\nline 3\nline three\nline 4\n"),
        "{index}"
    );
    assert!(index.contains("line 28\nadded\n"), "{index}");
    assert!(!index.contains("line two"), "{index}");
}

#[test]
fn textconv_is_applied_and_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let r = sb.init("r");
    let conv = sb.path("conv.sh");
    std::fs::write(&conv, "#!/bin/sh\ntr -d '\\000' < \"$1\" | tr a-z A-Z\n").unwrap();
    std::fs::set_permissions(&conv, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&r, ".gitattributes", "*.dat diff=upper\n");
    std::fs::write(r.join("f.dat"), b"\0one\ntwo\n").unwrap();
    git(&r, &["add", "."]);
    commit(&r, "data");
    std::fs::write(r.join("f.dat"), b"\0one\nthree\n").unwrap();
    // inverse: no program configured for the driver, so it's just binary
    assert_eq!(
        file_diff(&r, "f.dat", Side::Unstaged).unwrap(),
        FileDiff::Binary
    );
    git(
        &r,
        &["config", "diff.upper.textconv", conv.to_str().unwrap()],
    );
    let lines = |d: FileDiff| {
        assert!(matches!(d, FileDiff::Converted(_)), "{d:?}");
        text(&d)
    };
    let want = ["@@ -1,2 +1,2 @@", " ONE", "-TWO", "+THREE"];
    assert_eq!(lines(file_diff(&r, "f.dat", Side::Unstaged).unwrap()), want);
    // as git shows it
    let body = git_body(&r, &["--", "f.dat"]);
    assert_eq!(body, want, "{body:?}");
    git(&r, &["add", "f.dat"]);
    assert_eq!(lines(file_diff(&r, "f.dat", Side::Staged).unwrap()), want);
    commit(&r, "three");
    assert_eq!(
        lines(argus_core::diff::commit_file_diff(&r, "HEAD", "f.dat").unwrap()),
        want
    );
    // inverse: a file the driver doesn't cover stays an ordinary diff
    write(&r, "README", "changed\n");
    assert!(matches!(
        file_diff(&r, "README", Side::Unstaged).unwrap(),
        FileDiff::Text(_)
    ));
}

/// Apply the lines of `file`'s `side` diff written as `picks` (`-b`,
/// `+B`: a removed or an added line by its text), from whichever hunks
/// they're in, to the index or the work tree.
fn apply_lines(r: &Path, file: &str, side: Side, picks: &[&str], index: bool, reverse: bool) {
    let hunks = hunks(r, file, side);
    let mut left: Vec<&str> = picks.to_vec();
    let chosen: Vec<Vec<usize>> = hunks
        .iter()
        .map(|h| {
            let lines = h.lines.iter().enumerate();
            lines
                .filter(|(_, l)| {
                    let sign = match l.kind {
                        LineKind::Add => '+',
                        LineKind::Remove => '-',
                        LineKind::Context => return false,
                    };
                    let pick = format!("{sign}{}", l.text);
                    let found = left.iter().position(|p| **p == pick);
                    found.map(|i| left.remove(i)).is_some()
                })
                .map(|(at, _)| at)
                .collect()
        })
        .collect();
    assert!(left.is_empty(), "not in the diff: {left:?}");
    let parts: Vec<(&argus_core::diff::Hunk, Option<&[usize]>)> = hunks
        .iter()
        .zip(&chosen)
        .filter(|(_, at)| !at.is_empty())
        .map(|(h, at)| (h, Some(at.as_slice())))
        .collect();
    let op = ops::Op::Apply {
        patch: argus_core::diff::patch_hunks(file, &parts, reverse),
        index,
        reverse,
        what: "test".into(),
    };
    let o = ops::run(r, &op);
    assert!(o.success, "{}", o.output);
}

fn repo_with(sb: &Sandbox, name: &str, committed: &str, now: &str) -> std::path::PathBuf {
    let r = sb.init(name);
    std::fs::write(r.join("f"), committed).unwrap();
    commit(&r, "f");
    std::fs::write(r.join("f"), now).unwrap();
    r
}

fn in_index(r: &Path) -> String {
    git(r, &["show", ":f"])
}

fn in_work_tree(r: &Path) -> String {
    std::fs::read_to_string(r.join("f")).unwrap()
}

/// Lines taken from beside a last line that has no newline leave that
/// line whole: nothing is joined to it.
#[test]
fn lines_beside_a_last_line_without_a_newline_keep_it_whole() {
    let sb = Sandbox::new();
    // the last line gains its newline, and a line before it changes:
    // staging the changed line leaves the last one as it was
    let r = repo_with(&sb, "a", "a\nb\nc", "a\nB\nc\n");
    apply_lines(&r, "f", Side::Unstaged, &["-b", "+B"], true, false);
    assert_eq!(in_index(&r), "a\nB\nc");
    // and staging just the newline leaves the other line
    let r = repo_with(&sb, "b", "a\nb\nc", "a\nB\nc\n");
    apply_lines(&r, "f", Side::Unstaged, &["-c", "+c"], true, false);
    assert_eq!(in_index(&r), "a\nb\nc\n");
    // discarded from a work tree whose last line has none either
    let r = repo_with(&sb, "c", "a\nb\nc", "a\nB\nc\nd");
    apply_lines(&r, "f", Side::Unstaged, &["-c", "+c", "+d"], false, true);
    assert_eq!(in_work_tree(&r), "a\nB\nc");
    // a line added after one that had no newline gives it one
    let r = repo_with(&sb, "d", "a\nc", "a\nc\nd\n");
    apply_lines(&r, "f", Side::Unstaged, &["+d"], true, false);
    assert_eq!(in_index(&r), "a\nc\nd\n");
    // the same undone: the line taken away, the one before keeps the
    // newline it has there
    let r = repo_with(&sb, "e", "a\nc", "a\nc\nd\n");
    apply_lines(&r, "f", Side::Unstaged, &["+d"], false, true);
    assert_eq!(in_work_tree(&r), "a\nc\n");
    // (and a last line added without one keeps being without)
    let r = repo_with(&sb, "f", "a\nc", "a\nc2\nd");
    apply_lines(&r, "f", Side::Unstaged, &["+d"], true, false);
    assert_eq!(in_index(&r), "a\nc\nd");
    // a new last line without a newline, a line after it kept: still
    // lines of their own
    let r = repo_with(&sb, "g", "a\nb\nc\n", "a\nB\nd");
    apply_lines(&r, "f", Side::Unstaged, &["-b", "+B", "+d"], true, false);
    assert_eq!(in_index(&r), "a\nB\nd\nc\n");
}

/// Lines go where they are in the file, whatever of the file's other
/// changes is left out: before them (which moves them), or beside them.
#[test]
fn lines_land_where_they_are_whatever_is_left_out() {
    let sb = Sandbox::new();
    let x = |n: usize| "x\n".repeat(n);
    // five lines added early and left out; one added later, among lines
    // all alike, staged
    let now = format!("{}{}{}added\n{}", x(5), "y\n".repeat(5), x(35), x(20));
    let r = repo_with(&sb, "a", &x(60), &now);
    apply_lines(&r, "f", Side::Unstaged, &["+added"], true, false);
    assert_eq!(in_index(&r), format!("{}added\n{}", x(40), x(20)));
    // one added early and kept; a line removed later, put back
    let committed = format!("{}gone\n{}", x(40), x(20));
    let now = format!("{}added\n{}{}", x(5), x(35), x(20));
    let r = repo_with(&sb, "b", &committed, &now);
    apply_lines(&r, "f", Side::Unstaged, &["-gone"], false, true);
    assert_eq!(
        in_work_tree(&r),
        format!("{}added\n{}gone\n{}", x(5), x(35), x(20))
    );
    // two taken at once: the later one goes by where the earlier leaves
    // it, staged (five lines further down) and undone (one line back up)
    let both = format!("{}{}{}added\n{}", x(5), "y\n".repeat(5), x(35), x(20));
    let r = repo_with(&sb, "c", &x(60), &both);
    let picks = ["+y", "+y", "+y", "+y", "+y", "+added"];
    apply_lines(&r, "f", Side::Unstaged, &picks, true, false);
    assert_eq!(in_index(&r), both);
    let r = repo_with(&sb, "c2", &committed, &now);
    apply_lines(&r, "f", Side::Unstaged, &["+added", "-gone"], false, true);
    assert_eq!(in_work_tree(&r), committed);
    // one of two lines replaced side by side: the new line where the old
    // one was
    let now = lines(6).replace("line 3\nline 4\n", "line three\nline four\n");
    let r = repo_with(&sb, "d", &lines(6), &now);
    apply_lines(
        &r,
        "f",
        Side::Unstaged,
        &["-line 3", "+line three"],
        true,
        false,
    );
    assert_eq!(in_index(&r), lines(6).replace("line 3\n", "line three\n"));
    // and the other one's undone in the work tree
    apply_lines(
        &r,
        "f",
        Side::Unstaged,
        &["-line 4", "+line four"],
        false,
        true,
    );
    assert_eq!(
        in_work_tree(&r),
        lines(6).replace("line 3\n", "line three\n")
    );
}

/// A file kept with CRLF line endings: its lines are staged with them.
#[test]
fn lines_of_a_file_with_crlf_endings_keep_them() {
    let sb = Sandbox::new();
    let r = repo_with(&sb, "r", "a\r\nb\r\nc\r\n", "a\r\nB\r\nc\r\nd\r\n");
    apply_lines(&r, "f", Side::Unstaged, &["+d"], true, false);
    assert_eq!(in_index(&r), "a\r\nb\r\nc\r\nd\r\n");
    apply_lines(&r, "f", Side::Unstaged, &["-b", "+B"], false, true);
    assert_eq!(in_work_tree(&r), "a\r\nb\r\nc\r\nd\r\n");
}
