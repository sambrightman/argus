//! Properties that must hold for any input, checked on generated ones
//! (proptest shrinks a failure to a minimal case).

use std::sync::OnceLock;

use argus_core::config::Config;
use argus_core::diff::LineKind;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{App, Mode};
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
            term.draw(|f| ui::draw(f, &mut app, &view)).unwrap();
            prop_assert!(app.scroll <= app.cursor || app.rows.is_empty());
        }
        // and the app can always get back to normal
        for _ in 0..3 {
            app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        prop_assert!(matches!(app.mode, Mode::Normal), "{:?}", std::mem::discriminant(&app.mode));
    }
}
