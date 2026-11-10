//! TUI behaviour: keys -> state + effects, and rendering via TestBackend.

use std::path::PathBuf;

use argus_core::config::Config;
use argus_core::ops::Op;
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{Action, App, Effect, Mode, Msg, Row, Scope, StashKind};
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
    render_with(app, w, h, argus_core::icons::UNICODE)
}

fn render_with(app: &mut App, w: u16, h: u16, icons: argus_core::icons::Icons) -> String {
    let (theme, _) = Theme::build("mono", &Default::default());
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons,
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
    app.on_key(alt('n'));
    assert_eq!(current(&app), "acme/feature");
    assert!(matches!(app.rows[app.cursor], Row::Repo(_)));
    app.on_key(alt('p'));
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
fn toggle_favourite_edits_the_config_list_and_resorts() {
    let (_sb, mut app) = setup("");
    assert_eq!(names(&app), ["acme/dirty", "acme/feature", "acme/clean"]);
    app.on_key(key('j')); // feature
    app.on_key(key('m'));
    let before = app.ws.config.clone();
    let fx = app.on_key(key('f'));
    // saved as a change from the config before
    assert_eq!(fx, vec![Effect::SaveMarks(Box::new(before))]);
    assert_eq!(app.ws.config.favourite, ["github.com/acme/feature"]);
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
        app.ws.config.favourite,
        ["github.com/acme/feature"],
        "off again: removed"
    );
}

#[test]
fn ignore_hides_and_hidden_toggle_shows() {
    let (_sb, mut app) = setup("");
    app.act(Action::ToggleIgnore);
    assert_eq!(names(&app).len(), 2);
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3);
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 2);
}

#[test]
fn hidden_repos_get_no_bulk_actions_even_when_shown() {
    let (_sb, mut app) = setup("");
    let ignored = app.current().unwrap().path.clone();
    app.act(Action::ToggleIgnore);
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3, "shown");
    let paths = |fx: Vec<Effect>| match fx.as_slice() {
        [Effect::Jobs(j)] => j.iter().map(|j| j.path.clone()).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let all = paths(app.act(Action::Fetch(Scope::All)));
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(!all.contains(&ignored), "{all:?}");
    // inverse: chosen explicitly, it's acted on
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == ignored))
        .unwrap();
    assert_eq!(paths(app.act(Action::Fetch(Scope::Selected))), [ignored]);
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
    app.act(Action::Commit { all: true });
    assert!(matches!(app.mode, Mode::Input { .. }));
    assert!(
        app.on_key(code(KeyCode::Enter)).is_empty(),
        "empty message aborts"
    );
    app.on_key(key('c'));
    app.on_key(key('a'));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("commit all changes (1 repo(s)):"),
        "c a: {screen}"
    );
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
    for all in [true, false] {
        assert!(app.act(Action::Commit { all }).is_empty());
        assert!(matches!(app.mode, Mode::Normal));
    }
}

#[test]
fn c_c_commits_only_whats_staged() {
    let (sb, mut app) = setup("");
    // dirty has only unstaged changes: nothing to commit staged
    app.on_key(key('c'));
    app.on_key(key('c'));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "nothing staged (c a commits all changes)"
    );
    let d = sb.path("dev/dirty");
    git(&d, &["add", "README"]);
    app.ws.refresh(&d);
    app.rebuild();
    app.on_key(key('c'));
    app.on_key(key('c'));
    assert!(
        render(&mut app, 100, 20).contains("commit staged changes (1 repo(s)):"),
        "prompts"
    );
    for c in "wip".chars() {
        app.on_key(key(c));
    }
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Commit {
                message: "wip".into()
            }
        ),
        other => panic!("{other:?}"),
    }
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
fn auto_fix_all_leaves_hidden_repos_alone_even_when_shown() {
    // feature is the one repo with an applicable fix (see above)
    let (_sb, mut app) = setup("ignore.repos = ['acme/feature']");
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3, "shown");
    assert!(app.act(Action::AutoFix(Scope::All)).is_empty());
}

#[test]
fn auto_fix_leaves_an_ignored_state_alone() {
    // feature's only fix switches it to main; its branch is ignored
    let (_sb, mut app) = setup("[ignore]\nnon_default_branch = ['acme/feature@feat']");
    assert!(app.act(Action::AutoFix(Scope::All)).is_empty());
    let texts: Vec<String> = {
        app.cursor = app
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("feature")))
            .unwrap();
        app.on_key(code(KeyCode::Tab));
        detail_texts(&app)
    };
    assert!(
        !texts.iter().any(|t| t.starts_with("auto-fix")),
        "{texts:?}"
    );
}

/// Put the cursor on the details line of `repo` starting with `text`,
/// opening the repo's details first.
fn cursor_on_detail(app: &mut App, repo: &str, text: &str) {
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with(repo)))
        .unwrap();
    if !matches!(app.rows.get(app.cursor + 1), Some(Row::Detail { .. })) {
        app.on_key(code(KeyCode::Tab));
    }
    let at = app
        .rows
        .iter()
        .position(|r| match r {
            Row::Detail { line, .. } => line
                .parts
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<String>()
                .starts_with(text),
            _ => false,
        })
        .unwrap_or_else(|| panic!("no {text:?}: {:?}", detail_texts(app)));
    app.cursor = at;
}

#[test]
fn shell_and_quit_effects() {
    let (_sb, mut app) = setup("");
    let p = app.current().unwrap().path.clone();
    match app.on_key(key('!')).as_slice() {
        [
            Effect::Suspend {
                cwd, login_shell, ..
            },
        ] => assert!(cwd == &p && *login_shell),
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
        matches!(app.act(Action::GitUi).as_slice(), [Effect::Suspend { program, args, login_shell, .. }] if program == "sh" && args[1] == "lazygit" && !login_shell)
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
        out.contains("Mark · 1 repo") && out.contains("f  favourite"),
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
            // not those that ask for something first (commit…, gitignore…)
            .filter(|t| !t.contains('…'))
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
            .contains("2 repos · 1 need attention · a: attention only"),
        "{out}"
    );
    assert!(!out.contains("H: "), "nothing hidden: {out}");
    // hidden repos: H shows them, and hides them again
    let clean = app
        .ws
        .entries
        .iter()
        .find(|e| e.display_name() == "acme/clean");
    app.ws.state.snoozed.insert(
        clean.unwrap().key.clone(),
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    app.ws.resolve_flags();
    app.rebuild();
    let header = |app: &mut App| render(app, 100, 6).lines().next().unwrap().to_string();
    assert!(
        header(&mut app)
            .ends_with("1 repos · 1 need attention · a: attention only · H: show 1 hidden"),
        "{}",
        header(&mut app)
    );
    app.act(Action::ToggleHidden);
    assert!(
        header(&mut app).contains(
            "2 repos · 1 need attention · a: attention only · showing hidden · H: hide them"
        ),
        "{}",
        header(&mut app)
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
    let out = render(&mut app, 80, 5);
    assert!(out.contains("nothing needs attention 🎉"), "{out}");
    // the celebration is an icon: none in the ascii set
    let out = render_with(&mut app, 80, 5, argus_core::icons::ASCII);
    assert!(
        out.contains("nothing needs attention  (a: show all)"),
        "{out}"
    );
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
    let id = git(&sb.path("dev/clean"), &["rev-parse", "HEAD"]);
    let texts = detail_texts(&app);
    let at = texts
        .iter()
        .position(|t| *t == format!("commit    {}", id.trim()))
        .unwrap_or_else(|| panic!("full commit id on its own: {texts:?}"));
    assert_eq!(texts[at + 1], "tags      release, v1.0", "{texts:?}");
    // inverse: no tags, just the commit
    let (sb, mut app) = setup("");
    app.act(Action::Bottom);
    app.on_key(code(KeyCode::Tab));
    let id = git(&sb.path("dev/clean"), &["rev-parse", "HEAD"]);
    let texts = detail_texts(&app);
    assert!(
        texts.contains(&format!("commit    {}", id.trim())),
        "{texts:?}"
    );
    assert!(!texts.iter().any(|t| t.starts_with("tags")), "{texts:?}");
    assert!(
        !texts.iter().any(|t| t.starts_with("describe")),
        "{texts:?}"
    );
}

#[test]
fn details_describe_a_detached_head_past_a_tag() {
    let prep = |sb: &Sandbox| {
        let c = sb.path("dev/clean");
        git(&c, &["tag", "v1.0"]);
        commit(&c, "after the tag");
        git(&c, &["switch", "-q", "--detach"]);
    };
    let (sb, mut app) = setup_with("", prep);
    let clean = sb.path("dev/clean");
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == clean))
        .unwrap();
    app.on_key(code(KeyCode::Tab));
    let want = git(&clean, &["describe", "--tags"]);
    let texts = detail_texts(&app);
    assert!(
        texts.contains(&format!("describe  {}", want.trim())),
        "{texts:?}"
    );
}

#[test]
fn opening_details_asks_once_for_a_full_describe() {
    let prep = |sb: &Sandbox| {
        let c = sb.path("dev/clean");
        git(&c, &["tag", "v1.0"]);
        commit(&c, "one");
        commit(&c, "two");
        git(&c, &["switch", "-q", "--detach"]);
    };
    let (sb, mut app) = setup_with("describe_limit = 1", prep);
    let clean = sb.path("dev/clean");
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == clean))
        .unwrap();
    assert_eq!(
        app.act(Action::ToggleExpand),
        [Effect::Describe(clean.clone())]
    );
    let texts = detail_texts(&app);
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("describe  searching further back")),
        "{texts:?}"
    );
    // closed and opened again while it runs: not asked twice
    app.act(Action::ToggleExpand);
    assert!(app.act(Action::ToggleExpand).is_empty());
    // done: a refresh picks up the remembered answer
    assert_eq!(
        app.on_msg(Msg::Described(clean.clone())),
        [Effect::Refresh(vec![clean.clone()])]
    );
    assert!(app.describing.is_empty());
    // inverse: a repo whose walk didn't give up isn't described again
    let (_sb, mut app) = setup("");
    assert!(app.act(Action::ExpandAll).is_empty());
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
    // open details switch too
    let (_sb, mut app) = setup_with("", |sb| commit(&sb.path("dev/clean"), "local"));
    app.act(Action::ExpandAll);
    let ahead = |app: &App| detail_texts(app).iter().any(|t| t.ends_with(" ↑1"));
    assert!(ahead(&app), "{:?}", detail_texts(&app));
    app.act(Action::NextIcons); // nerd
    app.act(Action::NextIcons); // ascii
    assert!(!ahead(&app), "{:?}", detail_texts(&app));
    assert!(
        detail_texts(&app).iter().any(|t| t.ends_with(" ^1")),
        "{:?}",
        detail_texts(&app)
    );
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
        ("⠋", "job running"),
        ("▌", "marked for a batch action"),
        ("✓", "job or fix succeeded"),
        ("✗", "job or fix failed"),
    ] {
        assert!(
            out.lines().any(|l| l.contains(icon) && l.contains(meaning)),
            "{icon} {meaning}\n{out}"
        );
    }
}

#[test]
fn ascii_icons_render_without_other_symbols() {
    let d = argus_core::demo::build();
    let cfg = format!(
        "roots = ['{}']\n[ui]\nattention_only = false\nicons = 'ascii'",
        d.root.display()
    );
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    d.apply_marks(&mut ws);
    let mut app = App::new(ws);
    app.act(Action::ToggleMark);
    app.act(Action::ExpandAll);
    let ascii = argus_core::icons::ASCII;
    let main = render_with(&mut app, 120, 300, ascii.clone());
    app.on_key(key('?'));
    let help = render_with(&mut app, 120, 120, ascii.clone());
    let allowed = |c: char| {
        c.is_ascii()
            || argus_core::icons::PUNCTUATION.contains(c)
            || ('\u{2500}'..='\u{257f}').contains(&c) // borders
    };
    for out in [&main, &help] {
        let other: String = out.chars().filter(|&c| !allowed(c)).collect();
        assert!(other.is_empty(), "{other}\n{out}");
    }
    assert!(main.contains(">"), "marked row: {main}");
    assert!(
        main.contains("^"),
        "ahead, in badges and branch details: {main}"
    );
    assert!(help.contains("FAILED"), "legend: {help}");
    // inverse: the unicode set does use its symbols
    app.on_key(code(KeyCode::Esc));
    app.ws.config.ui.icons = "unicode".into();
    app.rebuild();
    let main = render(&mut app, 120, 300);
    assert!(main.contains('▌') && main.contains('↑'), "{main}");
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
        out.contains("demo/tagged") && out.contains("@release-1.0 (& 1 more)"),
        "{out}"
    );
    assert!(out.contains("tags      release-1.0, v1.0"), "{out}");
    for theme in argus_tui::theme::BUILTIN {
        for icons in argus_core::icons::SETS {
            let (t, warnings) = Theme::build(theme, &Default::default());
            assert!(warnings.is_empty(), "{theme}: {warnings:?}");
            let mut term = Terminal::new(TestBackend::new(100, 60)).unwrap();
            let view = ui::View {
                theme: &t,
                icons: argus_core::icons::by_name(icons),
                tick: 0,
            };
            term.draw(|f| ui::draw(f, &mut app, &view)).unwrap();
        }
    }
}

#[test]
fn clearing_marks_of_several_repos_asks_first() {
    use argus_core::rules::Mark;
    let (_sb, mut app) = setup("");
    let all: Vec<usize> = (0..app.ws.entries.len()).collect();
    app.ws.set_mark(&all, Mark::Favourite, true).unwrap();
    let favourites = |app: &App| app.ws.config.favourite.len();
    for e in &app.ws.entries {
        app.marked.insert(e.path.clone());
    }
    let fx = app.act(Action::ClearMarks);
    assert!(fx.is_empty(), "nothing saved before an answer: {fx:?}");
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(render(&mut app, 100, 8).contains("Clear marks of 3 repos? (y/n)"));
    // anything but y cancels, changing nothing
    let fx = app.on_key(key('n'));
    assert!(fx.is_empty() && matches!(app.mode, Mode::Normal));
    assert_eq!(favourites(&app), 3);
    assert!(app.message.as_ref().unwrap().0.contains("cancelled"));
    app.act(Action::ClearMarks);
    app.on_key(code(KeyCode::Esc));
    assert_eq!(favourites(&app), 3);
    // y clears them, saving config and state
    let before = app.ws.config.clone();
    app.act(Action::ClearMarks);
    let fx = app.on_key(key('y'));
    assert!(fx.contains(&Effect::SaveMarks(Box::new(before))) && fx.contains(&Effect::SaveState));
    assert_eq!(favourites(&app), 0);
    // one repo losing marks: no question (a snooze doesn't count)
    app.ws.set_mark(&[0], Mark::Ignore, true).unwrap();
    let k1 = app.ws.entries[1].key.clone();
    app.ws.state.snoozed.insert(
        k1,
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    let fx = app.act(Action::ClearMarks);
    assert!(fx.contains(&Effect::SaveState) && matches!(app.mode, Mode::Normal));
    assert!(app.ws.config.ignore.repos.is_empty() && app.ws.state.snoozed.is_empty());
}

/// The stash ops in `fx`'s jobs, by repo name.
fn stash_jobs(app: &App, fx: &[Effect]) -> Vec<(String, Op)> {
    fx.iter()
        .flat_map(|e| match e {
            Effect::Jobs(j) => j.clone(),
            _ => vec![],
        })
        .map(|j| (app.ws.get(&j.path).unwrap().display_name(), j.op))
        .collect()
}

fn stash_op(
    untracked: bool,
    keep_index: bool,
    staged: bool,
    paths: &[&str],
) -> impl Fn(&Op) -> bool {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    move |op| {
        matches!(op, Op::Stash { untracked: u, keep_index: k, staged: s, paths: p, .. }
            if *u == untracked && *k == keep_index && *s == staged && *p == paths)
    }
}

#[test]
fn stash_menu_switches_stay_open_and_last_the_session() {
    let (_sb, mut app) = setup("[stash]\nkeep_index = true");
    assert_eq!(current(&app), "acme/dirty");
    app.on_key(key('z'));
    let out = render(&mut app, 100, 30);
    assert!(
        out.contains("[x] include untracked") && out.contains("[x] keep index"),
        "{out}"
    );
    app.on_key(key('u'));
    app.on_key(key('k'));
    assert!(
        matches!(app.mode, Mode::Menu { .. }),
        "switches keep the menu open"
    );
    let out = render(&mut app, 100, 30);
    assert!(
        out.contains("[ ] include untracked") && out.contains("[ ] keep index"),
        "{out}"
    );
    app.on_key(code(KeyCode::Esc));
    app.on_key(key('z'));
    assert!(
        render(&mut app, 100, 30).contains("[ ] include untracked"),
        "this session"
    );
    assert!(app.ws.config.stash.untracked, "config unchanged");
    let fx = app.on_key(key('z'));
    assert!(matches!(app.mode, Mode::Normal));
    let jobs = stash_jobs(&app, &fx);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].0, "acme/dirty");
    assert!(
        stash_op(false, false, false, &[])(&jobs[0].1),
        "{:?}",
        jobs[0].1
    );
}

#[test]
fn stash_staged_only_and_refusals() {
    let staged = |sb: &Sandbox| {
        let d = sb.path("dev/dirty");
        write(&d, "staged", "s\n");
        git(&d, &["add", "staged"]);
    };
    let (sb, mut app) = setup_with("", staged);
    // a clean repo: nothing to do, said so
    app.act(Action::Bottom);
    assert_eq!(current(&app), "acme/clean");
    app.on_key(key('z'));
    let fx = app.on_key(key('z'));
    assert!(fx.is_empty());
    assert!(
        app.message.as_ref().unwrap().0.contains("nothing to stash"),
        "{:?}",
        app.message
    );
    let (_, is_error) = app.message.clone().unwrap();
    assert!(!is_error, "skipped, not refused");
    // a clean repo among those selected is skipped; the others are stashed
    for e in &app.ws.entries {
        app.marked.insert(e.path.clone());
    }
    let fx = app.act(Action::Stash(StashKind::All));
    let names: Vec<String> = stash_jobs(&app, &fx).into_iter().map(|(n, _)| n).collect();
    assert!(
        names.contains(&"acme/dirty".to_string()) && !names.contains(&"acme/clean".to_string()),
        "{names:?}"
    );
    app.marked.clear();
    app.running.clear();
    // staged only
    app.act(Action::Top);
    app.on_key(key('z'));
    let fx = app.on_key(key('i'));
    let jobs = stash_jobs(&app, &fx);
    assert!(
        jobs.len() == 1 && stash_op(true, false, true, &[])(&jobs[0].1),
        "{jobs:?}"
    );
    // a staged file with unstaged changes too: refused, nothing run
    let (sb2, mut app) = setup_with("", staged);
    let d = sb2.path("dev/dirty");
    write(&d, "staged", "s2\n");
    app.ws.refresh(&d);
    app.rebuild();
    app.on_key(key('z'));
    let fx = app.on_key(key('i'));
    assert!(fx.is_empty(), "{fx:?}");
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(
        is_error && msg.contains("unstaged") && msg.contains("acme/dirty"),
        "{msg}"
    );
    drop(sb);
}

#[test]
fn stash_with_a_message() {
    let (_sb, mut app) = setup("");
    app.on_key(key('z'));
    app.on_key(key('m'));
    assert!(matches!(app.mode, Mode::Input { .. }));
    for c in "wip".chars() {
        app.on_key(key(c));
    }
    let fx = app.on_key(code(KeyCode::Enter));
    let jobs = stash_jobs(&app, &fx);
    assert!(
        matches!(&jobs[..], [(n, Op::Stash { message, .. })] if n == "acme/dirty" && message == "wip"),
        "{jobs:?}"
    );
}

#[test]
fn files_marked_in_details_can_be_stashed() {
    let (sb, mut app) = setup_with("", |sb| write(&sb.path("dev/dirty"), "other", "x\n"));
    // nothing marked yet
    app.on_key(key('z'));
    let fx = app.on_key(key('f'));
    assert!(fx.is_empty());
    assert!(
        app.message.as_ref().unwrap().0.contains("mark files"),
        "{:?}",
        app.message
    );
    // mark README in the details
    app.on_key(code(KeyCode::Tab));
    let at = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some("README")),
        )
        .expect("README listed in the details");
    app.cursor = at;
    app.on_key(key(' '));
    assert_eq!(app.cursor, at + 1, "moves on, like marking repos");
    assert!(app.marked.is_empty(), "the repo itself isn't marked");
    let out = render(&mut app, 100, 30);
    assert!(
        out.lines().any(|l| l.contains('▌') && l.contains("README")),
        "{out}"
    );
    // with its diff open, only the file's own line carries the mark
    app.cursor = at;
    app.on_key(code(KeyCode::Tab));
    let out = render(&mut app, 100, 30);
    assert_eq!(out.lines().filter(|l| l.contains('▌')).count(), 1, "{out}");
    app.cursor = at;
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('z'));
    let fx = app.on_key(key('f'));
    let jobs = stash_jobs(&app, &fx);
    assert!(
        jobs.len() == 1 && stash_op(true, false, false, &["README"])(&jobs[0].1),
        "{jobs:?}"
    );
    // marks go on unmark-all, or when the file is no longer changed
    assert!(!app.marked_files.is_empty());
    app.on_key(code(KeyCode::Esc));
    assert!(app.marked_files.is_empty());
    app.cursor = at;
    app.on_key(key(' '));
    assert!(!app.marked_files.is_empty());
    let d = sb.path("dev/dirty");
    git(&d, &["checkout", "-q", "--", "README"]);
    app.ws.refresh(&d);
    app.rebuild();
    assert!(app.marked_files.is_empty(), "{:?}", app.marked_files);
}

/// Detail rows' text, trimmed.
fn trimmed(app: &App) -> Vec<String> {
    detail_texts(app)
        .iter()
        .map(|t| t.trim().to_string())
        .collect()
}

/// Expand the current repo and put the cursor on `file`'s line.
fn to_file(app: &mut App, file: &str) {
    if !trimmed(app).iter().any(|t| t.ends_with(file)) {
        app.on_key(code(KeyCode::Tab));
    }
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some(file) && line.diff.is_none()))
        .unwrap_or_else(|| panic!("{file} listed: {:?}", trimmed(app)));
}

/// In `dirty`: README "hello world" committed, then "hello there"; `both`
/// staged then changed again; `bin` binary and untracked; `new` untracked.
fn diff_fixture(sb: &Sandbox) {
    let d = sb.path("dev/dirty");
    write(&d, "README", "hello world\n");
    write(&d, "both", "one\n");
    commit(&d, "files");
    write(&d, "README", "hello there\n");
    write(&d, "both", "two\n");
    git(&d, &["add", "both"]);
    write(&d, "both", "three\n");
    std::fs::write(d.join("bin"), [0u8, 159, 146, 150]).unwrap();
    write(&d, "new", "a\nb\n");
}

#[test]
fn files_unfold_to_their_diff() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let at = t
        .iter()
        .position(|l| l == "unstaged changes")
        .expect("section");
    assert_eq!(
        t[at + 1..at + 4],
        ["@@ -1 +1 @@", "-hello world", "+hello there"]
    );
    // a file with both sides shows both, unstaged first
    to_file(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let both = t.iter().position(|l| l.ends_with("both")).unwrap();
    assert_eq!(
        t[both + 1..both + 9],
        [
            "unstaged changes",
            "@@ -1 +1 @@",
            "-two",
            "+three",
            "staged changes",
            "@@ -1 +1 @@",
            "-one",
            "+two"
        ]
    );
    // untracked: everything added; binary: said so
    to_file(&mut app, "new");
    app.on_key(code(KeyCode::Tab));
    to_file(&mut app, "bin");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let new = t.iter().position(|l| l.ends_with("new")).unwrap();
    assert_eq!(
        t[new + 1..new + 5],
        ["unstaged changes", "@@ -0,0 +1,2 @@", "+a", "+b"]
    );
    // nothing is staged for an untracked file (it's the last one listed)
    assert!(
        !t[new + 1..].contains(&"staged changes".to_string()),
        "{t:?}"
    );
    let bin = t.iter().position(|l| l.ends_with("bin")).unwrap();
    assert_eq!(t[bin + 1..bin + 3], ["unstaged changes", "binary file"]);
    // Tab on the file again folds it
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    assert!(!trimmed(&app).contains(&"-hello world".to_string()));
}

#[test]
fn hunks_fold() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let header = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.diff.as_ref().is_some_and(|d| d.hunk.is_some() && d.line.is_none())))
        .unwrap();
    app.cursor = header;
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert!(
        t.contains(&"@@ -1 +1 @@".to_string()) && !t.contains(&"-hello world".to_string()),
        "{t:?}"
    );
    // Tab on any line of a hunk folds it too; on the header again unfolds
    app.on_key(code(KeyCode::Tab));
    assert!(trimmed(&app).contains(&"-hello world".to_string()));
    app.cursor = header + 1;
    app.on_key(code(KeyCode::Tab));
    assert!(!trimmed(&app).contains(&"-hello world".to_string()));
    assert_eq!(app.cursor, header, "cursor moves to the folded header");
}

/// Whether every cell of `word` on the line containing `line_text` is drawn
/// reversed (refined), checking the first match.
fn word_reversed(app: &mut App, line_text: &str, word: &str) -> bool {
    let (theme, _) = Theme::build("terminal", &Default::default());
    let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons: argus_core::icons::UNICODE,
        tick: 0,
    };
    t.draw(|f| ui::draw(f, app, &view)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..40 {
        let row: String = (0..100).map(|x| buf[(x, y)].symbol().to_string()).collect();
        if let Some(lx) = row.find(line_text) {
            let wx = lx + line_text.find(word).unwrap();
            return (wx..wx + word.len()).all(|x| {
                buf[(x as u16, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            });
        }
    }
    panic!("{line_text} not drawn");
}

#[test]
fn changed_words_are_highlighted() {
    for (setting, there, hello) in [("all", true, false), ("off", false, false)] {
        let (_sb, mut app) = setup_with(&format!("[ui]\nrefine = '{setting}'"), diff_fixture);
        to_file(&mut app, "README");
        app.on_key(code(KeyCode::Tab));
        assert_eq!(
            word_reversed(&mut app, "+hello there", "there"),
            there,
            "{setting}"
        );
        assert_eq!(
            word_reversed(&mut app, "+hello there", "hello"),
            hello,
            "{setting}: unchanged word"
        );
        assert_eq!(
            word_reversed(&mut app, "-hello world", "world"),
            there,
            "{setting}: removed side"
        );
    }
    // selected: only the hunk under the cursor
    let (_sb, mut app) = setup_with("[ui]\nrefine = 'selected'", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    assert!(
        !word_reversed(&mut app, "+hello there", "there"),
        "cursor on the file line"
    );
    app.cursor += 3; // the + line
    assert!(word_reversed(&mut app, "+hello there", "there"));
}

#[test]
fn diffs_follow_status_updates() {
    let (sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    assert!(trimmed(&app).contains(&"+hello there".to_string()));
    let d = sb.path("dev/dirty");
    write(&d, "README", "hello again\n");
    let s = argus_core::status::status(&d, Default::default()).map_err(|e| e.to_string());
    app.on_msg(Msg::Status(d, Box::new(s)));
    let t = trimmed(&app);
    assert!(
        t.contains(&"+hello again".to_string()) && !t.contains(&"+hello there".to_string()),
        "{t:?}"
    );
}

/// The ops in `fx`'s jobs, in order.
fn job_ops(fx: &[Effect]) -> Vec<Op> {
    fx.iter()
        .flat_map(|e| match e {
            Effect::Jobs(j) => j.iter().map(|j| j.op.clone()).collect(),
            _ => vec![],
        })
        .collect()
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|p| p.to_string()).collect()
}

#[test]
fn stage_and_unstage_the_file_under_the_cursor() {
    let (_sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        let d = sb.path("dev/dirty");
        write(&d, "added", "x\n");
        git(&d, &["add", "added"]);
    });
    // not on a file: a hint, nothing run
    let fx = app.on_key(key('s'));
    assert!(fx.is_empty());
    assert!(
        app.message.as_ref().unwrap().0.contains("file"),
        "{:?}",
        app.message
    );
    to_file(&mut app, "README");
    let fx = app.on_key(key('s'));
    assert_eq!(
        job_ops(&fx),
        [Op::Add {
            paths: paths(&["README"])
        }]
    );
    app.running.clear();
    // on a diff line of a file: that file
    to_file(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    app.cursor += 3;
    let fx = app.on_key(key('u'));
    assert_eq!(
        job_ops(&fx),
        [Op::Restore {
            staged: true,
            worktree: false,
            from_head: false,
            paths: paths(&["both"])
        }]
    );
    app.running.clear();
    // a newly added file unstages with rm --cached
    to_file(&mut app, "added");
    let fx = app.on_key(key('u'));
    assert_eq!(
        job_ops(&fx),
        [Op::Remove {
            cached: true,
            paths: paths(&["added"])
        }]
    );
    app.running.clear();
    // nothing staged: nothing to unstage, said so
    to_file(&mut app, "new");
    let fx = app.on_key(key('u'));
    assert!(fx.is_empty());
    assert!(
        app.message.as_ref().unwrap().0.contains("nothing"),
        "{:?}",
        app.message
    );
}

#[test]
fn file_operations_apply_to_marked_files() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(key(' '));
    to_file(&mut app, "new");
    app.on_key(key(' '));
    app.act(Action::Top); // cursor away from both
    let fx = app.on_key(key('s'));
    assert_eq!(
        job_ops(&fx),
        [Op::Add {
            paths: paths(&["README", "new"])
        }]
    );
}

#[test]
fn discarding_always_asks() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    let fx = app.on_key(key('K'));
    assert!(fx.is_empty());
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(render(&mut app, 100, 30).contains("Discard changes to README? (y/n)"));
    let fx = app.on_key(key('n'));
    assert!(fx.is_empty() && matches!(app.mode, Mode::Normal));
    // several files: counted; y runs the discard
    to_file(&mut app, "README");
    app.on_key(key(' '));
    to_file(&mut app, "new");
    app.on_key(key(' '));
    app.on_key(key('K'));
    assert!(render(&mut app, 100, 30).contains("Discard changes to 2 files? (y/n)"));
    let fx = app.on_key(key('y'));
    assert_eq!(
        job_ops(&fx),
        [
            Op::Restore {
                staged: true,
                worktree: true,
                from_head: true,
                paths: paths(&["README"])
            },
            Op::Clean {
                paths: paths(&["new"])
            },
        ]
    );
}

#[test]
fn the_stash_list_says_how_many_are_ignored_and_stops_after_ten() {
    for (n, extra, first, more) in [
        (10, "[ignore]\nstashes = []", "stashes   10", None),
        (11, "", "stashes   11 (11 ignored)", Some("… 1 more")),
    ] {
        let (_sb, mut app) = setup_with(extra, |sb| {
            let r = sb.path("dev/clean");
            for i in 0..n {
                write(&r, "README", &format!("{i}\n"));
                git(&r, &["stash", "push", "-q", "-m", &format!("s{i}")]);
            }
        });
        cursor_on_detail(&mut app, "clean", "stashes");
        let texts = detail_texts(&app);
        let at = texts.iter().position(|t| t.starts_with("stashes")).unwrap();
        assert_eq!(texts[at], first, "{n}");
        let listed = texts[at + 1..]
            .iter()
            .take_while(|t| t.trim_start().starts_with("stash@"))
            .count();
        assert_eq!(listed, 10, "{n}");
        assert_eq!(texts.get(at + 11).map(|t| t.trim()), more, "{n}: {texts:?}");
    }
}

#[test]
fn explicit_fixes_run_for_every_branch_and_explain_blocked_ones() {
    let (sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        git(&f, &["switch", "-q", "-c", "b2"]);
        git(&f, &["push", "-q", "-u", "origin", "b2"]);
        commit(&f, "b2 work");
        git(&f, &["switch", "-q", "feat"]);
        commit(&f, "feat work");
        git(&f, &["switch", "-q", "main"]);
    });
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let pushes = |fx: Vec<Effect>| -> Vec<Op> {
        match fx.as_slice() {
            [Effect::Jobs(jobs)] => jobs.iter().map(|j| j.op.clone()).collect(),
            _ => vec![],
        }
    };
    let to = |b: &str| Op::PushTo {
        remote: "origin".into(),
        branch: b.into(),
    };
    assert_eq!(
        pushes(app.act(Action::PushBranches)),
        [to("b2"), to("feat")]
    );
    // rewritten after pushing: b2 is left out, feat still pushed
    let f = sb.path("dev/feature");
    git(&f, &["push", "-q", "origin", "b2"]);
    git(&f, &["switch", "-q", "b2"]);
    git(&f, &["reset", "-q", "--hard", "HEAD~1"]);
    commit(&f, "rewritten");
    git(&f, &["switch", "-q", "main"]);
    app.running.clear();
    app.ws.refresh(&f);
    assert_eq!(pushes(app.act(Action::PushBranches)), [to("feat")]);
    // only blocked ones: nothing runs, and the reason is shown
    git(&f, &["branch", "-q", "-D", "feat"]);
    app.running.clear();
    app.ws.refresh(&f);
    assert!(app.act(Action::PushBranches).is_empty());
    let (msg, is_error) = app.message.clone().unwrap();
    assert!(
        is_error && msg.contains("b2") && msg.contains("force"),
        "{msg}"
    );
}

#[test]
fn branch_and_remote_prompts_ask_then_run() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        git(&c, &["switch", "-q", "--detach", "main"]);
        commit(&c, "adrift");
        git(&c, &["remote", "remove", "origin"]);
        git(&sb.path("dev/dirty"), &["remote", "remove", "origin"]);
    });
    let go = |app: &mut App, name: &str| {
        app.act(Action::Top);
        for _ in 0..app.ws.entries.len() {
            if app.current().unwrap().path == sb.path(name) {
                return;
            }
            app.act(Action::Down);
        }
        panic!("{name} not listed");
    };
    go(&mut app, "dev/clean");
    let typed = |app: &mut App, a: Action, text: &str| {
        app.act(a);
        assert!(matches!(app.mode, Mode::Input { .. }));
        for c in text.chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter))
    };
    assert!(
        typed(&mut app, Action::CreateBranch, "").is_empty(),
        "empty aborts"
    );
    match typed(&mut app, Action::CreateBranch, "rescue").as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::CreateBranch {
                name: "rescue".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    app.running.clear();
    match typed(&mut app, Action::AddRemote, "git@example.com:me/clean.git").as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::AddRemote {
                name: "origin".into(),
                url: "git@example.com:me/clean.git".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    // inverse: not for repos where they don't apply, nor several remotes at once
    app.running.clear();
    go(&mut app, "dev/feature");
    let fx = app.act(Action::CreateBranch);
    assert!(fx.is_empty() && matches!(app.mode, Mode::Normal));
    app.marked = app.ws.entries.iter().map(|e| e.path.clone()).collect();
    assert!(app.act(Action::AddRemote).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .contains("one repo at a time")
    );
}

/// The ops a confirmation runs after `y`, or `None` if it didn't ask.
fn confirm(app: &mut App, a: Action, answer: char) -> Option<Vec<Op>> {
    app.act(a);
    if !matches!(app.mode, Mode::Confirm { .. }) {
        return None;
    }
    let fx = app.on_key(key(answer));
    assert!(matches!(app.mode, Mode::Normal));
    Some(match fx.as_slice() {
        [Effect::Jobs(jobs)] => jobs.iter().map(|j| j.op.clone()).collect(),
        _ => vec![],
    })
}

#[test]
fn destructive_fixes_ask_first_and_do_nothing_unless_told_yes() {
    let (sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        git(&d, &["stash", "push", "-q", "-m", "older"]);
        write(&d, "README", "changed again\n");
        git(&d, &["stash", "push", "-q", "-m", "newer"]);
        git(&d, &["branch", "done"]);
        // a merge stopped on a conflict in clean
        let c = sb.path("dev/clean");
        git(&c, &["switch", "-q", "-c", "o"]);
        write(&c, "README", "o");
        commit(&c, "o");
        git(&c, &["switch", "-q", "main"]);
        write(&c, "README", "m");
        commit(&c, "m");
        argus_core::testkit::git_may_fail(&c, &["merge", "o"]);
    });
    let go = |app: &mut App, name: &str| {
        app.act(Action::Top);
        for _ in 0..app.ws.entries.len() {
            if app.current().unwrap().path == sb.path(name) {
                return;
            }
            app.act(Action::Down);
        }
        panic!("{name} not listed");
    };
    use argus_core::status::InProgress;
    go(&mut app, "dev/clean");
    assert_eq!(confirm(&mut app, Action::AbortOperation, 'n'), Some(vec![]));
    assert_eq!(
        confirm(&mut app, Action::AbortOperation, 'y'),
        Some(vec![Op::Abort {
            what: InProgress::Merge
        }])
    );
    app.running.clear();
    go(&mut app, "dev/dirty");
    assert_eq!(
        confirm(&mut app, Action::AbortOperation, 'y'),
        None,
        "nothing in progress"
    );
    assert_eq!(
        confirm(&mut app, Action::DeleteMergedBranches, 'n'),
        Some(vec![])
    );
    assert_eq!(
        confirm(&mut app, Action::DeleteMergedBranches, 'y'),
        Some(vec![Op::DeleteBranch {
            branch: "done".into()
        }])
    );
    app.running.clear();
    // drop: only with the cursor on a stash line
    assert_eq!(confirm(&mut app, Action::DropStash, 'y'), None);
    app.act(Action::ToggleExpand);
    for _ in 0..app.rows.len() {
        if matches!(
            app.rows.get(app.cursor),
            Some(Row::Detail { line, .. }) if line.stash == Some(1)
        ) {
            break;
        }
        app.act(Action::Down);
    }
    let id = git(&sb.path("dev/dirty"), &["rev-parse", "stash@{1}"]);
    // questions say which selected repos have nothing to do
    app.marked = app.ws.entries.iter().map(|e| e.path.clone()).collect();
    app.act(Action::DeleteMergedBranches);
    match &app.mode {
        Mode::Confirm { question, .. } => assert_eq!(
            question,
            "Delete 1 merged branch(es): done? · 2 not applicable"
        ),
        _ => panic!("no question"),
    }
    app.on_key(key('n'));
    app.act(Action::AbortOperation);
    match &app.mode {
        Mode::Confirm { question, .. } => assert!(
            question.starts_with("Abort merge in acme/clean?")
                && question.ends_with("is lost. · 2 not applicable"),
            "{question}"
        ),
        _ => panic!("no question"),
    }
    app.on_key(key('n'));
    app.marked.clear();
    assert_eq!(confirm(&mut app, Action::DropStash, 'n'), Some(vec![]));
    assert_eq!(
        confirm(&mut app, Action::DropStash, 'y'),
        Some(vec![Op::StashDrop {
            index: 1,
            id: id.trim().to_string()
        }])
    );
}

#[test]
fn fix_actions_say_what_they_left_out() {
    let (sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        git(&f, &["switch", "-q", "-c", "b2"]);
        git(&f, &["push", "-q", "-u", "origin", "b2"]);
        commit(&f, "b2 work");
        git(&f, &["push", "-q"]);
        git(&f, &["reset", "-q", "--hard", "HEAD~1"]);
        commit(&f, "rewritten");
        git(&f, &["switch", "-q", "feat"]);
        commit(&f, "feat work");
        git(&f, &["switch", "-q", "main"]);
    });
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    app.marked = app.ws.entries.iter().map(|e| e.path.clone()).collect();
    assert!(!app.act(Action::PushBranches).is_empty());
    assert_eq!(
        msg(&app),
        "running 1 job(s)… · 2 not applicable · 1 blocked (acme/feature: origin/b2 has 1 commit(s) this branch doesn't (rebased?): pushing needs force)"
    );
    // a repo already running something is skipped, and said so
    assert!(app.act(Action::PushBranches).is_empty());
    assert!(
        msg(&app).starts_with("nothing started · 1 busy"),
        "{}",
        msg(&app)
    );
    // auto-fix: what had nothing to apply
    app.running.clear();
    git(&sb.path("dev/feature"), &["switch", "-q", "feat"]);
    let f = sb.path("dev/feature");
    app.ws.refresh(&f);
    assert!(!app.act(Action::AutoFix(Scope::Selected)).is_empty());
    assert_eq!(
        msg(&app),
        "auto-fix acme/feature: push… · 2 with nothing to apply"
    );
    assert!(app.act(Action::AutoFix(Scope::Selected)).is_empty());
    assert_eq!(msg(&app), "nothing started · 1 busy");
    // when it's finished
    app.on_msg(Msg::FixDone {
        path: f.clone(),
        applied: Ok(None),
    });
    assert_eq!(msg(&app), "auto-fix acme/feature: push: done");
}

#[test]
fn auto_fixing_several_repos_says_how_many_of_each() {
    let (sb, mut app) = setup_with("", |sb| {
        for n in ["clean", "dirty"] {
            let r = sb.path(&format!("dev/{n}"));
            git(&r, &["checkout", "-q", "--", "."]);
            git(&r, &["switch", "-q", "-c", "topic"]);
            git(&r, &["push", "-q", "-u", "origin", "topic"]);
        }
    });
    app.marked = app.ws.entries.iter().map(|e| e.path.clone()).collect();
    assert!(!app.act(Action::AutoFix(Scope::Selected)).is_empty());
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    assert_eq!(msg(&app), "auto-fixing 3 repo(s): switch-default (3)…");
    // "done" once all three are
    for (i, n) in ["clean", "dirty", "feature"].iter().enumerate() {
        app.on_msg(Msg::FixDone {
            path: sb.path(&format!("dev/{n}")),
            applied: Ok(None),
        });
        assert_eq!(msg(&app).ends_with(": done"), i == 2, "{}", msg(&app));
    }
}

#[test]
fn explanations_when_nothing_applies() {
    let (sb, mut app) = setup("");
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    app.act(Action::Bottom);
    assert_eq!(current(&app), "acme/clean");
    assert!(app.act(Action::AutoFix(Scope::Selected)).is_empty());
    assert_eq!(
        msg(&app),
        "no auto-fixes (push, push-branches, switch-default, reattach) apply"
    );
    assert!(app.act(Action::AbortOperation).is_empty());
    assert_eq!(msg(&app), "nothing in progress");
    assert!(app.act(Action::DeleteMergedBranches).is_empty());
    assert_eq!(msg(&app), "no merged branches to delete");
    assert!(app.act(Action::AddRemote).is_empty());
    assert_eq!(msg(&app), "the selection has remotes");
    assert!(matches!(app.mode, Mode::Normal));
    // pulling a branch whose upstream is gone only fetches
    let f = sb.path("dev/feature");
    git(&f, &["push", "-q", "origin", "--delete", "feat"]);
    app.ws.refresh(&f);
    while current(&app) != "acme/feature" {
        app.act(Action::Up);
    }
    match app.act(Action::Pull(Scope::Selected)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j[0].op, Op::Fetch),
        other => panic!("{other:?}"),
    }
}

#[test]
fn prompts_say_what_they_ask_for() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        git(&c, &["switch", "-q", "--detach", "main"]);
        commit(&c, "adrift");
    });
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.act(Action::CreateBranch);
    for c in "res".chars() {
        app.on_key(key(c));
    }
    let screen = render(&mut app, 80, 10);
    assert!(
        screen.contains("new branch at HEAD (1 repo(s)): res"),
        "{screen}"
    );
}

#[test]
fn the_fix_menu_says_what_auto_fix_would_do() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    app.on_key(key('x'));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("x: auto-fix acme/feature: switch main"),
        "{screen}"
    );
    assert!(
        screen.contains("a: auto-fix acme/feature: switch main"),
        "{screen}"
    );
    app.on_key(key('q'));
    // inverse: nothing to do for the selection
    app.act(Action::Bottom);
    app.on_key(key('x'));
    let screen = render(&mut app, 100, 20);
    assert!(screen.contains("x: nothing to auto-fix"), "{screen}");
    // other menus have no notes
    app.on_key(key('q'));
    app.on_key(key('P'));
    assert!(!render(&mut app, 100, 20).contains("auto-fix"));
}

/// The open menu's items as `key label`.
fn menu_items(app: &App) -> Vec<String> {
    match &app.mode {
        Mode::Menu { menu, .. } => menu
            .items
            .iter()
            .map(|i| format!("{} {}", i.key, i.label))
            .collect(),
        _ => panic!("no menu open"),
    }
}

#[test]
fn the_ignore_menu_steps_an_issue_through_its_states() {
    let (_sb, mut app) = setup("");
    cursor_on_detail(&mut app, "feature", "issue     not on default branch");
    assert!(app.act(Action::IgnoreIssues).is_empty());
    match &app.mode {
        Mode::Menu { menu, cursor, .. } => {
            assert_eq!(menu.title, "Ignore issues · acme/feature");
            assert_eq!(*cursor, 0, "on the issue under the cursor");
        }
        _ => panic!("no menu"),
    }
    let nd = |app: &App| app.ws.config.ignore.non_default_branch.clone();
    assert_eq!(
        menu_items(&app),
        [
            "1 · not on default branch (main)",
            "a all: ignore 1 remaining issue(s)"
        ]
    );
    // counted, pinned (the default), ignored everywhere, counted; the menu stays
    let fx = app.on_key(key('1'));
    assert!(matches!(fx.as_slice(), [Effect::SaveMarks(_)]), "{fx:?}");
    assert_eq!(nd(&app), ["github.com/acme/feature@feat"]);
    assert_eq!(
        menu_items(&app)[0],
        "1 ○ not on default branch (main) [ignored@feat]"
    );
    assert_eq!(menu_items(&app)[1..], ["n none: stop ignoring 1 issue(s)"]);
    app.on_key(key('1'));
    assert_eq!(nd(&app), ["github.com/acme/feature"]);
    assert_eq!(
        menu_items(&app)[0],
        "1 ● not on default branch (main) [ignored]"
    );
    app.on_key(key('1'));
    assert!(nd(&app).is_empty());
    assert_eq!(
        menu_items(&app),
        [
            "1 · not on default branch (main)",
            "a all: ignore 1 remaining issue(s)"
        ]
    );
    app.on_key(ctrl('g'));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn all_ignores_the_remaining_issues_and_none_stops_ignoring_them() {
    let (sb, mut app) = setup("");
    let f = sb.path("dev/feature");
    write(&f, "README", "wip\n");
    git(&f, &["stash", "push", "-q", "-m", "wip"]);
    app.ws.refresh(&f);
    app.rebuild();
    // from the stash's issue line: the menu opens on it
    cursor_on_detail(&mut app, "feature", "issue     1 stash");
    app.act(Action::IgnoreIssues);
    assert!(matches!(app.mode, Mode::Menu { cursor: 1, .. }));
    // the stash is ignored by the default patterns
    assert_eq!(
        menu_items(&app),
        [
            "1 · not on default branch (main)",
            "2 ● 1 stash(es) [ignored]",
            "a all: ignore 1 remaining issue(s)",
            "n none: stop ignoring 1 issue(s)"
        ]
    );
    let stashes = app.ws.config.ignore.stashes.clone();
    // each its default way; the ignored one left as it was
    app.on_key(key('a'));
    assert_eq!(
        app.ws.config.ignore.non_default_branch,
        ["github.com/acme/feature@feat"]
    );
    assert_eq!(app.ws.config.ignore.stashes, stashes);
    assert_eq!(menu_items(&app)[2..], ["n none: stop ignoring 2 issue(s)"]);
    // nothing remaining: a isn't there, and changes nothing
    assert!(app.on_key(key('a')).is_empty());
    assert_eq!(app.ws.config.ignore.stashes, stashes);
    let fx = app.on_key(key('n'));
    assert!(matches!(fx.as_slice(), [Effect::SaveMarks(_)]), "{fx:?}");
    assert!(app.ws.config.ignore.non_default_branch.is_empty());
    assert_eq!(
        app.ws.config.ignore.stashes[stashes.len()..],
        ["!github.com/acme/feature"]
    );
    assert_eq!(
        menu_items(&app),
        [
            "1 · not on default branch (main)",
            "2 · 1 stash(es)",
            "a all: ignore 2 remaining issue(s)"
        ]
    );
    assert!(app.on_key(key('n')).is_empty());
}

#[test]
fn none_leaves_issues_that_arent_ignored() {
    // the stash issue counts, though the patterns ignore one of the two
    let (_sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/feature");
        write(&r, "README", "mine\n");
        git(&r, &["stash", "push", "-q", "-m", "mine"]);
        write(&r, "README", "fix's\n");
        git(&r, &["stash", "push", "-q", "-m", "argus 2026-01-02 09:30"]);
    });
    cursor_on_detail(&mut app, "feature", "issue     not on default branch");
    app.act(Action::IgnoreIssues);
    let stashes = app.ws.config.ignore.stashes.clone();
    app.on_key(key('1'));
    // in the same order as before
    assert_eq!(
        menu_items(&app)[..2],
        [
            "1 ○ not on default branch (main) [ignored@feat]",
            "2 · 2 stash(es), 1 ignored by patterns"
        ]
    );
    app.on_key(key('n'));
    assert!(app.ws.config.ignore.non_default_branch.is_empty());
    assert_eq!(app.ws.config.ignore.stashes, stashes);
}

#[test]
fn stashes_step_through_all_pinned_to_their_messages_and_counted() {
    let (_sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        write(&r, "README", "mine\n");
        git(&r, &["stash", "push", "-q", "-m", "mine"]);
        write(&r, "README", "fix's\n");
        git(&r, &["stash", "push", "-q", "-m", "argus 2026-01-02 09:30"]);
    });
    let defaults = argus_core::Config::default().ignore.stashes;
    let own = |app: &App| app.ws.config.ignore.stashes[defaults.len()..].to_vec();
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.act(Action::IgnoreIssues);
    // yours is ignored by the patterns, the fix's isn't
    assert_eq!(
        menu_items(&app)[0],
        "1 · 2 stash(es), 1 ignored by patterns"
    );
    app.on_key(key('1'));
    assert_eq!(own(&app), ["github.com/acme/clean"]);
    assert_eq!(menu_items(&app)[0], "1 ● 2 stash(es) [ignored]");
    app.on_key(key('1'));
    assert_eq!(
        own(&app),
        [
            "!github.com/acme/clean",
            "github.com/acme/clean@argus 2026-01-02 09:30",
            "github.com/acme/clean@mine"
        ]
    );
    assert_eq!(menu_items(&app)[0], "1 ○ 2 stash(es) [ignored@2 messages]");
    app.on_key(key('1'));
    assert_eq!(own(&app), ["!github.com/acme/clean"]);
    assert_eq!(menu_items(&app)[0], "1 · 2 stash(es)");
}

#[test]
fn a_repo_with_nothing_to_ignore_says_so() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/dirty" {
        app.act(Action::Down);
    }
    assert!(app.act(Action::IgnoreIssues).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "no issue of this repo can be ignored"
    );
}

#[test]
fn toggles_say_what_they_will_do() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let label = |app: &mut App, menu: char, key: char| {
        app.on_key(self::key(menu));
        let l = menu_items(app)
            .into_iter()
            .find(|l| l.starts_with(&format!("{key} ")))
            .unwrap();
        app.on_key(ctrl('g'));
        l
    };
    assert_eq!(label(&mut app, 'm', 'f'), "f favourite");
    assert_eq!(
        label(&mut app, 'm', 'I'),
        "I ignore repo: hidden, no pulls or bulk actions"
    );
    assert_eq!(label(&mut app, 'm', 'i'), "i ignore issues…");
    app.act(Action::ToggleFavourite);
    assert_eq!(label(&mut app, 'm', 'f'), "f unfavourite");
    assert_eq!(
        label(&mut app, 'v', 'a'),
        "a show only repos needing attention"
    );
    assert_eq!(
        label(&mut app, 'v', 'h'),
        "h show ignored and snoozed repos"
    );
    assert_eq!(label(&mut app, 'v', 'l'), "l show process log");
    app.act(Action::ToggleLog);
    assert_eq!(label(&mut app, 'v', 'l'), "l hide process log");
}

#[test]
fn untracked_files_go_into_the_gitignore_chosen() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        // tracked, so sub/'s untracked files are listed one by one
        write(&c, "sub/keep.txt", "x\n");
        git(&c, &["add", "sub/keep.txt"]);
        commit(&c, "keep");
        write(&c, "build/out.o", "x\n");
        write(&c, "notes.txt", "x\n");
    });
    // a tracked file: nothing to ignore
    cursor_on_detail(&mut app, "dirty", "     M README");
    assert!(app.act(Action::GitignoreMenu).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .starts_with("no untracked file selected")
    );
    // an untracked directory under the cursor
    cursor_on_detail(&mut app, "clean", "    ?? build/");
    app.on_key(key('i'));
    match &app.mode {
        Mode::Menu { menu, .. } => assert_eq!(menu.title, "Gitignore · 1 untracked file(s)"),
        _ => panic!("no menu"),
    }
    assert_eq!(
        menu_items(&app),
        [
            "t add /build/ to .gitignore",
            "d add /build/ to .gitignore",
            "p add /build/ to .git/info/exclude (this clone only)",
            "g add /build/ to the global excludes file (every repo)",
            "a anchored: only this path, not the name anywhere",
        ]
    );
    // unanchored: the rules shown change, the menu stays
    app.on_key(key('a'));
    assert_eq!(menu_items(&app)[0], "t add build/ to .gitignore");
    match app.on_key(key('t')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Ignore {
                file: argus_core::gitignore::IgnoreFile::TopLevel,
                paths: vec!["build/".into()],
                anchored: false
            }
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(app.mode, Mode::Normal));
    // marked files, from a subdirectory: its own .gitignore is relative
    let c = sb.path("dev/clean");
    write(&c, "sub/a.log", "x\n");
    write(&c, "sub/b.log", "x\n");
    app.running.clear();
    app.ws.refresh(&c);
    app.marked_files.insert(
        c.clone(),
        ["sub/a.log".to_string(), "sub/b.log".to_string()].into(),
    );
    app.act(Action::GitignoreMenu);
    assert_eq!(
        menu_items(&app)[1],
        "d add /a.log, /b.log to sub/.gitignore"
    );
    assert_eq!(
        menu_items(&app)[0],
        "t add /sub/a.log, /sub/b.log to .gitignore"
    );
}

/// The details lines after the one starting `after`, while they're
/// indented deeper than `depth` spaces, trimmed.
fn below(app: &App, after: &str, depth: usize) -> Vec<String> {
    let texts = detail_texts(app);
    let at = texts
        .iter()
        .position(|t| t.starts_with(after))
        .unwrap_or_else(|| panic!("no {after:?}: {texts:?}"));
    texts[at + 1..]
        .iter()
        .take_while(|t| t.len() - t.trim_start().len() > depth)
        .map(|t| t.trim().to_string())
        .collect()
}

#[test]
fn unpushed_commits_unfold_and_so_does_each_commit() {
    let (sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        write(&f, "new.txt", "x\n");
        git(&f, &["add", "new.txt"]);
        git(
            &f,
            &[
                "commit",
                "-q",
                "-m",
                "Add new.txt\n\nBecause it was needed.",
            ],
        );
    });
    let f = sb.path("dev/feature");
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit(s)");
    app.act(Action::ToggleExpand);
    let id = git(&f, &["rev-parse", "HEAD"]);
    let short = &id[..7];
    let commits = below(&app, "issue     1 unpushed commit(s)", 0);
    assert_eq!(commits.len(), 1, "{commits:?}");
    assert!(
        commits[0].starts_with(&format!("{short} Add new.txt  ")),
        "{commits:?}"
    );
    // the cursor stays put; Tab on the commit shows it in full
    cursor_on_detail(&mut app, "feature", &format!("      {short}"));
    app.act(Action::ToggleExpand);
    let full = below(&app, &format!("      {short}"), 6);
    assert_eq!(full[0], format!("commit {}", id.trim()));
    assert!(full[1].starts_with("author "), "{full:?}");
    assert_eq!(
        &full[2..6],
        ["Add new.txt", "", "Because it was needed.", "files (1)"].map(|s| s.to_string())
    );
    assert_eq!(full[6], "A new.txt");
    // a file it changed unfolds into its diff
    cursor_on_detail(&mut app, "feature", "          A new.txt");
    app.act(Action::ToggleExpand);
    assert_eq!(
        below(&app, "          A new.txt", 10),
        ["@@ -0,0 +1 @@", "+x"]
    );
    // stays unfolded across a refresh, cursor on the commit
    app.ws.refresh(&f);
    app.on_msg(Msg::Status(
        f.clone(),
        Box::new(Ok(app.ws.get(&f).unwrap().status().unwrap().clone())),
    ));
    assert!(detail_texts(&app).iter().any(|t| t.trim() == "files (1)"));
    // and folds again
    cursor_on_detail(&mut app, "feature", &format!("      {short}"));
    app.act(Action::ToggleExpand);
    assert!(!detail_texts(&app).iter().any(|t| t.trim() == "files (1)"));
}

#[test]
fn other_branches_unfold_into_their_commits() {
    let (_sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        git(&f, &["switch", "-q", "-c", "local"]);
        commit(&f, "only here");
        git(&f, &["switch", "-q", "main"]);
    });
    cursor_on_detail(&mut app, "feature", "issue     1 other local branch(es)");
    app.act(Action::ToggleExpand);
    let branches = below(&app, "issue     1 other local branch(es)", 0);
    assert_eq!(branches, ["local  1 on no remote"]);
    cursor_on_detail(&mut app, "feature", "      local");
    app.act(Action::ToggleExpand);
    let commits = below(&app, "      local", 6);
    assert!(
        commits.len() == 1 && commits[0].contains(" only here  "),
        "{commits:?}"
    );
    // the branch's own line, in the branches list, unfolds the same
    assert!(
        detail_texts(&app)
            .iter()
            .any(|t| t.starts_with("    local") && t.ends_with("1 on no remote"))
    );
}

#[test]
fn an_untracked_directory_unfolds_into_files_that_can_be_acted_on() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        write(&c, "build/a.o", "a\n");
        write(&c, "build/sub/b.o", "b\n");
    });
    cursor_on_detail(&mut app, "clean", "    ?? build/");
    app.act(Action::ToggleExpand);
    assert_eq!(
        below(&app, "    ?? build/", 4),
        ["?? build/a.o", "?? build/sub/b.o"]
    );
    // a file in it: staged, gitignored, diffed like any untracked file
    cursor_on_detail(&mut app, "clean", "      ?? build/a.o");
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Add {
                paths: vec!["build/a.o".into()]
            }
        ),
        other => panic!("{other:?}"),
    }
    app.running.clear();
    app.act(Action::GitignoreMenu);
    assert_eq!(menu_items(&app)[0], "t add /build/a.o to .gitignore");
    app.on_key(ctrl('g'));
    cursor_on_detail(&mut app, "clean", "      ?? build/a.o");
    app.act(Action::ToggleExpand);
    assert!(
        detail_texts(&app).iter().any(|t| t.trim() == "+a"),
        "{:?}",
        detail_texts(&app)
    );
    let _ = sb;
}

#[test]
fn a_stash_unfolds_into_what_it_holds() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        write(&c, "README", "stashed\n");
        git(&c, &["stash", "push", "-q", "-m", "keep this"]);
    });
    cursor_on_detail(&mut app, "clean", "          stash@{0} keep this");
    app.act(Action::ToggleExpand);
    let held = below(&app, "          stash@{0} keep this", 10);
    assert!(held.iter().any(|l| l == "On main: keep this"), "{held:?}");
    assert!(held.iter().any(|l| l == "M README"), "{held:?}");
}

#[test]
fn commits_behind_and_on_a_detached_head_unfold() {
    let (_sb, mut app) = setup_with("", |sb| {
        sb.advance_remote("acme", "clean", "theirs.txt");
        git(&sb.path("dev/clean"), &["fetch", "-q"]);
        let d = sb.path("dev/dirty");
        git(&d, &["switch", "-q", "--detach"]);
        commit(&d, "adrift");
    });
    cursor_on_detail(&mut app, "clean", "issue     1 commit(s) behind upstream");
    app.act(Action::ToggleExpand);
    let incoming = below(&app, "issue     1 commit(s) behind upstream", 0);
    assert!(
        incoming.len() == 1 && incoming[0].contains(" remote theirs.txt  "),
        "{incoming:?}"
    );
    cursor_on_detail(&mut app, "dirty", "issue     1 commit(s) on detached HEAD");
    app.act(Action::ToggleExpand);
    let adrift = below(&app, "issue     1 commit(s) on detached HEAD", 0);
    assert!(
        adrift.len() == 1 && adrift[0].contains(" adrift  "),
        "{adrift:?}"
    );
}

/// The text of the row under the cursor, trimmed.
fn here(app: &App) -> String {
    match &app.rows[app.cursor] {
        Row::Repo(i) => app.ws.entries[*i].display_name(),
        Row::Detail { line, .. } => line
            .parts
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<String>()
            .trim()
            .to_string(),
    }
}

#[test]
fn n_and_p_step_through_sections_skipping_diff_and_message_lines() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let mut steps = vec![];
    for k in ['n', 'n', 'n', 'p', 'p'] {
        app.on_key(key(k));
        steps.push(here(&app));
    }
    assert_eq!(
        steps,
        [
            "unstaged changes",
            "@@ -1 +1 @@",
            "?? bin",
            "@@ -1 +1 @@",
            "unstaged changes"
        ]
    );
    // a commit: its message is skipped, its file's hunk isn't
    cursor_on_detail(&mut app, "dirty", "issue     1 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('n'));
    assert!(here(&app).contains(" files "), "{}", here(&app));
    app.on_key(code(KeyCode::Tab));
    let mut steps = vec![];
    for _ in 0..4 {
        app.on_key(key('n'));
        steps.push(here(&app));
    }
    assert!(steps[0].starts_with("commit "), "{steps:?}");
    assert!(steps[1].starts_with("author "), "{steps:?}");
    assert_eq!(steps[2..], ["files (2)", "M README"], "{steps:?}");
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('n'));
    assert_eq!(here(&app), "@@ -1 +1 @@");
    app.on_key(key('n'));
    assert_eq!(here(&app), "A both");
    // M-n / M-p: the next and previous repo
    app.on_key(alt('n'));
    assert_eq!(here(&app), "acme/feature");
    app.on_key(alt('p'));
    assert_eq!(here(&app), "acme/dirty");
}

#[test]
fn details_list_fixes_that_need_input_with_their_keys() {
    let (sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        // merged, no upstream: to delete
        for b in ["old1", "old2"] {
            git(&sb.path("dev/dirty"), &["branch", b, "HEAD~1"]);
        }
    });
    cursor_on_detail(&mut app, "dirty", "issue");
    let t = trimmed(&app);
    let fix = |name: &str| {
        t.iter()
            .find(|l| l.starts_with(&format!("fix       {name}…")))
            .unwrap_or_else(|| panic!("{name}: {t:?}"))
            .clone()
    };
    assert_eq!(
        fix("gitignore"),
        "fix       gitignore…  2 untracked path(s)  (i)"
    );
    assert!(fix("commit").ends_with("  (c a)"), "{}", fix("commit"));
    assert_eq!(
        fix("delete-branches"),
        "fix       delete-branches…  2 merged branches: old1, old2  (b x)"
    );
    // one: its own reason
    let d = sb.path("dev/dirty");
    git(&d, &["branch", "-q", "-D", "old2"]);
    app.ws.refresh(&d);
    app.rebuild();
    let line = trimmed(&app)
        .into_iter()
        .find(|l| l.starts_with("fix       delete-branches…"))
        .unwrap();
    assert_eq!(
        line,
        "fix       delete-branches…  old1 is merged into main  (b x)"
    );
}

#[test]
fn gitignore_on_a_repo_takes_all_its_untracked_files() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    while current(&app) != "acme/dirty" {
        app.act(Action::Down);
    }
    app.on_key(key('i'));
    assert_eq!(menu_items(&app)[0], "t add /bin, /new to .gitignore");
    // nothing untracked
    app.on_key(ctrl('g'));
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    assert!(app.act(Action::GitignoreMenu).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "nothing untracked to ignore"
    );
}
