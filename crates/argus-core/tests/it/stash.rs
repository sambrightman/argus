//! Stash options, run for real: what each leaves behind.

use std::path::Path;

use argus_core::config::StashConfig;
use argus_core::fix::{FixKind, apply_fix, plan, simulate};
use argus_core::ops::{self, Op};
use argus_core::status::{RepoStatus, StatusOptions, status};
use argus_core::testkit::{Sandbox, git, write};

fn st(r: &Path) -> RepoStatus {
    status(r, StatusOptions::default()).unwrap()
}

/// A repo with a staged change (`staged`), an unstaged one (`README`), a
/// file with both (`both`), and an untracked file (`new`).
fn dirty(sb: &Sandbox) -> std::path::PathBuf {
    let r = sb.init("r");
    write(&r, "staged", "s\n");
    write(&r, "both", "1\n");
    git(&r, &["add", "staged", "both"]);
    git(&r, &["commit", "-q", "-m", "files"]);
    write(&r, "staged", "s2\n");
    write(&r, "both", "2\n");
    git(&r, &["add", "staged", "both"]);
    write(&r, "both", "3\n");
    write(&r, "README", "changed\n");
    write(&r, "new", "untracked\n");
    r
}

/// Changed paths with their two-letter codes.
fn changes(r: &Path) -> Vec<String> {
    st(r)
        .changes
        .files
        .iter()
        .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
        .collect()
}

fn stash(r: &Path, untracked: bool, keep_index: bool, staged: bool, paths: &[&str]) {
    let op = Op::Stash {
        message: "test".into(),
        untracked,
        keep_index,
        staged,
        paths: paths.iter().map(|p| p.to_string()).collect(),
    };
    let out = ops::run(r, &op);
    assert!(out.success, "{op:?}: {}", out.output);
}

#[test]
fn each_option_leaves_what_it_should() {
    let sb = Sandbox::new();
    let mut before = changes(&dirty(&sb));
    before.sort();
    assert_eq!(before, [" M README", "?? new", "M  staged", "MM both"]);
    let case = |name: &str, untracked, keep_index, staged, paths: &[&str], want: &[&str]| {
        let sb = Sandbox::new();
        let r = dirty(&sb);
        if staged {
            // git can't stash only staged changes of a file that also has
            // unstaged ones (see stash_problem): make `both` staged only
            git(&r, &["restore", "--worktree", "both"]);
        }
        stash(&r, untracked, keep_index, staged, paths);
        let mut got = changes(&r);
        got.sort();
        let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want, "{name}");
        assert_eq!(st(&r).stashes.len(), 1, "{name}");
    };
    case("everything", true, false, false, &[], &[]);
    case("without untracked", false, false, false, &[], &["?? new"]);
    // keep index: staged changes stay staged (their unstaged parts go)
    case(
        "keep index",
        true,
        true,
        false,
        &[],
        &["M  staged", "M  both"],
    );
    // staged only: untracked is never included (git refuses the pair)
    case(
        "staged only",
        true,
        false,
        true,
        &[],
        &[" M README", "?? new"],
    );
    case(
        "some paths",
        true,
        false,
        false,
        &["README", "new"],
        &["M  staged", "MM both"],
    );
}

#[test]
fn stash_args_and_labels() {
    let op = |untracked, keep_index, staged, paths: &[&str]| Op::Stash {
        message: "-rf".into(),
        untracked,
        keep_index,
        staged,
        paths: paths.iter().map(|p| p.to_string()).collect(),
    };
    let args = |o: &Op| o.args().join(" ");
    assert_eq!(
        args(&op(true, false, false, &[])),
        "stash push --include-untracked --message -rf"
    );
    assert_eq!(
        args(&op(false, true, false, &[])),
        "stash push --keep-index --message -rf"
    );
    assert_eq!(
        args(&op(true, false, true, &[])),
        "stash push --staged --message -rf"
    );
    // paths come after `--`, so none can be taken as an option
    assert_eq!(
        args(&op(false, false, false, &["-x", "b"])),
        "stash push --message -rf -- -x b"
    );
    assert_eq!(op(true, false, false, &[]).label(), "stash");
    assert_eq!(
        op(false, true, false, &[]).label(),
        "stash (keeping index, no untracked)"
    );
    assert_eq!(op(true, false, true, &[]).label(), "stash staged");
    assert_eq!(
        op(true, false, false, &["a", "b"]).label(),
        "stash 2 path(s)"
    );
}

#[test]
fn the_stash_fix_follows_the_config_defaults() {
    let sb = Sandbox::new();
    let r = dirty(&sb);
    let keep = StashConfig {
        untracked: false,
        keep_index: true,
    };
    let fix = plan(&st(&r), &keep)
        .into_iter()
        .find(|f| f.kind == FixKind::Stash)
        .unwrap();
    let after = simulate(&st(&r), &fix);
    assert_eq!(
        (
            after.changes.staged,
            after.changes.unstaged,
            after.changes.untracked
        ),
        (2, 0, 1),
        "counts follow the files"
    );
    // the predicted stash is the one the fix will really make
    assert!(ops::run(&r, &fix.op).success);
    assert_eq!(after.stashes, st(&r).stashes);
    assert!(
        after.stashes[0].starts_with("argus "),
        "{:?}",
        after.stashes
    );
    let counts = |cfg: StashConfig| {
        let s = st(&r);
        let f = plan(&s, &cfg)
            .into_iter()
            .find(|f| f.kind == FixKind::Stash)
            .unwrap();
        let c = simulate(&s, &f).changes;
        (c.staged, c.unstaged, c.untracked, c.files.len())
    };
    assert_eq!(counts(StashConfig::default()), (0, 0, 0, 0));
    let no_untracked = StashConfig {
        untracked: false,
        keep_index: false,
    };
    assert_eq!(counts(no_untracked), (0, 0, 1, 1));
    // predicted: staged changes stay, untracked stays
    let mut predicted: Vec<String> = simulate(&st(&r), &fix)
        .changes
        .files
        .iter()
        .map(|f| format!("{}{} {}", f.index, f.worktree, f.path))
        .collect();
    predicted.sort();
    assert_eq!(predicted, ["?? new", "M  both", "M  staged"]);
    let a = apply_fix(&r, &[FixKind::Stash], &keep).unwrap().unwrap();
    assert!(a.outcome.success, "{}", a.outcome.output);
    let mut got = changes(&r);
    got.sort();
    assert_eq!(got, predicted, "the prediction matches");
    // inverse: with only untracked files and untracked off, nothing to stash
    let sb = Sandbox::new();
    let r = sb.init("u");
    write(&r, "new", "x\n");
    let off = StashConfig {
        untracked: false,
        keep_index: false,
    };
    assert!(!plan(&st(&r), &off).iter().any(|f| f.kind == FixKind::Stash));
    assert!(
        plan(&st(&r), &StashConfig::default())
            .iter()
            .any(|f| f.kind == FixKind::Stash)
    );
}

#[test]
fn staged_only_stashes_are_refused_when_git_would_fail() {
    use argus_core::ops::stash_problem;
    let sb = Sandbox::new();
    let r = dirty(&sb);
    let s = st(&r);
    let p = stash_problem(&r, &s, true, true, &[]).unwrap();
    assert!(p.contains("both") && p.contains("unstaged"), "{p}");
    // limited to paths without such a file: fine
    assert_eq!(stash_problem(&r, &s, true, true, &["staged".into()]), None);
    assert!(stash_problem(&r, &s, true, true, &["both".into()]).is_some());
    // inverse: other stashes don't care
    assert_eq!(stash_problem(&r, &s, true, false, &[]), None);
    // nothing to stash at all
    let clean = sb.init("clean");
    let p = stash_problem(&clean, &st(&clean), true, false, &[]).unwrap();
    assert!(p.contains("nothing to stash"), "{p}");
    // only untracked files, without untracked
    write(&clean, "x", "1\n");
    assert_eq!(stash_problem(&clean, &st(&clean), true, false, &[]), None);
    let p = stash_problem(&clean, &st(&clean), false, false, &[]).unwrap();
    assert!(p.contains("nothing to stash"), "{p}");
    // staged only, with nothing staged
    let p = stash_problem(&clean, &st(&clean), true, true, &[]).unwrap();
    assert!(p.contains("nothing staged"), "{p}");
}

/// A file of 12 lines with line 1 changed and staged, then line `edit`
/// changed again in the work tree.
fn staged_then_edited(sb: &Sandbox, name: &str, edit: usize) -> std::path::PathBuf {
    let r = sb.init(name);
    let lines = |f: &dyn Fn(usize) -> String| (1..=12).map(|i| f(i) + "\n").collect::<String>();
    write(&r, "f", &lines(&|i| format!("line {i}")));
    git(&r, &["add", "f"]);
    git(&r, &["commit", "-q", "-m", "f"]);
    write(
        &r,
        "f",
        &lines(&|i| {
            if i == 1 {
                "staged".into()
            } else {
                format!("line {i}")
            }
        }),
    );
    git(&r, &["add", "f"]);
    write(
        &r,
        "f",
        &lines(&|i| match i {
            1 if edit != 1 => "staged".into(),
            i if i == edit => "unstaged".into(),
            i => format!("line {i}"),
        }),
    );
    r
}

#[test]
fn the_staged_only_check_agrees_with_git() {
    use argus_core::ops::stash_problem;
    // git uses one line of context: an edit two lines away is fine
    for (edit, git_succeeds) in [(10, true), (3, true), (2, false), (1, false)] {
        let sb = Sandbox::new();
        let r = staged_then_edited(&sb, "r", edit);
        let problem = stash_problem(&r, &st(&r), true, true, &[]);
        assert_eq!(
            problem.is_none(),
            git_succeeds,
            "edit at line {edit}: {problem:?}"
        );
        if let Some(p) = &problem {
            assert!(p.contains("f") && p.contains("overlap"), "{p}");
        }
        // and git, asked for real on a copy, agrees
        let copy = staged_then_edited(&sb, "copy", edit);
        let out = ops::run(
            &copy,
            &Op::Stash {
                message: "t".into(),
                untracked: false,
                keep_index: false,
                staged: true,
                paths: vec![],
            },
        );
        assert_eq!(
            out.success, git_succeeds,
            "edit at line {edit}: {}",
            out.output
        );
        // the check changed nothing
        assert!(st(&r).stashes.is_empty());
        assert_eq!(changes(&r), ["MM f"]);
    }
}

#[test]
fn no_stash_before_the_first_commit() {
    let sb = Sandbox::new();
    let r = sb.path("empty");
    std::fs::create_dir_all(&r).unwrap();
    git(&r, &["init", "-q"]);
    write(&r, "a", "1\n");
    let p = argus_core::ops::stash_problem(&r, &st(&r), true, false, &[]).unwrap();
    assert!(p.contains("no commit"), "{p}");
}
