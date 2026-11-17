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
    // (nor is the index kept: what's staged is what's being put away)
    case(
        "staged only, keep index asked for",
        true,
        true,
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

/// A stash that isn't the newest is popped by where it is, as long as
/// it's still the stash that was meant.
#[test]
fn a_chosen_stash_is_popped_if_it_is_still_that_one() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "a", "1\n");
    write(&r, "b", "1\n");
    git(&r, &["add", "."]);
    git(&r, &["commit", "-q", "-m", "files"]);
    write(&r, "a", "older\n");
    git(&r, &["stash", "push", "-q", "-m", "older"]);
    write(&r, "b", "newer\n");
    git(&r, &["stash", "push", "-q", "-m", "newer"]);
    let s = st(&r);
    let pop = |index: usize, id: &str| {
        ops::run(
            &r,
            &Op::StashPopAt {
                index,
                id: id.into(),
            },
        )
    };
    assert_eq!(
        Op::StashPopAt {
            index: 1,
            id: String::new()
        }
        .label(),
        "pop stash@{1}"
    );
    // not the stash it was: refused, nothing popped
    let o = pop(1, &s.stash_ids[0]);
    assert!(
        !o.success && o.output.contains("isn't the stash it was"),
        "{}",
        o.output
    );
    assert_eq!(st(&r).stashes.len(), 2);
    // the older one: its change is back, the newer still stashed
    let o = pop(1, &s.stash_ids[1]);
    assert!(o.success, "{}", o.output);
    assert_eq!(changes(&r), [" M a"]);
    let left = st(&r);
    assert_eq!(left.stash_ids, [s.stash_ids[0].clone()]);
    // what the planner expects of it
    let predicted = simulate(
        &s,
        &argus_core::fix::Fix {
            kind: FixKind::StashPop,
            op: Op::StashPopAt {
                index: 1,
                id: s.stash_ids[1].clone(),
            },
            reason: String::new(),
            blocked: None,
        },
    );
    assert_eq!(predicted.stash_ids, left.stash_ids);
    // one that would conflict is refused, as the newest is
    write(&r, "b", "in the way\n");
    git(&r, &["commit", "-q", "-am", "b changed"]);
    let o = pop(0, &s.stash_ids[0]);
    assert!(
        !o.success && o.output.contains("would conflict in b"),
        "{}",
        o.output
    );
}

/// A marked file is stashed by its name, not as a pattern: `[x].txt`
/// isn't also `x.txt`.
#[test]
fn a_path_to_stash_is_that_path_whatever_is_in_its_name() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "x.txt", "1\n");
    write(&r, "[x].txt", "1\n");
    git(&r, &["add", "."]);
    git(&r, &["commit", "-q", "-m", "files"]);
    write(&r, "x.txt", "2\n");
    write(&r, "[x].txt", "2\n");
    stash(&r, true, false, false, &["[x].txt"]);
    assert_eq!(changes(&r), [" M x.txt"]);
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
    assert_eq!(
        args(&op(true, true, true, &[])),
        "stash push --staged --message -rf"
    );
    assert_eq!(op(true, true, true, &[]).label(), "stash staged");
    // paths come after `--`, so none can be taken as an option
    assert_eq!(
        args(&op(false, false, false, &["-x", "b"])),
        "stash push --message -rf -- :(literal)-x :(literal)b"
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
        ..Default::default()
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
    // a staged-only or paths-only stash: only the stash is predicted
    for (staged, paths) in [(true, vec![]), (false, vec!["README".to_string()])] {
        let Op::Stash {
            message,
            untracked,
            keep_index,
            ..
        } = fix.op.clone()
        else {
            unreachable!()
        };
        let partial = argus_core::fix::Fix {
            op: Op::Stash {
                message,
                untracked,
                keep_index,
                staged,
                paths: paths.clone(),
            },
            ..fix.clone()
        };
        let s = st(&r);
        let after = simulate(&s, &partial);
        assert_eq!(after.changes, s.changes, "staged {staged} paths {paths:?}");
        assert_eq!(after.stashes.len(), s.stashes.len() + 1);
    }
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
        ..Default::default()
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
        ..Default::default()
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
    // files with only unstaged changes (README) aren't checked: with
    // `both` staged only, nothing stands in the way
    git(&r, &["restore", "--worktree", "both"]);
    assert_eq!(stash_problem(&r, &st(&r), true, true, &[]), None);
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

/// `f`, thirty lines, with lines 3 and 4 changed side by side and a line
/// added near the end; `g` changed and staged. Returns the patches that
/// stage and that discard line 3's change and the added line, leaving
/// line 4's.
fn lines_to_stash(sb: &Sandbox) -> (std::path::PathBuf, String, String) {
    let r = sb.init("r");
    let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    write(&r, "f", &lines);
    write(&r, "g", "g\n");
    git(&r, &["add", "f", "g"]);
    git(&r, &["commit", "-q", "-m", "files"]);
    write(&r, "g", "g2\n");
    git(&r, &["add", "g"]);
    let changed = lines
        .replace("line 3\n", "line three\n")
        .replace("line 4\n", "line four\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&r, "f", &changed);
    let (stage, discard) = lines_patches(&r);
    (r, stage, discard)
}

/// The patches for [`lines_to_stash`]'s repo, from its diff as it is.
fn lines_patches(r: &Path) -> (String, String) {
    use argus_core::diff::{self, FileDiff, LineKind, Side};
    let Ok(FileDiff::Text(hunks)) = diff::file_diff(r, "f", Side::Unstaged) else {
        panic!("f's diff");
    };
    assert_eq!(hunks.len(), 2);
    let of = |hunk: &diff::Hunk, texts: &[&str]| -> Vec<usize> {
        let lines = hunk.lines.iter().enumerate();
        lines
            .filter(|(_, l)| l.kind != LineKind::Context && texts.contains(&l.text.as_str()))
            .map(|(at, _)| at)
            .collect()
    };
    let (first, second) = (
        of(&hunks[0], &["line 3", "line three"]),
        of(&hunks[1], &["added"]),
    );
    assert_eq!((first.len(), second.len()), (2, 1));
    let parts = [
        (&hunks[0], Some(&first[..])),
        (&hunks[1], Some(&second[..])),
    ];
    (
        diff::patch_hunks("f", &parts, false),
        diff::patch_hunks("f", &parts, true),
    )
}

/// Lines of a file are stashed by themselves: out of the work tree, the
/// changed line beside them left (which git's own staged-only stash can't
/// do), what's staged left staged; popped, they're back.
#[test]
fn lines_are_stashed_by_themselves() {
    let sb = Sandbox::new();
    let (r, stage, discard) = lines_to_stash(&sb);
    let before = std::fs::read_to_string(r.join("f")).unwrap();
    let op = Op::StashLines {
        message: "some lines".into(),
        stage,
        discard,
        lines: 3,
    };
    assert_eq!(op.label(), "stash 3 lines");
    let out = ops::run(&r, &op);
    assert!(out.success, "{}", out.output);
    assert_eq!(out.output, "On main: some lines");
    let text = std::fs::read_to_string(r.join("f")).unwrap();
    assert!(
        text.contains("line 3\nline four\n") && !text.contains("added"),
        "{text}"
    );
    assert_eq!(changes(&r), [" M f", "M  g"]);
    let list = git(&r, &["stash", "list"]);
    assert!(list.trim_end().ends_with("On main: some lines"), "{list}");
    assert_eq!(list.lines().count(), 1);
    // nothing of the making left behind
    let left: Vec<_> = std::fs::read_dir(r.join(".git"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("argus"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // the stash holds those lines, and the index as it is
    let held = git(&r, &["diff", "HEAD", "stash@{0}", "--", "f"]);
    assert!(
        held.contains("+line three") && held.contains("+added") && !held.contains("four"),
        "{held}"
    );
    assert_eq!(
        git(&r, &["rev-parse", "stash@{0}^2^{tree}"]),
        git(&r, &["write-tree"])
    );
    assert_eq!(
        git(&r, &["log", "-1", "--format=%s", "stash@{0}^2"]).trim(),
        format!(
            "index on main: {} files",
            git(&r, &["rev-parse", "--short", "HEAD"]).trim()
        )
    );
    // git pops onto no file with changes of its own (as after its own
    // `stash -p`): its refusal is passed on with what to do, the stash kept
    let pop = ops::run(&r, &Op::StashPop);
    assert!(!pop.success, "{}", pop.output);
    assert!(
        pop.output.contains("would be overwritten"),
        "{}",
        pop.output
    );
    assert!(
        pop.output
            .ends_with("(commit or stash those files' other changes first)"),
        "{}",
        pop.output
    );
    assert_eq!(git(&r, &["stash", "list"]).lines().count(), 1);
    // without them, they're back
    git(&r, &["checkout", "-q", "--", "f"]);
    let pop = ops::run(&r, &Op::StashPop);
    assert!(pop.success, "{}", pop.output);
    assert_eq!(
        std::fs::read_to_string(r.join("f")).unwrap(),
        before.replace("line four", "line 4")
    );
    assert_eq!(changes(&r), [" M f", "M  g"]);
}

/// Lines that can't be taken out of the work tree aren't stashed either:
/// nothing changes, and no stash is left.
#[test]
fn a_stash_of_lines_that_fails_changes_nothing() {
    let sb = Sandbox::new();
    let (r, stage, discard) = lines_to_stash(&sb);
    let before = std::fs::read_to_string(r.join("f")).unwrap();
    let nothing_changed = |r: &Path| {
        assert_eq!(std::fs::read_to_string(r.join("f")).unwrap(), before);
        assert_eq!(changes(r), [" M f", "M  g"]);
        assert_eq!(git(r, &["stash", "list"]), "");
        let left = std::fs::read_dir(r.join(".git")).unwrap();
        let left = left.filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_string_lossy().contains("argus")
        });
        assert_eq!(left.count(), 0);
    };
    // the work tree changed since the patch was made
    let op = Op::StashLines {
        message: "m".into(),
        stage: stage.clone(),
        discard: discard.replace("+added", "+something else"),
        lines: 3,
    };
    let out = ops::run(&r, &op);
    assert!(
        !out.success && out.output.contains("not stashed"),
        "{}",
        out.output
    );
    nothing_changed(&r);
    // the index did
    let op = Op::StashLines {
        message: "m".into(),
        stage: stage
            .replace("+added", "+something else")
            .replace(" line 29", " line x"),
        discard,
        lines: 3,
    };
    let out = ops::run(&r, &op);
    assert!(
        !out.success && out.output.contains("not stashed"),
        "{}",
        out.output
    );
    nothing_changed(&r);
    // git's own reason is passed on: here, the index locked
    std::fs::write(r.join(".git/index.lock"), "").unwrap();
    let op = Op::StashLines {
        message: "m".into(),
        stage: lines_patches(&r).0,
        discard: lines_patches(&r).1,
        lines: 3,
    };
    let out = ops::run(&r, &op);
    assert!(
        !out.success && out.output.contains("not stashed") && out.output.contains("index.lock"),
        "{}",
        out.output
    );
    std::fs::remove_file(r.join(".git/index.lock")).unwrap();
    nothing_changed(&r);
    // no commit to stash against
    let fresh = sb.path("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    git(&fresh, &["init", "-q"]);
    let out = ops::run(&fresh, &op);
    assert!(
        !out.success && out.output.contains("no commit yet"),
        "{}",
        out.output
    );
}

/// A stash made with something staged holds that as its index, which
/// git can't restore once the index has moved on. The pop fails with
/// git's word for it and nothing changed; popping without the index is a
/// thing of its own, to be asked for.
#[test]
fn a_stash_whose_index_no_longer_applies_pops_without_it_when_asked() {
    let sb = Sandbox::new();
    let (r, stage, discard) = lines_to_stash(&sb);
    let op = Op::StashLines {
        message: "m".into(),
        stage,
        discard,
        lines: 3,
    };
    assert!(ops::run(&r, &op).success);
    let id = git(&r, &["rev-parse", "stash@{0}"]).trim().to_string();
    git(&r, &["checkout", "-q", "--", "f"]);
    write(&r, "h", "h\n");
    git(&r, &["add", "h"]);
    let before = changes(&r);
    let pop = ops::run(&r, &Op::StashPop);
    assert!(
        !pop.success && pop.output.contains("Try without --index"),
        "{}",
        pop.output
    );
    assert_eq!(changes(&r), before, "nothing changed");
    assert_eq!(git(&r, &["stash", "list"]).lines().count(), 1);
    // asked for: only that stash, as it was
    let plain = |id: &str| Op::StashPopPlain {
        index: 0,
        id: id.to_string(),
    };
    assert_eq!(plain(&id).label(), "pop stash@{0} without its index");
    let other = ops::run(&r, &plain("0000000000000000000000000000000000000000"));
    assert!(
        !other.success && other.output.contains("isn't the stash it was"),
        "{}",
        other.output
    );
    let pop = ops::run(&r, &plain(&id));
    assert!(pop.success, "{}", pop.output);
    assert!(
        std::fs::read_to_string(r.join("f"))
            .unwrap()
            .contains("added")
    );
    assert_eq!(git(&r, &["stash", "list"]), "");
}

/// The check before a pop is against what's staged, as the pop is: a
/// stash that conflicts with a staged change is refused before anything
/// is touched, and one that doesn't is popped, whatever else is staged
/// or changed.
#[test]
fn the_check_before_a_pop_goes_by_what_is_staged() {
    let sb = Sandbox::new();
    let numbered: String = (1..=10).map(|i| format!("{i}\n")).collect();
    let repo = |name: &str| {
        let r = sb.init(name);
        write(&r, "f", &numbered);
        write(&r, "g", "g\n");
        git(&r, &["add", "f", "g"]);
        git(&r, &["commit", "-q", "-m", "files"]);
        r
    };
    // a staged change that the stash's conflicts with
    let r = repo("conflict");
    write(&r, "f", &numbered.replacen("1\n", "ONE\n", 1));
    git(&r, &["stash", "-q"]);
    write(&r, "f", &numbered.replacen("1\n", "UNO\n", 1));
    git(&r, &["add", "f"]);
    let pop = ops::run(&r, &Op::StashPop);
    assert!(!pop.success, "{}", pop.output);
    assert_eq!(pop.output, "not popping: it would conflict in f");
    assert_eq!(changes(&r), ["M  f"], "nothing touched");
    assert!(
        !std::fs::read_to_string(r.join("f"))
            .unwrap()
            .contains("<<<<")
    );
    // inverse: another file partly staged is no reason to refuse
    let r = repo("other");
    write(&r, "f", &numbered.replacen("1\n", "ONE\n", 1));
    git(&r, &["stash", "-q"]);
    write(&r, "g", "g\nG1\n");
    git(&r, &["add", "g"]);
    write(&r, "g", "g\nG1\nG2\n");
    let pop = ops::run(&r, &Op::StashPop);
    assert!(pop.success, "{}", pop.output);
    assert_eq!(changes(&r), [" M f", "MM g"]);
    // nor a file the stash deletes that's deleted already
    let r = repo("deleted");
    std::fs::remove_file(r.join("g")).unwrap();
    git(&r, &["stash", "-q"]);
    std::fs::remove_file(r.join("g")).unwrap();
    let pop = ops::run(&r, &Op::StashPop);
    assert!(pop.success, "{}", pop.output);
    // nor a file argus just rewrote the same (stashing all its lines)
    use argus_core::diff::{self, FileDiff, Side};
    let r = repo("same");
    write(&r, "g", "g\nmore\n");
    let Ok(FileDiff::Text(hunks)) = diff::file_diff(&r, "g", Side::Unstaged) else {
        panic!("g's diff");
    };
    let parts = [(&hunks[0], None)];
    let all = Op::StashLines {
        message: "m".into(),
        stage: diff::patch_hunks("g", &parts, false),
        discard: diff::patch_hunks("g", &parts, true),
        lines: 1,
    };
    assert!(ops::run(&r, &all).success);
    let pop = ops::run(&r, &Op::StashPop);
    assert!(pop.success, "{}", pop.output);
    assert_eq!(changes(&r), [" M g"]);
}

/// A stash holding an untracked file that's there again can't be popped
/// whole (git restores the rest, then stops): said before trying. And
/// with no stash there's nothing to check.
#[test]
fn a_pop_that_cannot_be_whole_is_refused_first() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    write(&r, "f", "f\n");
    git(&r, &["add", "f"]);
    git(&r, &["commit", "-q", "-m", "f"]);
    let none = ops::run(&r, &Op::StashPop);
    assert!(!none.success, "{}", none.output);
    assert_eq!(none.output, "not popping: there's no stash@{0}");
    write(&r, "f", "f2\n");
    write(&r, "u", "u\n");
    git(&r, &["stash", "-q", "-u"]);
    write(&r, "u", "another\n");
    let pop = ops::run(&r, &Op::StashPop);
    assert!(!pop.success, "{}", pop.output);
    assert_eq!(
        pop.output,
        "not popping: it holds the untracked u, which is there again: move it away first"
    );
    assert_eq!(changes(&r), ["?? u"], "nothing touched");
    // inverse: out of the way, it pops
    std::fs::remove_file(r.join("u")).unwrap();
    let pop = ops::run(&r, &Op::StashPop);
    assert!(pop.success, "{}", pop.output);
    assert_eq!(changes(&r), [" M f", "?? u"]);
}

/// A stash of lines on a detached HEAD, and in a linked worktree (whose
/// index is elsewhere): made as git would, nothing left behind.
#[test]
fn lines_are_stashed_off_a_branch_and_in_a_linked_worktree() {
    let sb = Sandbox::new();
    let (r, stage, discard) = lines_to_stash(&sb);
    git(&r, &["checkout", "-q", "--detach"]);
    let op = |stage: String, discard: String| Op::StashLines {
        message: "m".into(),
        stage,
        discard,
        lines: 3,
    };
    let out = ops::run(&r, &op(stage, discard));
    assert!(out.success, "{}", out.output);
    assert_eq!(out.output, "On (no branch): m");
    // a linked worktree of another repo
    let other = Sandbox::new();
    let (main, ..) = lines_to_stash(&other);
    git(&main, &["stash", "-q"]);
    let wt = other.path("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"],
    );
    let lines = std::fs::read_to_string(wt.join("f")).unwrap();
    let changed = lines
        .replace("line 3\n", "line three\n")
        .replace("line 4\n", "line four\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&wt, "f", &changed);
    let (stage, discard) = lines_patches(&wt);
    let out = ops::run(&wt, &op(stage, discard));
    assert!(out.success, "{}", out.output);
    assert_eq!(out.output, "On side: m");
    let text = std::fs::read_to_string(wt.join("f")).unwrap();
    assert!(
        text.contains("line 3\nline four\n") && !text.contains("added"),
        "{text}"
    );
    let left = walk(&main.join(".git"));
    assert!(!left.iter().any(|n| n.contains("argus")), "{left:?}");
}

/// Every file name under `dir`.
fn walk(dir: &Path) -> Vec<String> {
    let mut names = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        names.push(e.file_name().to_string_lossy().into_owned());
        if e.path().is_dir() {
            names.extend(walk(&e.path()));
        }
    }
    names
}

/// A mode change is told from a change of content.
#[test]
fn a_files_mode_change_is_seen() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let r = dirty(&sb);
    assert!(!ops::mode_changed(&r, "README"), "content only");
    assert!(
        !ops::mode_changed(&r, "staged"),
        "unchanged in the work tree"
    );
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(r.join("README"), mode).unwrap();
    assert!(ops::mode_changed(&r, "README"));
}
