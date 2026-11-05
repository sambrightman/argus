//! TUI behaviour: keys -> state + effects, and rendering via TestBackend.

use std::path::PathBuf;

use argus_core::config::Config;
use argus_core::ops::Op;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{Action, App, Effect, Mode, Msg, Row, Scope};
use argus_tui::menu::MenuId;
use argus_tui::theme::Theme;
use argus_tui::ui;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}
fn code(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

/// Three clones: `clean`, `dirty` (modified file), `feature` (pushed feature branch).
fn setup(extra: &str) -> (Sandbox, App) {
    setup_with(extra, |_| {})
}

/// Like [`setup`], with `prep` run on the sandbox before the scan.
fn setup_with(extra: &str, prep: impl Fn(&Sandbox)) -> (Sandbox, App) {
    let sb = Sandbox::new();
    for n in ["clean", "dirty", "feature"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty"), "README", "changed\n");
    let f = sb.path("dev/feature");
    git(&f, &["switch", "-q", "-c", "feat"]);
    commit(&f, "feat");
    git(&f, &["push", "-q", "-u", "origin", "feat"]);
    prep(&sb);
    let cfg = format!("roots = ['{}']\n{extra}", sb.path("dev").display());
    let mut config = Config::parse(&cfg).unwrap();
    if !extra.contains("attention_only") {
        // Most tests look at every repo; the default view has its own test.
        config.ui.attention_only = false;
    }
    let mut ws = Workspace::new(config, State::default()).unwrap();
    ws.scan();
    (sb, App::new(ws))
}

fn names(app: &App) -> Vec<String> {
    app.rows
        .iter()
        .filter_map(|r| {
            if let Row::Repo(i) = r {
                Some(app.ws.entries[*i].display_name())
            } else {
                None
            }
        })
        .collect()
}

fn current(app: &App) -> String {
    app.current().unwrap().display_name()
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let (theme, _) = Theme::build("mono", &Default::default());
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons: argus_core::badges::UNICODE,
        tick: 0,
    };
    t.draw(|f| ui::draw(f, app, &view)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn attention_sorting_in_rows() {
    let (_sb, app) = setup("");
    assert_eq!(
        names(&app),
        vec!["acme/dirty", "acme/feature", "acme/clean"]
    );
}

#[test]
fn navigation_emacs_vi_and_arrows_agree() {
    let (_sb, mut app) = setup("");
    for (down, up) in [
        (code(KeyCode::Down), code(KeyCode::Up)),
        (ctrl('n'), ctrl('p')),
        (key('j'), key('k')),
    ] {
        app.cursor = 0;
        app.on_key(down);
        assert_eq!(current(&app), "acme/feature");
        app.on_key(up);
        assert_eq!(current(&app), "acme/dirty");
        app.on_key(up);
        assert_eq!(app.cursor, 0, "clamped at top");
    }
    app.on_key(alt('>'));
    assert_eq!(current(&app), "acme/clean");
    app.on_key(key('j'));
    assert_eq!(current(&app), "acme/clean", "clamped at bottom");
    app.on_key(alt('<'));
    assert_eq!(app.cursor, 0);
    app.on_key(key('G'));
    assert_eq!(app.cursor, 2);
    app.on_key(key('g'));
    app.on_key(key('g'));
    assert_eq!(app.cursor, 0, "vi gg");
}

#[test]
fn single_g_refreshes_without_moving() {
    let (_sb, mut app) = setup("");
    app.cursor = 2;
    let fx = app.on_key(key('g'));
    assert!(matches!(fx.as_slice(), [Effect::Refresh(p)] if p.len() == 3));
    app.on_key(key('j'));
    app.on_key(key('g'));
    assert_eq!(app.cursor, 2, "pending g is cleared by other keys");
}

#[test]
fn unfold_details_and_next_repo_skips_them() {
    let (_sb, mut app) = setup("");
    let before = app.rows.len();
    app.on_key(code(KeyCode::Tab));
    assert!(app.rows.len() > before);
    assert!(matches!(app.rows[1], Row::Detail { .. }));
    app.on_key(key('n'));
    assert_eq!(current(&app), "acme/feature");
    assert!(matches!(app.rows[app.cursor], Row::Repo(_)));
    app.on_key(key('p'));
    assert_eq!(current(&app), "acme/dirty");
    // fold from a detail line returns to the repo row
    app.on_key(key('j'));
    assert!(matches!(app.rows[app.cursor], Row::Detail { .. }));
    app.on_key(code(KeyCode::Tab));
    assert_eq!(app.rows.len(), before);
    assert_eq!(current(&app), "acme/dirty");
    assert!(matches!(app.rows[app.cursor], Row::Repo(_)));
}

#[test]
fn details_show_issues_and_fixes() {
    let (_sb, mut app) = setup("");
    app.act(Action::Bottom);
    app.act(Action::PrevRepo); // feature
    app.act(Action::ToggleExpand);
    let out = render(&mut app, 100, 30);
    assert!(out.contains("not on default branch (main)"), "{out}");
    assert!(out.contains("switch main"), "{out}");
    assert!(out.contains("origin/feat"), "{out}");
}

#[test]
fn filter_narrows_and_escape_restores() {
    let (_sb, mut app) = setup("");
    app.on_key(key('/'));
    assert!(matches!(app.mode, Mode::Input { .. }));
    for c in "feat".chars() {
        app.on_key(key(c));
    }
    assert_eq!(names(&app), vec!["acme/feature"], "live filtering");
    app.on_key(code(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.filter, "feat");
    app.on_key(code(KeyCode::Esc));
    assert_eq!(names(&app).len(), 3);
    // C-g while typing cancels and clears
    app.on_key(ctrl('s'));
    app.on_key(key('x'));
    assert!(names(&app).is_empty());
    app.on_key(ctrl('g'));
    assert_eq!(names(&app).len(), 3);
}

#[test]
fn transient_menu_by_key_and_by_arrows() {
    let (_sb, mut app) = setup("");
    // feature repo: b d = switch default
    app.act(Action::Bottom);
    app.act(Action::PrevRepo);
    app.on_key(key('b'));
    assert!(matches!(&app.mode, Mode::Menu { menu, .. } if menu.id == MenuId::Branch));
    let fx = app.on_key(key('d'));
    assert!(matches!(app.mode, Mode::Normal));
    match fx.as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Switch {
                branch: "main".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    assert!(app.running.contains_key(&app.current().unwrap().path));

    // arrows + Enter: Fetch menu, second item = fetch all visible
    let (_sb2, mut app) = setup("");
    app.on_key(key('f'));
    app.on_key(code(KeyCode::Down));
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j.len(), 3),
        other => panic!("{other:?}"),
    }
}

#[test]
fn menu_cancel_keys_do_nothing() {
    for k in [key('q'), code(KeyCode::Esc), ctrl('g')] {
        let (_sb, mut app) = setup("");
        app.on_key(key('P'));
        assert!(app.on_key(k).is_empty());
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.running.is_empty());
    }
}

#[test]
fn fix_not_applicable_explains_instead_of_running() {
    let (_sb, mut app) = setup("");
    // cursor on dirty (on main already)
    let fx = app.act(Action::SwitchDefault);
    assert!(fx.is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("not applicable"));
}

#[test]
fn marks_select_targets_and_space_advances() {
    let (_sb, mut app) = setup("");
    app.on_key(key(' '));
    assert_eq!(current(&app), "acme/feature", "space advances");
    app.on_key(key('j'));
    app.on_key(key(' '));
    assert_eq!(app.marked.len(), 2);
    let sel: Vec<String> = app
        .selected()
        .into_iter()
        .map(|i| app.ws.entries[i].display_name())
        .collect();
    assert_eq!(sel, vec!["acme/dirty", "acme/clean"]);
    app.on_key(key('U'));
    assert!(app.marked.is_empty());
    assert_eq!(app.selected().len(), 1, "falls back to current");
}

#[test]
fn toggle_favourite_persists_in_state_and_resorts() {
    let (_sb, mut app) = setup("");
    assert_eq!(names(&app), ["acme/dirty", "acme/feature", "acme/clean"]);
    app.on_key(key('j')); // feature
    app.on_key(key('m'));
    let fx = app.on_key(key('f'));
    assert!(fx.contains(&Effect::SaveState));
    assert_eq!(
        app.ws.state.marks["github.com/acme/feature"].favourite,
        Some(true)
    );
    assert_eq!(names(&app), ["acme/feature", "acme/dirty", "acme/clean"]);
    assert_eq!(current(&app), "acme/feature", "cursor follows the repo");
    // inverse: a clean favourite stays below unclean repos
    app.act(Action::Bottom);
    app.on_key(key('m'));
    app.on_key(key('f'));
    assert_eq!(names(&app)[2], "acme/clean");
    app.on_key(key('m'));
    app.on_key(key('f'));
    assert_eq!(
        app.ws.state.marks["github.com/acme/clean"].favourite,
        Some(false)
    );
}

#[test]
fn exclude_hides_and_hidden_toggle_shows() {
    let (_sb, mut app) = setup("");
    app.act(Action::ToggleExclude);
    assert_eq!(names(&app).len(), 2);
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3);
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 2);
}

#[test]
fn snooze_prompt_validates_duration() {
    let (_sb, mut app) = setup("");
    app.act(Action::Snooze);
    // default text "3d"; replace with garbage
    app.on_key(ctrl('u'));
    for c in "soon".chars() {
        app.on_key(key(c));
    }
    assert!(app.on_key(code(KeyCode::Enter)).is_empty());
    assert!(app.message.as_ref().unwrap().1, "error shown");
    assert!(app.ws.state.snoozed.is_empty());
    app.act(Action::Snooze);
    let fx = app.on_key(code(KeyCode::Enter)); // 3d
    assert!(fx.contains(&Effect::SaveState));
    assert_eq!(app.ws.state.snoozed.len(), 1);
    assert_eq!(names(&app).len(), 2, "snoozed repo hidden");
}

#[test]
fn commit_prompts_and_rejects_empty_message() {
    let (_sb, mut app) = setup("");
    app.act(Action::Commit);
    assert!(matches!(app.mode, Mode::Input { .. }));
    assert!(
        app.on_key(code(KeyCode::Enter)).is_empty(),
        "empty message aborts"
    );
    app.act(Action::Commit);
    for c in "wip".chars() {
        app.on_key(key(c));
    }
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::CommitAll {
                message: "wip".into()
            }
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn commit_on_clean_repo_does_not_prompt() {
    let (_sb, mut app) = setup("");
    app.act(Action::Bottom);
    assert!(app.act(Action::Commit).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn job_finish_logs_and_refreshes() {
    let (_sb, mut app) = setup("");
    let path = app.current().unwrap().path.clone();
    app.act(Action::Fetch(Scope::Selected));
    assert!(app.running.contains_key(&path));
    let fx = app.on_msg(Msg::JobFinished {
        path: path.clone(),
        label: "fetch acme/dirty".into(),
        op: Op::Fetch,
        outcome: argus_core::ops::OpOutcome {
            success: false,
            code: Some(1),
            output: "boom".into(),
            duration: Default::default(),
        },
    });
    assert_eq!(fx, vec![Effect::Refresh(vec![path.clone()])]);
    assert!(!app.running.contains_key(&path));
    assert_eq!(app.log.len(), 1);
    assert!(app.message.as_ref().unwrap().1);
    // a repo with a job in flight doesn't get a second one
    app.act(Action::Fetch(Scope::Selected));
    assert!(app.act(Action::Fetch(Scope::Selected)).is_empty());
}

#[test]
fn auto_fix_only_targets_repos_with_applicable_fixes() {
    let (_sb, mut app) = setup("");
    match app.act(Action::AutoFix(Scope::All)).as_slice() {
        [Effect::Fix { paths, .. }] => {
            assert_eq!(paths.len(), 1);
            assert!(paths[0].ends_with("feature"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn shell_and_quit_effects() {
    let (_sb, mut app) = setup("");
    let p = app.current().unwrap().path.clone();
    match app.on_key(key('!')).as_slice() {
        [Effect::Suspend { cwd, .. }] => assert_eq!(cwd, &p),
        other => panic!("{other:?}"),
    }
    assert_eq!(app.on_key(key('q')), vec![Effect::SaveState, Effect::Quit]);
    assert!(app.quit);
}

#[test]
fn git_ui_requires_config() {
    let (_sb, mut app) = setup("");
    assert!(app.act(Action::GitUi).is_empty());
    assert!(app.message.as_ref().unwrap().1);
    let (_sb, mut app) = setup("[tools]\ngit_ui = 'lazygit'");
    assert!(
        matches!(app.act(Action::GitUi).as_slice(), [Effect::Suspend { program, args, .. }] if program == "sh" && args[1] == "lazygit")
    );
}

#[test]
fn render_main_screen() {
    let (_sb, mut app) = setup("");
    let out = render(&mut app, 80, 8);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("3 repos · 2 need attention"), "{out}");
    assert!(
        lines[1].contains("acme/dirty") && lines[1].contains("main") && lines[1].contains("±1"),
        "{out}"
    );
    assert!(
        lines[2].contains("acme/feature") && lines[2].contains("feat"),
        "{out}"
    );
    assert!(lines[3].contains("acme/clean"), "{out}");
    assert!(lines[7].contains("? help"), "{out}");
}

#[test]
fn render_menu_and_help_and_narrow_terminal() {
    let (_sb, mut app) = setup("");
    app.on_key(key('m'));
    let out = render(&mut app, 80, 16);
    assert!(
        out.contains("Mark · 1 repo") && out.contains("toggle favourite"),
        "{out}"
    );
    app.on_key(code(KeyCode::Esc));
    app.on_key(key('?'));
    let out = render(&mut app, 100, 40);
    assert!(out.contains("transient") || out.contains("Pull:"), "{out}");
    app.on_key(key('x'));
    // tiny terminals must not panic
    for (w, h) in [(1, 1), (10, 3), (20, 2)] {
        render(&mut app, w, h);
    }
}

#[test]
fn render_empty_states() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    assert!(render(&mut app, 80, 5).contains("no repositories"));
    // actions on an empty list are harmless
    for a in [
        Action::Down,
        Action::ToggleExpand,
        Action::ToggleMark,
        Action::Shell,
        Action::Push,
        Action::ToggleFavourite,
    ] {
        app.act(a);
    }
}

#[test]
fn scanned_message_replaces_entries_and_keeps_cursor_repo() {
    let (_sb, mut app) = setup("");
    app.act(Action::Bottom);
    let path = app.current().unwrap().path.clone();
    let entries = app.ws.entries.clone();
    app.notices = vec!["no roots configured".into()];
    let truncated = vec![PathBuf::from("/deep/dir")];
    app.on_msg(Msg::Scanned { entries, truncated });
    assert_eq!(app.current().unwrap().path, path);
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(!is_error);
    assert!(msg.starts_with("3 repos · no roots configured · 1 directory below max_depth (6) not scanned, e.g. /deep/dir"), "{msg}");
    let _ = PathBuf::new();
}

fn detail_texts(app: &App) -> Vec<String> {
    app.rows
        .iter()
        .filter_map(|r| match r {
            Row::Detail { line, .. } => Some(
                line.parts
                    .iter()
                    .map(|(_, t)| t.as_str())
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn details_show_the_auto_fix_its_chain_and_alternatives() {
    let untracked = |sb: &Sandbox| write(&sb.path("dev/feature"), "notes.txt", "x\n");
    let fixes = |auto: &str| {
        let (_sb, mut app) = setup_with(&format!("[fix]\nauto = {auto}"), untracked);
        app.on_key(key('j')); // feature
        app.on_key(code(KeyCode::Tab));
        detail_texts(&app)
            .into_iter()
            .filter(|t| {
                ["auto-fix", "then", "fix "]
                    .iter()
                    .any(|p| t.starts_with(p))
            })
            .map(|t| t.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fixes("['stash', 'switch-default']"),
        ["auto-fix stash 1", "then switch main"]
    );
    assert_eq!(
        fixes("['switch-default', 'stash']"),
        ["auto-fix switch main", "then stash 1"]
    );
    assert_eq!(
        fixes("['switch-default']"),
        ["auto-fix switch main", "fix stash 1"]
    );
    // nothing allowed: no auto-fix, alternatives in the default order
    assert_eq!(fixes("[]"), ["fix stash 1", "fix switch main"]);
}

#[test]
fn details_say_why_a_fix_is_not_automatic() {
    let sb = Sandbox::new();
    let r = sb.diverged("acme", "w", "dev/w", true);
    git(&r, &["config", "pull.rebase", "false"]);
    let cfg = format!(
        "roots = ['{}']\n[fix]\nauto = ['pull']",
        sb.path("dev").display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    app.on_key(code(KeyCode::Tab));
    let fixes: Vec<_> = detail_texts(&app)
        .into_iter()
        .filter(|t| t.starts_with("fix") || t.starts_with("auto-fix"))
        .collect();
    assert_eq!(fixes.len(), 1, "{fixes:?}");
    assert!(
        fixes[0].starts_with("fix       pull"),
        "never the auto-fix: {fixes:?}"
    );
    assert!(
        fixes[0].ends_with("not automatic: would conflict in README"),
        "{fixes:?}"
    );
}

#[test]
fn starts_showing_only_repos_needing_attention() {
    let sb = Sandbox::new();
    for n in ["clean", "dirty"] {
        sb.remote("acme", n);
        sb.clone_as_github("acme", n, &format!("dev/{n}"));
    }
    write(&sb.path("dev/dirty"), "README", "changed\n");
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    assert_eq!(names(&app), vec!["acme/dirty"]);
    let out = render(&mut app, 80, 6);
    assert!(
        out.lines()
            .next()
            .unwrap()
            .contains("1 of 2 repos need attention · a: show all"),
        "{out}"
    );
    // inverse: `a` shows everything, and the header says so
    app.act(Action::ToggleAttention);
    assert_eq!(names(&app), vec!["acme/dirty", "acme/clean"]);
    let out = render(&mut app, 80, 6);
    assert!(
        out.lines()
            .next()
            .unwrap()
            .contains("2 repos · 1 need attention"),
        "{out}"
    );
    // and the config can start with everything shown
    let cfg = format!(
        "roots = ['{}']\n[ui]\nattention_only = false",
        sb.path("dev").display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    assert_eq!(names(&App::new(ws)).len(), 2);
}

#[test]
fn no_repositories_is_not_reported_as_nothing_needing_attention() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path("dev")).unwrap();
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    assert!(render(&mut app, 80, 5).contains("no repositories"));
    app.act(Action::ToggleAttention);
    assert!(render(&mut app, 80, 5).contains("no repositories"));
    // inverse: repos that are all clean do need nothing
    sb.remote("acme", "clean");
    sb.clone_as_github("acme", "clean", "dev/clean");
    app.ws.scan();
    app.act(Action::ToggleAttention);
    assert!(render(&mut app, 80, 5).contains("nothing needs attention"));
}

#[test]
fn details_show_the_commit_and_its_tags() {
    let tag = |sb: &Sandbox| {
        let c = sb.path("dev/clean");
        git(&c, &["tag", "v1.0"]);
        git(&c, &["tag", "-a", "-m", "r", "release"]);
    };
    let (sb, mut app) = setup_with("", tag);
    app.act(Action::Bottom); // clean
    app.on_key(code(KeyCode::Tab));
    let id = git(&sb.path("dev/clean"), &["rev-parse", "--short", "HEAD"]);
    let texts = detail_texts(&app);
    assert!(
        texts.contains(&format!("commit    {}  tags release, v1.0", id.trim())),
        "{texts:?}"
    );
    // inverse: no tags, just the commit
    let (sb, mut app) = setup("");
    app.act(Action::Bottom);
    app.on_key(code(KeyCode::Tab));
    let id = git(&sb.path("dev/clean"), &["rev-parse", "--short", "HEAD"]);
    assert!(detail_texts(&app).contains(&format!("commit    {}", id.trim())));
}

#[test]
fn view_menu_cycles_themes_and_icon_sets_for_this_session() {
    let (_sb, mut app) = setup("[ui]\ntheme = 'light'\nicons = 'nerd'");
    let mut themes = vec![];
    for _ in 0..4 {
        app.on_key(key('v'));
        app.on_key(key('t'));
        themes.push(app.ws.config.ui.theme.clone());
    }
    assert_eq!(themes, ["mono", "terminal", "dark", "light"]);
    assert!(
        app.message.as_ref().unwrap().0.contains("theme light"),
        "{:?}",
        app.message
    );
    let mut sets = vec![];
    for _ in 0..3 {
        app.on_key(key('v'));
        app.on_key(key('i'));
        sets.push(app.ws.config.ui.icons.clone());
    }
    assert_eq!(sets, ["ascii", "unicode", "nerd"]);
    // an unknown configured name starts the cycle from the beginning
    let (_sb, mut app) = setup("[ui]\ntheme = 'nope'");
    app.act(Action::NextTheme);
    assert_eq!(app.ws.config.ui.theme, "terminal");
}

#[test]
fn help_has_a_legend_of_every_icon() {
    let (_sb, mut app) = setup("");
    app.on_key(key('?'));
    let out = render(&mut app, 100, 80);
    for (icon, meaning) in [
        ("◆", "favourite"),
        ("±", "unstaged changes"),
        ("?", "untracked files"),
        ("@", "detached HEAD (tag or commit)"),
        ("$", "stashes"),
        ("≠", "conflicts"),
    ] {
        assert!(
            out.lines().any(|l| l.contains(icon) && l.contains(meaning)),
            "{icon} {meaning}\n{out}"
        );
    }
}

#[test]
fn demo_renders_in_every_theme_and_icon_set() {
    let d = argus_core::demo::build();
    let cfg = format!(
        "roots = ['{}']\n[ui]\nattention_only = false",
        d.root.display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    d.apply_marks(&mut ws);
    let mut app = App::new(ws);
    app.act(Action::ExpandAll);
    let out = render(&mut app, 120, 200);
    if std::env::var_os("ARGUS_SHOW_DEMO").is_some() {
        println!("{out}");
    }
    assert!(
        out.contains("demo/tagged") && out.contains("@release-1.0 +1"),
        "{out}"
    );
    assert!(out.contains("tags release-1.0, v1.0"), "{out}");
    for theme in argus_tui::theme::BUILTIN {
        for icons in argus_core::badges::SETS {
            let (t, warnings) = Theme::build(theme, &Default::default());
            assert!(warnings.is_empty(), "{theme}: {warnings:?}");
            let mut term = Terminal::new(TestBackend::new(100, 60)).unwrap();
            let view = ui::View {
                theme: &t,
                icons: argus_core::badges::by_name(icons),
                tick: 0,
            };
            term.draw(|f| ui::draw(f, &mut app, &view)).unwrap();
        }
    }
}
