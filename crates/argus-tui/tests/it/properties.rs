//! Properties that must hold for any input, checked on generated ones
//! (proptest shrinks a failure to a minimal case).

use std::collections::BTreeSet;
use std::sync::OnceLock;

use argus_core::config::Config;
use argus_core::diff::LineKind;
use argus_core::ops::Op;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{App, Effect, Marked, Mode, Row, Target};
use argus_tui::details::{Section, Seg};
use argus_tui::refine::refine;
use argus_tui::theme::Theme;
use argus_tui::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use proptest::prelude::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn kind() -> impl Strategy<Value = LineKind> {
    prop_oneof![
        Just(LineKind::Context),
        Just(LineKind::Add),
        Just(LineKind::Remove)
    ]
}

/// A scanned workspace with clean, dirty, staged and untracked changes,
/// built once (its sandbox lives for the whole run).
fn workspace() -> &'static Workspace {
    static WS: OnceLock<(Sandbox, Workspace)> = OnceLock::new();
    &WS.get_or_init(|| {
        let sb = Sandbox::new();
        for n in ["a", "b", "c"] {
            sb.remote("acme", n);
            sb.clone_as_github("acme", n, &format!("dev/{n}"));
        }
        let b = sb.path("dev/b");
        write(&b, "README", "one two\n");
        commit(&b, "readme");
        write(&b, "README", "one three\n");
        write(&b, "staged", "s\n");
        git(&b, &["add", "staged"]);
        write(&b, "new", "x\ny\n");
        // (one file staged and changed again: listed twice, a side each)
        write(&b, "both", "one\n");
        git(&b, &["add", "both"]);
        git(&b, &["commit", "-q", "-m", "both"]);
        write(&b, "both", "two\n");
        git(&b, &["add", "both"]);
        write(&b, "both", "three\n");
        // every repo has files, so the last row can be a file's line
        write(&sb.path("dev/a"), "untracked", "u\n");
        write(&sb.path("dev/c"), "README", "changed\n");
        // (and files of several hunks, changed and staged, in another repo)
        let c = sb.path("dev/c");
        let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        write(&c, "multi", &lines);
        write(&c, "st", &lines);
        commit(&c, "lines");
        let changed = lines
            .replace("line 3\n", "line three\n")
            .replace("line 28\n", "line 28\nadded\n");
        write(&c, "st", &changed);
        git(&c, &["add", "st"]);
        write(&c, "multi", &changed);
        let cfg = format!(
            "roots = ['{}']\n[ui]\nattention_only = false",
            sb.path("dev").display()
        );
        let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
        ws.scan();
        (sb, ws)
    })
    .1
}

/// Keys a user might press, including ones that open menus, prompts and
/// confirmations. Effects are returned, never run, so nothing is changed.
fn key() -> impl Strategy<Value = KeyEvent> {
    let plain = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
    prop_oneof![
        // moving, unfolding and marking, more often: they reach the edges
        4 => prop::sample::select(vec![KeyCode::End, KeyCode::Tab, KeyCode::Char(' '), KeyCode::Down])
            .prop_map(plain),
        2 => prop::sample::select(" jknpgG<>/?hqLaHRsuKzfFPcbxmov$!UikmtuyN".chars().collect::<Vec<_>>())
            .prop_map(move |c| plain(KeyCode::Char(c))),
        1 => prop::sample::select(vec![
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Backspace,
        ])
        .prop_map(plain),
        1 => prop::sample::select("npvdugs".chars().collect::<Vec<_>>())
            .prop_map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
    ]
}

/// A state got to directly, as few runs of keys from the top would: every
/// repo's details open, some files' diffs unfolded, space pressed on some
/// rows of the changes, the cursor on any row.
fn arranged(
    unfold: &[prop::sample::Index],
    toggle: &[prop::sample::Index],
    region: Option<(prop::sample::Index, usize)>,
    cursor: prop::sample::Index,
) -> App {
    let plain = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
    let mut app = App::new(workspace().clone());
    for n in (0..app.rows.len()).rev() {
        app.cursor = n;
        app.on_key(plain(KeyCode::Tab));
    }
    for pick in unfold {
        let folded: Vec<usize> = (0..app.rows.len())
            .filter(|&n| {
                let file = matches!(app.target(n), Some(Target::File { .. }));
                let open = matches!(app.rows.get(n + 1),
                    Some(Row::Detail { line, .. }) if line.diff.is_some());
                let own = matches!(&app.rows[n], Row::Detail { line, .. } if line.diff.is_none());
                file && own && !open
            })
            .collect();
        if !folded.is_empty() {
            app.cursor = folded[pick.index(folded.len())];
            app.on_key(plain(KeyCode::Tab));
        }
    }
    for pick in toggle {
        let targets: Vec<usize> = (0..app.rows.len())
            .filter(|&n| app.target(n).is_some())
            .collect();
        app.cursor = targets[pick.index(targets.len())];
        app.on_key(plain(KeyCode::Char(' ')));
    }
    // a region: started on a file, a hunk or a line of one (what it
    // takes follows), then so many rows down
    let in_diff: Vec<usize> = (0..app.rows.len())
        .filter(|&n| {
            matches!(
                app.target(n),
                Some(Target::Hunk { .. } | Target::File { .. })
            )
        })
        .collect();
    match region {
        Some((start, down)) if !in_diff.is_empty() => {
            app.cursor = in_diff[start.index(in_diff.len())];
            app.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
            for _ in 0..down {
                app.on_key(plain(KeyCode::Down));
            }
            // (the cursor stays where the region ends)
        }
        _ => app.cursor = cursor.index(app.rows.len()),
    }
    app
}

/// The changed lines whose rows show wholly marked under `section`, and
/// the files whose own rows do, as the screen has them.
fn shown_marked(app: &App, section: Section) -> Taken {
    let (mut lines, mut files) = (vec![], BTreeSet::new());
    for n in 0..app.rows.len() {
        let Row::Detail { line, .. } = &app.rows[n] else {
            continue;
        };
        match app.target(n) {
            Some(t @ Target::File { .. }) if app.marked(&t) == Marked::Wholly => {
                let Target::File {
                    repo,
                    section: s,
                    file,
                } = t
                else {
                    unreachable!()
                };
                if s == section {
                    files.insert((repo, file));
                }
            }
            Some(t @ Target::Hunk { at: Some(_), .. }) if app.marked(&t) == Marked::Wholly => {
                let Target::Hunk {
                    repo,
                    section: s,
                    file,
                    ..
                } = t
                else {
                    unreachable!()
                };
                let l = line.diff.as_deref().and_then(|d| d.line.as_ref());
                let sign = l.map(|l| if l.kind == LineKind::Add { '+' } else { '-' });
                if let (true, Some(l), Some(sign)) = (s == section, l, sign) {
                    lines.push((repo, file, sign, l.text.clone()));
                }
            }
            _ => {}
        }
    }
    lines.sort();
    lines.dedup();
    (lines, files)
}

/// A changed line of a file's diff, as a patch has it: repo, file, sign,
/// text.
type PatchLine = (std::path::PathBuf, String, char, String);

/// Lines to go as patches, and files to go whole (by repo).
type Taken = (Vec<PatchLine>, BTreeSet<(std::path::PathBuf, String)>);

/// What the rows say `verb` should take: the changed lines to go as
/// patches and the files to go whole, or `None` when something marked is
/// on the wrong side for it (nothing is to happen then). Worked out from
/// what each row is and shows, not from what the keys read.
fn expected(app: &App, verb: char, all: bool) -> Option<Taken> {
    let right = |s: Section| (s == Section::Staged) == (verb == 'u');
    let detail = |n: usize| match &app.rows[n] {
        Row::Detail { repo, line } => Some((app.ws.entries[*repo].path.clone(), line)),
        Row::Repo(_) => None,
    };
    // a row's changed line, if it's one
    let line_of = |n: usize| {
        let (repo, line) = detail(n)?;
        let d = line.diff.as_deref()?;
        let l = d.line.as_ref()?;
        let sign = match l.kind {
            LineKind::Add => '+',
            LineKind::Remove => '-',
            LineKind::Context => return None,
        };
        let key = (line.section?, line.file.clone()?, d.hunk?);
        Some((key, (repo, line.file.clone()?, sign, l.text.clone())))
    };
    let rows = 0..app.rows.len();
    let (mut lines, mut files) = (vec![], BTreeSet::new());
    // With marks in a repo the cursor isn't in, it's asked which: all of
    // them, or this repo alone (its marks, else the cursor's row).
    let here = match &app.rows[app.cursor] {
        Row::Repo(i) | Row::Detail { repo: i, .. } => app.ws.entries[*i].path.clone(),
    };
    let asked = app.repos_with_marks().iter().any(|r| *r != here);
    let within = |repo: &std::path::PathBuf| all || !asked || *repo == here;
    let marked = match asked && !all {
        true => app.repos_with_marks().contains(&here),
        false => app.marked_in(None) != (0, 0),
    };
    if marked {
        for n in rows {
            let target = app.target(n).filter(|t| {
                let (Target::Repo { repo }
                | Target::Section { repo, .. }
                | Target::File { repo, .. }
                | Target::Hunk { repo, .. }) = t;
                within(repo)
            });
            match target {
                Some(Target::File {
                    repo,
                    section,
                    file,
                }) if app.file_marked(&repo, section, &file) => {
                    if !right(section) {
                        return None;
                    }
                    files.insert((repo, file));
                }
                Some(t @ Target::Hunk { at: Some(_), .. }) if app.marked(&t) == Marked::Wholly => {
                    let Target::Hunk {
                        repo,
                        section,
                        file,
                        ..
                    } = &t
                    else {
                        unreachable!()
                    };
                    if app.file_marked(repo, *section, file) {
                        continue;
                    }
                    if !right(*section) {
                        return None;
                    }
                    lines.extend(line_of(n).map(|(_, l)| l));
                }
                _ => {}
            }
        }
        // (the changes show twice with the uncommitted issue unfolded)
        lines.sort();
        lines.dedup();
        return Some((lines, files));
    }
    // nothing marked: what the cursor's row is, or is in. The repo's own
    // row, and a row of its details that's none of its changes: all the
    // repo lists on the key's side.
    let everything = |repo: &std::path::PathBuf| {
        let status = app.ws.get(repo).and_then(|e| e.status());
        let sections = Section::ALL.into_iter().filter(|s| right(*s));
        let listed = sections.flat_map(|s| status.map(|st| s.files(st)).unwrap_or_default());
        listed.map(|f| (repo.clone(), f)).collect::<BTreeSet<_>>()
    };
    let Some((repo, line)) = detail(app.cursor) else {
        return Some((lines, everything(&here)));
    };
    let Some(section) = line.section else {
        return Some((lines, everything(&repo)));
    };
    let blank = line.id.last() == Some(&Seg::Blank);
    if let Some(section) = Section::heads(line) {
        if !right(section) {
            return None;
        }
        let status = app.ws.get(&repo).and_then(|e| e.status());
        let listed = status.map(|s| section.files(s)).unwrap_or_default();
        files.extend(listed.into_iter().map(|f| (repo.clone(), f)));
        return Some((lines, files));
    }
    let file = line.file.clone()?;
    if blank {
        return Some((lines, files));
    }
    if !right(section) {
        return None;
    }
    match line.diff.as_deref().and_then(|d| d.hunk) {
        Some(hunk) if section != Section::Untracked => {
            let key = (section, file, hunk);
            let hunks = rows
                .filter_map(line_of)
                .filter(|(k, l)| *k == key && l.0 == repo);
            lines.extend(hunks.map(|(_, l)| l));
            lines.sort();
            lines.dedup();
        }
        _ => drop(files.insert((repo, file))),
    }
    Some((lines, files))
}

/// What `fx`'s jobs would change: the lines their patches take, and the
/// files taken whole.
fn changed(fx: &[Effect]) -> Taken {
    let (mut lines, mut files) = (vec![], BTreeSet::new());
    for e in fx {
        let Effect::Jobs(jobs) = e else { continue };
        for j in jobs {
            match &j.op {
                Op::Apply { patch, .. } => {
                    let mut file = String::new();
                    for l in patch.lines() {
                        if let Some(name) = l.strip_prefix("+++ b/") {
                            file = name.to_string();
                        } else if !l.starts_with("--- ") {
                            let sign = l.chars().next().filter(|c| "+-".contains(*c));
                            lines.extend(
                                sign.map(|s| (j.path.clone(), file.clone(), s, l[1..].to_string())),
                            );
                        }
                    }
                }
                Op::Add { paths }
                | Op::AddTracked { paths }
                | Op::Clean { paths }
                | Op::Restore { paths, .. }
                | Op::Remove { paths, .. } => {
                    files.extend(paths.iter().map(|p| (j.path.clone(), p.clone())));
                }
                other => panic!("not a file operation: {other:?}"),
            }
        }
    }
    lines.sort();
    (lines, files)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// From any state of marks, unfolded diffs and cursor, `s`, `u` and
    /// `K` change exactly what the rows show as theirs to take: those
    /// lines and those files, no others, and none of them left out. With
    /// something marked on the wrong side, nothing.
    #[test]
    fn the_keys_take_exactly_what_the_rows_show(
        unfold in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
        toggle in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
        region in prop::option::weighted(0.3, (any::<prop::sample::Index>(), 0..6usize)),
        cursor in any::<prop::sample::Index>(),
        verb in prop::sample::select(vec!['s', 'u', 'K']),
        all in any::<bool>(),
    ) {
        let mut app = arranged(&unfold, &toggle, region, cursor);
        let want = expected(&app, verb, all);
        let at = format!("{:?}", app.target(app.cursor));
        let mut fx = app.on_key(KeyEvent::new(KeyCode::Char(verb), KeyModifiers::NONE));
        // (asked which marks: all of them, or this repo's)
        if matches!(app.mode, Mode::Menu { .. }) {
            let answer = if all { 'A' } else { verb };
            fx = app.on_key(KeyEvent::new(KeyCode::Char(answer), KeyModifiers::NONE));
        }
        if matches!(app.mode, Mode::Confirm { .. }) {
            fx = app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        }
        let got = changed(&fx);
        let want = want.unwrap_or_default();
        prop_assert_eq!(&got.0, &want.0, "the lines {} takes, the cursor on {}", verb, at);
        prop_assert_eq!(&got.1, &want.1, "the files {} takes, the cursor on {}", verb, at);
    }

    /// Stashing what's marked, from any state: a file goes by its path
    /// (which takes all its changes) only if every row of it shows wholly
    /// marked, and a line only if its row does (or its file's, under
    /// unstaged). And when anything is stashed, nothing that shows wholly
    /// marked is left out.
    #[test]
    fn stashing_takes_exactly_what_shows_as_marked(
        unfold in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
        toggle in prop::collection::vec(any::<prop::sample::Index>(), 0..6),
        region in prop::option::weighted(0.3, (any::<prop::sample::Index>(), 0..6usize)),
        cursor in any::<prop::sample::Index>(),
    ) {
        let mut app = arranged(&unfold, &toggle, region, cursor);
        let shown: Vec<Taken> = Section::ALL.iter().map(|s| shown_marked(&app, *s)).collect();
        let listed = |repo: &std::path::Path, file: &str| -> Vec<usize> {
            let status = workspace().get(repo).and_then(|e| e.status());
            let lists = |n: &usize| status.is_some_and(|s| Section::ALL[*n].files(s).iter().any(|f| f == file));
            (0..3).filter(lists).collect()
        };
        let whole = |repo: &std::path::Path, file: &str, n: usize| {
            shown[n].1.contains(&(repo.to_path_buf(), file.to_string()))
        };
        let unstaged = 1;
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let mut fx = app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        // (asked which repos' marks: all of them)
        if matches!(&app.mode, Mode::Menu { menu, .. } if menu.items.iter().any(|i| i.key == 'A')) {
            fx = app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
        }
        let (mut by_path, mut by_line): (BTreeSet<(std::path::PathBuf, String)>, Vec<PatchLine>) =
            (BTreeSet::new(), vec![]);
        let mut stashed = false;
        for e in &fx {
            let Effect::Jobs(jobs) = e else { continue };
            for j in jobs {
                stashed = true;
                match &j.op {
                    Op::Stash { paths, .. } => {
                        prop_assert!(!paths.is_empty(), "a stash of everything in {}", j.path.display());
                        by_path.extend(paths.iter().map(|p| (j.path.clone(), p.clone())));
                    }
                    Op::StashLines { stage, .. } => {
                        let mut file = String::new();
                        for l in stage.lines() {
                            if let Some(name) = l.strip_prefix("+++ b/") {
                                file = name.to_string();
                            } else if !l.starts_with("--- ") {
                                let sign = l.chars().next().filter(|c| "+-".contains(*c));
                                by_line.extend(sign.map(|s| (j.path.clone(), file.clone(), s, l[1..].to_string())));
                            }
                        }
                    }
                    other => prop_assert!(false, "not a stash: {other:?}"),
                }
            }
        }
        // nothing but what shows
        for (repo, file) in &by_path {
            let rows = listed(repo, file);
            prop_assert!(
                !rows.is_empty() && rows.iter().all(|n| whole(repo, file, *n)),
                "{} stashed by its path, though not marked everywhere it's listed", file
            );
        }
        for l in &by_line {
            prop_assert!(
                shown[unstaged].0.contains(l) || whole(&l.0, &l.1, unstaged),
                "{}{} of {} stashed, though it doesn't show marked under unstaged", l.2, l.3, l.1
            );
        }
        // and all of it, when anything is
        if stashed {
            for l in &shown[unstaged].0 {
                prop_assert!(
                    by_line.contains(l) || by_path.contains(&(l.0.clone(), l.1.clone())),
                    "{}{} of {} shows marked and wasn't stashed", l.2, l.3, l.1
                );
            }
            for (n, (_, files)) in shown.iter().enumerate() {
                for (repo, file) in files {
                    let lines = by_line.iter().any(|l| l.0 == *repo && l.1 == *file);
                    prop_assert!(
                        by_path.contains(&(repo.clone(), file.clone())) || (n == unstaged && lines),
                        "{} shows marked under {:?} and wasn't stashed", file, Section::ALL[n]
                    );
                }
            }
        }
    }
}

proptest! {
    /// Refining only marks words: each line's text is kept exactly, and
    /// context lines are never marked.
    #[test]
    fn refining_keeps_every_line(lines in prop::collection::vec((kind(), "[a-c ]{0,12}"), 0..12)) {
        let input: Vec<(LineKind, &str)> = lines.iter().map(|(k, t)| (*k, t.as_str())).collect();
        let out = refine(&input);
        prop_assert_eq!(out.len(), input.len());
        for ((k, text), segs) in input.iter().zip(&out) {
            let joined: String = segs.iter().map(|s| s.1.as_str()).collect();
            prop_assert_eq!(&joined, text);
            if *k == LineKind::Context {
                prop_assert!(segs.iter().all(|s| !s.0));
            }
        }
    }

    /// Whatever is pressed, the cursor stays on a row, the view scrolls to
    /// it, and drawing works.
    #[test]
    fn the_cursor_stays_in_range(keys in prop::collection::vec(key(), 0..40)) {
        let mut app = App::new(workspace().clone());
        let (theme, _) = Theme::build("terminal", &Default::default());
        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        for k in keys {
            app.on_key(k);
            if app.quit {
                break;
            }
            prop_assert!(app.rows.is_empty() || app.cursor < app.rows.len(), "cursor {} of {}", app.cursor, app.rows.len());
            let view = ui::View { theme: &theme, icons: argus_core::icons::UNICODE, tick: 0 };
            term.draw(|f| drop(ui::draw(f, &mut app, &view))).unwrap();
            prop_assert!(app.scroll <= app.cursor || app.rows.is_empty());
        }
        // and the app can always get back to normal
        for _ in 0..3 {
            app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        prop_assert!(matches!(app.mode, Mode::Normal), "{:?}", std::mem::discriminant(&app.mode));
    }
}
