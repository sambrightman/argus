//! Render one TUI frame for the given roots to stdout (debugging aid):
//! `cargo run -p argus-tui --example snapshot -- ~/dev [keys]`
use argus_core::{Config, State, Workspace};
use argus_tui::{app::App, theme::Theme, ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("root");
    let keys = args.next().unwrap_or_default();
    let cfg = Config { roots: vec![root.into()], ..Default::default() };
    let mut ws = Workspace::new(cfg, State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    for c in keys.chars() {
        let k = match c {
            '\t' => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            c => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
        };
        app.on_key(k);
    }
    let (theme, _) = Theme::build("terminal", &Default::default());
    let (w, h) = (110, 30);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons: argus_core::badges::UNICODE,
        tick: 0,
    };
    t.draw(|f| ui::draw(f, &mut app, &view)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        println!("{}", line.trim_end());
    }
}
