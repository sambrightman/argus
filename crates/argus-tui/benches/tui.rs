//! Benchmarks for the TUI: word refinement on a large hunk, and rebuilding
//! and drawing the screen with many repos, details open, and a diff with
//! changed words highlighted.
//!
//! ```sh
//! cargo bench -p argus-tui
//! ```

use std::hint::black_box;

use argus_core::config::Config;
use argus_core::diff::LineKind;
use argus_core::testkit::{Sandbox, commit, write};
use argus_core::{State, Workspace};
use argus_tui::app::{Action, App};
use argus_tui::refine::refine;
use argus_tui::theme::Theme;
use argus_tui::ui;
use criterion::{Criterion, criterion_group, criterion_main};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn refinement(c: &mut Criterion) {
    let removed: Vec<String> = (0..100)
        .map(|i| format!("    let value_{i} = compute(alpha, beta, {i}); // old"))
        .collect();
    let added: Vec<String> = (0..100)
        .map(|i| format!("    let value_{i} = compute(alpha, gamma, {i}); // new"))
        .collect();
    let lines: Vec<(LineKind, &str)> = removed
        .iter()
        .map(|l| (LineKind::Remove, l.as_str()))
        .chain(added.iter().map(|l| (LineKind::Add, l.as_str())))
        .collect();
    c.bench_function("refine 100 removed + 100 added lines", |b| {
        b.iter(|| black_box(refine(&lines)))
    });
}

fn screen(c: &mut Criterion) {
    let sb = Sandbox::new();
    for i in 0..40 {
        let r = sb.init(&format!("dev/r{i:02}"));
        if i % 4 == 0 {
            write(&r, "README", "changed\n");
        }
    }
    // a repo with a 200-line file changed throughout
    let big = sb.path("dev/r00");
    let text = |w: &str| {
        (0..200)
            .map(|i| format!("line {i} {w}\n"))
            .collect::<String>()
    };
    write(&big, "big", &text("before"));
    commit(&big, "big");
    write(&big, "big", &text("after"));
    let cfg = format!(
        "roots = ['{}']\n[ui]\nattention_only = false",
        sb.path("dev").display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    app.act(Action::ExpandAll);
    // open big's diff
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, argus_tui::app::Row::Detail { line, .. } if line.file.as_deref() == Some("big")))
        .unwrap();
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let (theme, _) = Theme::build("dark", &Default::default());
    let mut term = Terminal::new(TestBackend::new(160, 60)).unwrap();
    let mut g = c.benchmark_group("tui");
    g.bench_function("rebuild rows (40 repos, details, a diff)", |b| {
        b.iter(|| app.rebuild())
    });
    g.bench_function("draw a frame (160x60, words highlighted)", |b| {
        let view = ui::View {
            theme: &theme,
            icons: argus_core::icons::UNICODE,
            tick: 0,
        };
        b.iter(|| {
            term.draw(|f| ui::draw(f, &mut app, &view)).unwrap();
        })
    });
    g.finish();
}

criterion_group!(benches, refinement, screen);
criterion_main!(benches);
