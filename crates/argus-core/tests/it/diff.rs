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
    let FileDiff::Text(hunks) = d else {
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
    run_all(&r, ops::discard_ops(&st(&r), &["a".into()]));
    assert!(codes(&r).is_empty(), "{:?}", codes(&r));
    assert!(!r.join("a").exists());
}

#[test]
fn discard_returns_files_to_head() {
    let sb = Sandbox::new();
    let r = messy(&sb);
    run_all(&r, ops::discard_ops(&st(&r), &all(&r)));
    assert!(codes(&r).is_empty(), "{:?}", codes(&r));
    assert_eq!(
        std::fs::read_to_string(r.join("README")).unwrap(),
        "hello\n"
    );
    assert_eq!(std::fs::read_to_string(r.join("gone")).unwrap(), "g\n");
    assert!(!r.join("new").exists() && !r.join("added").exists());
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
        assert_eq!(a.last().unwrap(), "-f");
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
