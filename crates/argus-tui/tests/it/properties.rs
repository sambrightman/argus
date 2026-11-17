//! Properties that must hold for any input, checked on generated ones
//! (proptest shrinks a failure to a minimal case).

use std::sync::OnceLock;

use argus_core::config::Config;
use argus_core::diff::LineKind;
use argus_core::ops::Op;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{App, Effect, Marked, Mode, Row, Target};
use argus_tui::details::Section;
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

/// Keys that move about the details, fold and unfold them, and mark:
/// rows, regions, all the repos, and their undoing.
fn marking_key() -> impl Strategy<Value = KeyEvent> {
    let plain = |c: KeyCode| KeyEvent::new(c, KeyModifiers::NONE);
    prop_oneof![
        6 => prop::sample::select(vec![KeyCode::Tab, KeyCode::Char(' '), KeyCode::Down, KeyCode::Down])
            .prop_map(plain),
        2 => prop::sample::select(vec![
            KeyCode::Up,
            KeyCode::End,
            KeyCode::Home,
            KeyCode::Char('n'),
            KeyCode::Char('p'),
            KeyCode::Char('M'),
            KeyCode::Char('U'),
        ])
        .prop_map(plain),
        1 => prop::sample::select(vec![' ', 'g'])
            .prop_map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
    ]
}

/// The files `fx`'s jobs would change, by repo.
fn touched(fx: &[Effect]) -> Vec<(std::path::PathBuf, String)> {
    let mut out = vec![];
    for e in fx {
        let Effect::Jobs(jobs) = e else { continue };
        for j in jobs {
            let files: Vec<String> = match &j.op {
                Op::Add { paths }
                | Op::AddTracked { paths }
                | Op::Clean { paths }
                | Op::Restore { paths, .. }
                | Op::Remove { paths, .. } => paths.clone(),
                Op::Apply { patch, .. } => {
                    let named = patch.lines().filter_map(|l| l.strip_prefix("+++ b/"));
                    named.map(str::to_string).collect()
                }
                other => panic!("not a file operation: {other:?}"),
            };
            out.extend(files.into_iter().map(|f| (j.path.clone(), f)));
        }
    }
    out
}

/// Whether `file` of `repo` shows as marked (or with `at`, is what that
/// row is or holds) in one of `sections`: by its own row when it has one,
/// else by its section's heading, else (all folded away) by the repo's.
fn shows(
    app: &App,
    repo: &std::path::Path,
    file: &str,
    sections: &[Section],
    at: Option<usize>,
) -> bool {
    let rows = || (0..app.rows.len()).filter_map(|n| Some((n, app.target(n)?)));
    let is = |t: &Target, section: Section, whole: bool| match t {
        Target::File {
            repo: r,
            section: s,
            file: f,
        } => !whole && r == repo && *s == section && f == file,
        Target::Hunk {
            repo: r,
            section: s,
            file: f,
            ..
        } => !whole && at.is_some() && r == repo && *s == section && f == file,
        Target::Section {
            repo: r,
            section: s,
        } => whole && r == repo && *s == section,
    };
    sections.iter().any(|&section| {
        let listed = app.ws.get(repo).and_then(|e| e.status());
        if !listed.is_some_and(|s| section.files(s).iter().any(|f| f == file)) {
            return false;
        }
        match at {
            // with nothing marked: the row under the cursor is its (the
            // file's own, or one of its diff, an unchanged line too), or
            // its section's heading
            Some(at) => match &app.rows[at] {
                Row::Detail { repo: r, line } => {
                    let heading = Section::heads(line) == Some(section);
                    let its = line.file.as_deref() == Some(file) && line.section == Some(section);
                    app.ws.entries[*r].path == repo && (heading || its)
                }
                Row::Repo(_) => false,
            },
            None => {
                let own: Vec<bool> = rows()
                    .filter(|(_, t)| matches!(t, Target::File { .. }) && is(t, section, false))
                    .map(|(_, t)| app.marked(&t) != Marked::No)
                    .collect();
                let heading: Vec<bool> = rows()
                    .filter(|(_, t)| is(t, section, true))
                    .map(|(_, t)| app.marked(&t) != Marked::No)
                    .collect();
                match (own.is_empty(), heading.is_empty()) {
                    (false, _) => own.iter().all(|m| *m),
                    (true, false) => heading.iter().all(|m| *m),
                    (true, true) => app.marked_in(Some(repo)) != (0, 0),
                }
            }
        }
    })
}

proptest! {
    /// Whatever is marked, and wherever the cursor is, `s`, `u` and `K`
    /// change only files that show as marked on the side the key is for;
    /// with nothing marked, only what the cursor is on. Nothing is taken
    /// by its other side, nor from where nothing shows it.
    #[test]
    fn the_keys_take_only_what_shows_as_theirs(
        keys in prop::collection::vec(marking_key(), 0..40),
        verb in prop::sample::select(vec!['s', 'u', 'K']),
    ) {
        let mut app = App::new(workspace().clone());
        for k in keys {
            app.on_key(k);
        }
        prop_assume!(matches!(app.mode, Mode::Normal));
        // (a region's lines become marks as the key is pressed)
        let region = app.region_lines();
        let marked = app.marked_in(None) != (0, 0) || !region.is_empty();
        let cursor = app.cursor;
        let before: Vec<Option<Target>> = (0..app.rows.len()).map(|n| app.target(n)).collect();
        let sections: &[Section] = match verb {
            'u' => &[Section::Staged],
            _ => &[Section::Untracked, Section::Unstaged],
        };
        // what shows as the key's to take, before it's pressed (which
        // unmarks the lines it takes)
        let mut theirs = vec![];
        for e in &app.ws.entries {
            let files = e.status().map(|s| s.changes.files.clone()).unwrap_or_default();
            for f in files {
                if shows(&app, &e.path, &f.path, sections, (!marked).then_some(cursor)) {
                    theirs.push((e.path.clone(), f.path));
                }
            }
        }
        let mut fx = app.on_key(KeyEvent::new(KeyCode::Char(verb), KeyModifiers::NONE));
        if matches!(app.mode, Mode::Confirm { .. }) {
            fx = app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        }
        for (repo, file) in touched(&fx) {
            let in_region = region.iter().any(|k| k.0 == repo && k.1 == file);
            let shown = in_region || theirs.contains(&(repo.clone(), file.clone()));
            prop_assert!(
                shown,
                "{verb} changes {file} in {}, which {}: rows {:?}",
                repo.display(),
                if marked { "doesn't show as marked there" } else { "the cursor isn't on" },
                before.iter().flatten().collect::<Vec<_>>(),
            );
        }
    }

    /// Stashing what's marked takes only what shows as marked: a file by
    /// its path (which takes all its changes) only when it's marked
    /// everywhere it's listed, lines only of files with marks under
    /// unstaged.
    #[test]
    fn stashing_marked_takes_only_what_shows_as_marked(
        keys in prop::collection::vec(marking_key(), 0..40),
    ) {
        let mut app = App::new(workspace().clone());
        for k in keys {
            app.on_key(k);
        }
        prop_assume!(matches!(app.mode, Mode::Normal));
        let shown = |app: &App, repo: &std::path::Path, file: &str, section: Section, whole: bool| {
            let rows = (0..app.rows.len()).filter_map(|n| app.target(n));
            let mut own = rows.filter(|t| matches!(t, Target::File { repo: r, section: s, file: f }
                if r == repo && *s == section && f == file));
            match own.next() {
                Some(t) if whole => app.marked(&t) == Marked::Wholly,
                Some(t) => app.marked(&t) != Marked::No,
                // (folded away: something of the repo's is marked, at least)
                None => app.marked_in(Some(repo)) != (0, 0),
            }
        };
        let mut by_path = vec![];
        let mut by_lines = vec![];
        for e in &app.ws.entries {
            let Some(status) = e.status() else { continue };
            for f in &status.changes.files {
                let listed: Vec<Section> = Section::ALL
                    .into_iter()
                    .filter(|s| s.files(status).contains(&f.path))
                    .collect();
                if listed.iter().all(|s| shown(&app, &e.path, &f.path, *s, true)) {
                    by_path.push((e.path.clone(), f.path.clone()));
                }
                if shown(&app, &e.path, &f.path, Section::Unstaged, false) {
                    by_lines.push((e.path.clone(), f.path.clone()));
                }
            }
        }
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        let fx = app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        for e in &fx {
            let Effect::Jobs(jobs) = e else { continue };
            for j in jobs {
                match &j.op {
                    Op::Stash { paths, .. } => {
                        prop_assert!(!paths.is_empty(), "a stash of everything in {}", j.path.display());
                        for p in paths {
                            prop_assert!(
                                by_path.contains(&(j.path.clone(), p.clone())),
                                "{p} stashed by its path, though not marked everywhere it's listed"
                            );
                        }
                    }
                    Op::StashLines { stage, .. } => {
                        for p in stage.lines().filter_map(|l| l.strip_prefix("+++ b/")) {
                            prop_assert!(
                                by_lines.contains(&(j.path.clone(), p.to_string())),
                                "lines of {p} stashed, though nothing of it shows marked under unstaged"
                            );
                        }
                    }
                    other => prop_assert!(false, "not a stash: {other:?}"),
                }
            }
        }
    }

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
