//! TUI behaviour: keys -> state + effects, and rendering via TestBackend.

use std::path::PathBuf;

use argus_core::config::Config;
use argus_core::ops::{Op, ResetMode};
use argus_core::testkit::{Sandbox, commit, git, write};
use argus_core::{State, Workspace};
use argus_tui::app::{Action, App, Effect, Mode, Msg, Prompt, Row, Scope, StashKind};
use argus_tui::details::Seg;
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
    if !extra.contains("forge_info") {
        // No forge lookups unless a test is about them.
        config.ui.forge_info = false;
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
    t.draw(|f| drop(ui::draw(f, app, &view))).unwrap();
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

/// `/` matches fuzzily, as the branch picker does: the letters in order,
/// not necessarily together.
#[test]
fn the_filter_matches_fuzzily() {
    let (_sb, mut app) = setup("");
    app.on_key(key('/'));
    for c in "acln".chars() {
        app.on_key(key(c));
    }
    assert_eq!(names(&app), vec!["acme/clean"]);
    app.on_key(ctrl('g'));
    // inverse: letters out of order don't match
    app.on_key(key('/'));
    for c in "naelc".chars() {
        app.on_key(key(c));
    }
    assert!(names(&app).is_empty(), "{:?}", names(&app));
}

/// `#tag` in `/` filters by tags, `!#tag` by not having one, `|`
/// either; with text, both must match.
#[test]
fn the_filter_takes_tags() {
    let (_sb, mut app) = setup(
        "[tags.work]\nrepos = ['acme/clean', 'acme/dirty']\n[tags.home]\nrepos = ['acme/feature']\n",
    );
    let filter = |app: &mut App, q: &str| {
        app.on_key(key('/'));
        for c in q.chars() {
            app.on_key(key(c));
        }
        let n = names(app);
        app.on_key(ctrl('g'));
        n
    };
    assert_eq!(filter(&mut app, "#work"), ["acme/dirty", "acme/clean"]);
    assert_eq!(filter(&mut app, "!#work"), ["acme/feature"]);
    assert_eq!(filter(&mut app, "#work | #home").len(), 3);
    assert_eq!(filter(&mut app, "#work cln"), ["acme/clean"]);
    assert!(filter(&mut app, "#nope").is_empty());
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
    app.on_key(key('t'));
    let before = app.ws.config.clone();
    let fx = app.on_key(key('f'));
    // saved as a change from the config before
    assert_eq!(fx, vec![Effect::SaveTags(Box::new(before))]);
    assert_eq!(
        app.ws.config.tags["favourite"].repos,
        ["github.com/acme/feature"]
    );
    assert_eq!(names(&app), ["acme/feature", "acme/dirty", "acme/clean"]);
    assert_eq!(current(&app), "acme/feature", "cursor follows the repo");
    // inverse: a clean favourite stays below unclean repos
    app.act(Action::Bottom);
    app.on_key(key('t'));
    app.on_key(key('f'));
    assert_eq!(names(&app)[2], "acme/clean");
    app.on_key(key('t'));
    app.on_key(key('f'));
    assert_eq!(
        app.ws.config.tags["favourite"].repos,
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
    app.on_key(key('A'));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("commit all changes (1 repo(s)):"),
        "c A: {screen}"
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
    // dirty has only unstaged changes: no c c
    app.on_key(key('c'));
    assert_eq!(
        menu_items(&app),
        ["A commit all changes…", "a amend the last commit…"]
    );
    app.on_key(ctrl('g'));
    let d = sb.path("dev/dirty");
    git(&d, &["add", "README"]);
    app.ws.refresh(&d);
    app.rebuild();
    app.on_key(key('c'));
    assert_eq!(
        menu_items(&app),
        [
            "c commit staged changes…",
            "A commit all changes…",
            "a amend the last commit…",
            "f fixup a commit with what's staged…",
            "F instant fixup: fold what's staged into a commit…",
            "s squash: a squash! commit of a commit, adding a message…",
            "S instant squash: fold what's staged and a message into a commit…"
        ]
    );
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
    let (_sb, mut app) = setup("tags.ignore.repos = ['acme/feature']");
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3, "shown");
    assert!(app.act(Action::AutoFix(Scope::All)).is_empty());
}

#[test]
fn auto_fix_leaves_an_ignored_state_alone() {
    // feature's only fix switches it to main; its branch is ignored
    let (_sb, mut app) = setup("[tags.ignore.issues]\nnon_default_branch = ['acme/feature@feat']");
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
    app.on_key(key('t'));
    let out = render(&mut app, 80, 16);
    assert!(
        out.contains("Tag · 1 repo") && out.contains("f  tag favourite"),
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
        Action::ToggleLabel(0),
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
    // a snoozed (hidden) one: counted, and said
    let key = app.ws.entries[0].key.clone();
    app.ws.state.snoozed.insert(
        key,
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    let entries = app.ws.entries.clone();
    app.on_msg(Msg::Scanned {
        entries,
        truncated: vec![],
    });
    let (msg, _) = app.message.clone().unwrap();
    assert!(msg.starts_with("3 repos, 1 hidden · "), "{msg}");
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
            .contains("1 of 2 repos needs attention · a: show all"),
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
            .contains("2 repos · 1 needs attention · a: attention only"),
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
            .ends_with("1 repo · 1 needs attention · a: attention only · H: show 1 hidden"),
        "{}",
        header(&mut app)
    );
    app.act(Action::ToggleHidden);
    assert!(
        header(&mut app).contains(
            "2 repos · 1 needs attention · a: attention only · showing hidden · H: hide them"
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
    assert_eq!(texts[at + 1], "git tags  release, v1.0", "{texts:?}");
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
    // tall enough for all of the help
    let out = render(&mut app, 100, 160);
    for (icon, meaning) in [
        ("◆", "favourite"),
        ("±", "unstaged changes"),
        ("?", "untracked files"),
        ("@", "detached HEAD (tag or commit)"),
        ("$", "stashes"),
        ("≠", "conflicts"),
        ("⠋", "job running"),
        ("▌", "marked, in a repo's details"),
        ("│", "partly marked"),
        ("•", "a repo marked for a batch action"),
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
    d.apply_tags(&mut ws);
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
    assert!(main.contains("#demo/"), "marked repo: {main}");
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
    assert!(main.contains('•') && main.contains('↑'), "{main}");
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
    d.apply_tags(&mut ws);
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
    assert!(out.contains("git tags  release-1.0, v1.0"), "{out}");
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
            term.draw(|f| drop(ui::draw(f, &mut app, &view))).unwrap();
        }
    }
}

#[test]
fn clearing_tags_of_several_repos_asks_first() {
    use argus_core::rules::Tag;
    let (_sb, mut app) = setup("");
    let all: Vec<usize> = (0..app.ws.entries.len()).collect();
    app.ws.set_tag(&all, Tag::Label(0), true).unwrap();
    let favourites = |app: &App| app.ws.config.tags["favourite"].repos.len();
    for e in &app.ws.entries {
        app.marked.insert(e.path.clone());
    }
    let fx = app.act(Action::ClearTags);
    assert!(fx.is_empty(), "nothing saved before an answer: {fx:?}");
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(render(&mut app, 100, 8).contains("Clear tags of 3 repos? (y/n)"));
    // anything but y cancels, changing nothing
    let fx = app.on_key(key('n'));
    assert!(fx.is_empty() && matches!(app.mode, Mode::Normal));
    assert_eq!(favourites(&app), 3);
    assert!(app.message.as_ref().unwrap().0.contains("cancelled"));
    app.act(Action::ClearTags);
    app.on_key(code(KeyCode::Esc));
    assert_eq!(favourites(&app), 3);
    // y clears them, saving config and state
    let before = app.ws.config.clone();
    app.act(Action::ClearTags);
    let fx = app.on_key(key('y'));
    assert!(fx.contains(&Effect::SaveTags(Box::new(before))) && fx.contains(&Effect::SaveState));
    assert_eq!(favourites(&app), 0);
    // one repo losing tags: no question (a snooze doesn't count)
    app.ws.set_tag(&[0], Tag::Ignore, true).unwrap();
    let k1 = app.ws.entries[1].key.clone();
    app.ws.state.snoozed.insert(
        k1,
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    let fx = app.act(Action::ClearTags);
    assert!(fx.contains(&Effect::SaveState) && matches!(app.mode, Mode::Normal));
    assert!(app.ws.config.tags["ignore"].repos.is_empty() && app.ws.state.snoozed.is_empty());
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
    let fx = app.on_key(key('g'));
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
    let fx = app.on_key(key('g'));
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
    // the message switch: whichever stash is chosen asks for one first
    app.on_key(key('z'));
    app.on_key(key('M'));
    assert!(
        matches!(app.mode, Mode::Menu { .. }),
        "a switch: still open"
    );
    app.on_key(key('z'));
    assert!(matches!(app.mode, Mode::Input { .. }));
    for c in "wip".chars() {
        app.on_key(key(c));
    }
    let fx = app.on_key(code(KeyCode::Enter));
    let jobs = stash_jobs(&app, &fx);
    assert!(
        matches!(&jobs[..], [(n, Op::Stash { message, staged: false, .. })] if n == "acme/dirty" && message == "wip"),
        "{jobs:?}"
    );
    // a staged stash too; no message given, the usual one
    app.running.clear();
    assert!(app.act(Action::Stash(StashKind::Staged)).is_empty());
    assert!(matches!(app.mode, Mode::Input { .. }));
    let fx = app.on_key(code(KeyCode::Enter));
    // (nothing staged in dirty: said, not stashed)
    assert!(fx.is_empty() && !matches!(app.mode, Mode::Input { .. }));
    // inverse: switched off again, stashed at once
    app.on_key(key('z'));
    app.on_key(key('M'));
    let fx = app.on_key(key('z'));
    let jobs = stash_jobs(&app, &fx);
    assert!(
        matches!(&jobs[..], [(_, Op::Stash { message, .. })] if message.starts_with("argus ")),
        "{jobs:?}"
    );
}

/// From config: `[stash] message = true` starts with the switch on.
#[test]
fn the_message_switch_starts_as_config_says() {
    let (_sb, mut app) = setup("[stash]\nmessage = true\n");
    app.on_key(key('z'));
    app.on_key(key('z'));
    assert!(matches!(app.mode, Mode::Input { .. }));
}

/// With one stash there's nothing to pick: pop pops it, drop asks only
/// whether to.
#[test]
fn pop_and_drop_take_a_repos_only_stash_without_asking_which() {
    let (sb, mut app) = setup("");
    let d = sb.path("dev/dirty");
    git(&d, &["stash", "push", "-q", "-m", "only"]);
    status_arrives(&mut app, &d);
    let id = app.ws.get(&d).unwrap().status().unwrap().stash_ids[0].clone();
    cursor_on_detail(&mut app, "dirty", "path");
    app.on_key(key('z'));
    match &app.mode {
        Mode::Menu { menu, .. } => {
            let label = |k: char| {
                menu.items
                    .iter()
                    .find(|i| i.key == k)
                    .unwrap()
                    .label
                    .clone()
            };
            assert_eq!((label('p').as_str(), label('d').as_str()), ("pop", "drop…"));
        }
        _ => panic!("no menu"),
    }
    match app.on_key(key('p')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::StashPopAt {
                index: 0,
                id: id.clone()
            }
        ),
        other => panic!("{other:?}"),
    }
    app.running.clear();
    app.on_key(key('z'));
    assert!(app.on_key(key('d')).is_empty());
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.starts_with("Drop stash@{0} (only)")),
        "asked whether, not which"
    );
    match app.on_key(key('y')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j[0].op, Op::StashDrop { index: 0, id }),
        other => panic!("{other:?}"),
    }
}

/// Pop and drop take the stash under the cursor, else ask which (the
/// newest first); drop asks before dropping either way.
#[test]
fn pop_and_drop_take_the_cursors_stash_or_ask_which() {
    let (sb, mut app) = setup("");
    let d = sb.path("dev/dirty");
    git(&d, &["stash", "push", "-q", "-m", "older"]);
    write(&d, "README", "again\n");
    git(&d, &["stash", "push", "-q", "-m", "newer"]);
    status_arrives(&mut app, &d);
    let ids = app.ws.get(&d).unwrap().status().unwrap().stash_ids.clone();
    let op = |fx: &[Effect]| match fx {
        [Effect::Jobs(j)] => j[0].op.clone(),
        other => panic!("{other:?}"),
    };
    let at = |index: usize| (index, ids[index].clone());
    // on the repo's own line: asked which, the newest first
    cursor_on_detail(&mut app, "dirty", "path");
    app.on_key(key('z'));
    assert!(matches!(&app.mode, Mode::Menu { menu, .. }
        if menu.items.iter().any(|i| i.key == 'p' && i.label == "pop…")));
    app.on_key(key('p'));
    match &app.mode {
        Mode::Pick { title, labels, .. } => {
            assert_eq!(title, "Pop · acme/dirty");
            assert_eq!(labels[..], ["stash@{0} newer", "stash@{1} older"]);
        }
        _ => panic!("no list"),
    }
    let (index, id) = at(0);
    assert_eq!(
        op(&app.on_key(code(KeyCode::Enter))),
        Op::StashPopAt { index, id }
    );
    app.running.clear();
    // the second of them
    app.on_key(key('z'));
    app.on_key(key('p'));
    app.on_key(code(KeyCode::Down));
    let (index, id) = at(1);
    assert_eq!(
        op(&app.on_key(code(KeyCode::Enter))),
        Op::StashPopAt { index, id }
    );
    app.running.clear();
    // drop: picked, then asked about
    app.on_key(key('z'));
    app.on_key(key('d'));
    assert!(app.on_key(code(KeyCode::Enter)).is_empty());
    match &app.mode {
        Mode::Confirm { question, .. } => assert_eq!(
            question,
            "Drop stash@{0} (newer) of acme/dirty? It can't be recovered."
        ),
        _ => panic!("not asked"),
    }
    let (index, id) = at(0);
    assert_eq!(op(&app.on_key(key('y'))), Op::StashDrop { index, id });
    app.running.clear();
    // on a stash's line: that one, pop without asking anything
    cursor_on_detail(&mut app, "dirty", "    stash@{1}");
    app.on_key(key('z'));
    let (index, id) = at(1);
    assert_eq!(op(&app.on_key(key('p'))), Op::StashPopAt { index, id });
    app.running.clear();
    app.on_key(key('z'));
    assert!(app.on_key(key('d')).is_empty());
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.starts_with("Drop stash@{1} "))
    );
    app.on_key(key('n'));
    // several repos selected, none under the cursor: one at a time
    cursor_on_detail(&mut app, "dirty", "path");
    app.marked = app.ws.entries.iter().map(|e| e.path.clone()).collect();
    assert!(app.act(Action::PopStash).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .contains("one repo at a time")
    );
    // so the menu doesn't offer them then; it does on a stash's line,
    // whatever is marked
    let offers = |app: &mut App| {
        app.on_key(key('z'));
        let has = matches!(&app.mode, Mode::Menu { menu, .. }
            if menu.items.iter().any(|i| i.key == 'p') && menu.items.iter().any(|i| i.key == 'd'));
        app.on_key(code(KeyCode::Esc));
        has
    };
    assert!(!offers(&mut app));
    app.marked = [sb.path("dev/clean")].into();
    cursor_on_detail(&mut app, "dirty", "    stash@{0}");
    assert!(offers(&mut app));
}

/// A file's diff is two columns in from the file, wherever the file is:
/// in a section, or in an untracked folder unfolded.
#[test]
fn a_diff_is_two_in_from_its_file_wherever_that_is() {
    let (_sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        write(&d, "dir/x", "1\n");
        write(&d, "dir/y", "2\n");
    });
    cursor_on_detail(&mut app, "dirty", "    dir/");
    app.on_key(code(KeyCode::Tab));
    let col = |app: &App, n: usize| {
        let t = &detail_texts(app)[n];
        t.len() - t.trim_start().len()
    };
    for file in ["dir/x", "README"] {
        let at = trimmed(&app)
            .iter()
            .position(|t| t.ends_with(file))
            .unwrap();
        app.cursor = app.row_of_detail(at);
        app.on_key(code(KeyCode::Tab));
        assert!(trimmed(&app)[at + 1].starts_with("@@"), "{file}");
        assert_eq!(col(&app, at + 1), col(&app, at) + 2, "{file}");
    }
}

#[test]
fn files_marked_in_details_can_be_stashed() {
    let (sb, mut app) = setup_with("", |sb| write(&sb.path("dev/dirty"), "other", "x\n"));
    // nothing marked yet: not offered, and said if asked for anyway
    app.on_key(key('z'));
    assert!(
        matches!(&app.mode, Mode::Menu { menu, .. } if menu.items.iter().all(|i| i.key != 'm'))
    );
    app.on_key(code(KeyCode::Esc));
    assert!(app.act(Action::Stash(StashKind::MarkedFiles)).is_empty());
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
    // with its diff open, the hunk and its two changed lines show it too
    // (and its section's heading, as the only file there)
    app.cursor = at;
    app.on_key(code(KeyCode::Tab));
    let out = render(&mut app, 100, 30);
    assert_eq!(out.lines().filter(|l| l.contains('▌')).count(), 5, "{out}");
    app.cursor = at;
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('z'));
    let fx = app.on_key(key('m'));
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

/// Expand the current repo and put the cursor on `file`'s line under
/// staged.
fn to_staged(app: &mut App, file: &str) {
    to_file(app, file);
    app.cursor = app
        .rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some(file)
            && line.diff.is_none()
            && line.section == Some(argus_tui::details::Section::Staged))
        })
        .unwrap_or_else(|| panic!("{file} staged: {:?}", trimmed(app)));
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
    let at = t.iter().position(|l| l == "M README").expect("its line");
    assert_eq!(
        t[at + 1..at + 4],
        ["@@ -1 +1 @@", "-hello world", "+hello there"]
    );
    // a file with both sides: each under its section, each side's diff
    // under its own line, opened one at a time
    to_file(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let both = t.iter().position(|l| l == "M both").unwrap();
    assert_eq!(
        t[both + 1..both + 7],
        ["@@ -1 +1 @@", "-two", "+three", "", "staged (1)", "M both"]
    );
    assert_eq!(t.len(), both + 7, "the staged side still folded: {t:?}");
    to_staged(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert_eq!(
        t[both + 6..],
        ["M both", "@@ -1 +1 @@", "-one", "+two"],
        "{t:?}"
    );
    // untracked: everything added; binary: said so
    to_file(&mut app, "new");
    app.on_key(code(KeyCode::Tab));
    to_file(&mut app, "bin");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let new = t.iter().position(|l| l == "new").unwrap();
    assert_eq!(t[new + 1..new + 4], ["@@ -0,0 +1,2 @@", "+a", "+b"]);
    let bin = t.iter().position(|l| l == "bin").unwrap();
    assert_eq!(t[bin + 1], "binary file");
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
    t.draw(|f| drop(ui::draw(f, app, &view))).unwrap();
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
        [Op::AddTracked {
            paths: paths(&["README"])
        }]
    );
    app.running.clear();
    // a file is unstaged from its line under staged
    to_staged(&mut app, "both");
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
    // the wrong side says what to do instead, and runs nothing
    let said = |app: &mut App, k: char| {
        assert!(app.on_key(key(k)).is_empty());
        assert!(matches!(app.mode, Mode::Normal), "nothing asked");
        app.message.as_ref().unwrap().0.clone()
    };
    to_file(&mut app, "new");
    assert_eq!(said(&mut app, 'u'), "that's not staged: s stages it");
    to_file(&mut app, "README");
    assert_eq!(said(&mut app, 'u'), "that's not staged: s stages it");
    to_staged(&mut app, "both");
    assert_eq!(said(&mut app, 's'), "that's staged already: u unstages it");
    assert_eq!(
        said(&mut app, 'K'),
        "that's staged: u unstages it, then K discards it"
    );
}

/// The cursor on the own line of `section` in the current repo's details.
fn to_section(app: &mut App, section: argus_tui::details::Section) {
    app.cursor = app
        .rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { line, .. }
            if argus_tui::details::Section::heads(line) == Some(section))
        })
        .unwrap_or_else(|| panic!("{section:?}: {:?}", trimmed(app)));
}

#[test]
fn a_sections_own_line_acts_on_all_its_files_and_folds_it() {
    use argus_tui::details::Section;
    let (_sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        let d = sb.path("dev/dirty");
        write(&d, "added", "x\n");
        git(&d, &["add", "added"]);
    });
    to_file(&mut app, "README");
    // s on unstaged: every file with unstaged changes
    to_section(&mut app, Section::Unstaged);
    let fx = app.on_key(key('s'));
    assert_eq!(
        job_ops(&fx),
        [Op::AddTracked {
            paths: paths(&["README", "both"])
        }]
    );
    app.running.clear();
    // on untracked: all of those
    to_section(&mut app, Section::Untracked);
    let fx = app.on_key(key('s'));
    assert_eq!(
        job_ops(&fx),
        [Op::Add {
            paths: paths(&["bin", "new"])
        }]
    );
    app.running.clear();
    // u on staged: everything staged, each as it's unstaged
    to_section(&mut app, Section::Staged);
    let fx = app.on_key(key('u'));
    let ops = job_ops(&fx);
    assert!(ops.contains(&Op::Remove {
        cached: true,
        paths: paths(&["added"])
    }));
    assert!(
        ops.iter().any(
            |o| matches!(o, Op::Restore { staged: true, paths: p, .. } if p == &paths(&["both"]))
        ),
        "{ops:?}"
    );
    app.running.clear();
    // the wrong way round is said, as for a file
    assert!(app.on_key(key('s')).is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("staged already"));
    // K on unstaged asks about all of them
    to_section(&mut app, Section::Unstaged);
    assert!(app.on_key(key('K')).is_empty());
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question == "Discard unstaged changes to 2 files? They're lost.")
    );
    app.on_key(key('n'));
    // Tab folds the section to its own line, and back; the others stay
    let lines = |app: &App| trimmed(app);
    let before = lines(&app);
    app.on_key(code(KeyCode::Tab));
    let folded = lines(&app);
    assert!(folded.contains(&"unstaged (2)".to_string()));
    assert!(!folded.contains(&"M README".to_string()), "{folded:?}");
    assert!(folded.contains(&"new".to_string()) && folded.contains(&"A added".to_string()));
    assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. }
        if Section::heads(line) == Some(Section::Unstaged)));
    app.on_key(code(KeyCode::Tab));
    assert_eq!(lines(&app), before);
    // a folded section's files are still what s on it stages
    app.on_key(code(KeyCode::Tab));
    let fx = app.on_key(key('s'));
    assert_eq!(
        job_ops(&fx),
        [Op::AddTracked {
            paths: paths(&["README", "both"])
        }]
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
    // tracked, then untracked
    assert_eq!(
        job_ops(&fx),
        [
            Op::AddTracked {
                paths: paths(&["README"])
            },
            Op::Add {
                paths: paths(&["new"])
            }
        ]
    );
}

#[test]
fn discarding_always_asks() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    let fx = app.on_key(key('K'));
    assert!(fx.is_empty());
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(
        render(&mut app, 100, 30)
            .contains("Discard unstaged changes to README? They're lost. (y/n)")
    );
    let fx = app.on_key(key('n'));
    assert!(fx.is_empty() && matches!(app.mode, Mode::Normal));
    // several files: counted; y runs the discard
    to_file(&mut app, "README");
    app.on_key(key(' '));
    to_file(&mut app, "new");
    app.on_key(key(' '));
    app.on_key(key('K'));
    // (saying what goes: the untracked one is deleted)
    assert!(render(&mut app, 120, 30).contains(
        "Discard unstaged changes to 1 file and delete 1 untracked? They're lost. (y/n)"
    ));
    let fx = app.on_key(key('y'));
    assert_eq!(
        job_ops(&fx),
        [
            Op::Restore {
                staged: false,
                worktree: true,
                from_head: false,
                paths: paths(&["README"])
            },
            Op::Clean {
                paths: paths(&["new"])
            },
        ]
    );
    app.running.clear();
    app.marked_files.clear();
    // an untracked file alone: deleted, said so
    to_file(&mut app, "new");
    app.on_key(key('K'));
    assert!(render(&mut app, 100, 30).contains("Delete untracked new? It's lost. (y/n)"));
    app.on_key(key('n'));
    // a file with staged changes too: only the unstaged ones go, from
    // its line or from the section's
    to_file(&mut app, "both");
    app.on_key(key('K'));
    let fx = app.on_key(key('y'));
    assert_eq!(
        job_ops(&fx),
        [Op::Restore {
            staged: false,
            worktree: true,
            from_head: false,
            paths: paths(&["both"])
        }]
    );
    app.running.clear();
    to_section(&mut app, argus_tui::details::Section::Unstaged);
    app.on_key(key('K'));
    let fx = app.on_key(key('y'));
    assert_eq!(
        job_ops(&fx),
        [Op::Restore {
            staged: false,
            worktree: true,
            from_head: false,
            paths: paths(&["README", "both"])
        }]
    );
    app.running.clear();
    // marked where it's staged: not K's to take, and nothing asked
    let d = app.current().unwrap().path.clone();
    git(&d, &["add", "README", "both"]);
    app.ws.refresh(&d);
    app.rebuild();
    to_file(&mut app, "README");
    app.on_key(key(' '));
    assert!(app.on_key(key('K')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .contains("some marked files are staged")
    );
}

#[test]
fn the_stash_list_says_how_many_are_ignored_and_stops_after_ten() {
    for (n, extra, first, more) in [
        (
            10,
            "[tags.ignore.issues]\nstashes = []",
            "stashes (10)",
            None,
        ),
        (11, "", "stashes (11, 11 ignored)", Some("… 1 more")),
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
        // in the stash colour while any of them counts as an issue
        let Row::Detail { line, .. } = &app.rows[app.cursor] else {
            panic!("the heading");
        };
        let role = argus_core::badges::Role::Stash;
        assert_eq!(line.parts[0].0 == Some(role), n == 10, "{:?}", line.parts);
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
            branch: "done".into(),
            force_at: None,
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
    // the Fix menu's key for it is k (A fixes everything shown)
    app.on_key(key('x'));
    assert!(
        menu_items(&app).iter().any(|i| i.starts_with("k abort…")),
        "{:?}",
        menu_items(&app)
    );
    app.on_key(key('k'));
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.starts_with("Abort merge in acme/clean?"))
    );
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
        "fix once acme/feature: push… · 2 with nothing to apply"
    );
    assert!(app.act(Action::AutoFix(Scope::Selected)).is_empty());
    assert_eq!(msg(&app), "nothing started · 1 busy");
    // when it's finished
    app.on_msg(Msg::FixDone {
        path: f.clone(),
        applied: Ok(None),
    });
    assert_eq!(msg(&app), "fix once acme/feature: push: done");
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
    assert_eq!(msg(&app), "fix once 3 repo(s): switch-default (3)…");
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
fn the_fix_menus_auto_fix_items_say_what_they_do() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    app.on_key(key('x'));
    assert_eq!(
        menu_items(&app)[..4],
        [
            "x fix once acme/feature: switch main",
            "a fix all once · acme/feature: switch main",
            "X fix auto acme/feature: switch main",
            "A fix all auto · acme/feature: switch main"
        ]
    );
    // no header saying it again
    let screen = render(&mut app, 100, 20);
    assert!(!screen.contains("x: "), "{screen}");
    app.on_key(key('q'));
    // nothing to auto-fix in the selection: no x
    app.act(Action::Bottom);
    app.on_key(key('x'));
    assert_eq!(
        menu_items(&app)[..2],
        [
            "a fix all once · acme/feature: switch main",
            "A fix all auto · acme/feature: switch main"
        ]
    );
    assert!(
        !menu_items(&app)
            .iter()
            .any(|i| i.starts_with("x ") || i.starts_with("X "))
    );
}

#[test]
fn the_footer_names_n_p_j_k_in_lists_only() {
    let (_sb, mut app) = setup_with("", two_local_commits);
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("q/Esc/C-g cancel · ↑↓ C-n C-p Enter"),
        "{screen}"
    );
    app.on_key(key('s'));
    let screen = render(&mut app, 100, 20);
    assert!(
        screen.contains("q/Esc/C-g cancel · ↑↓ n p j k Enter"),
        "{screen}"
    );
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
    let nd = |app: &App| app.ws.config.ignore_issues().non_default_branch.clone();
    assert_eq!(
        menu_items(&app),
        [
            "1 · not on default branch (main)",
            "a all: ignore 1 remaining issue(s)"
        ]
    );
    // counted, pinned (the default), ignored everywhere, counted; the menu stays
    let fx = app.on_key(key('1'));
    assert!(matches!(fx.as_slice(), [Effect::SaveTags(_)]), "{fx:?}");
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
    let stashes = app.ws.config.ignore_issues().stashes.clone();
    // each its default way; the ignored one left as it was
    app.on_key(key('a'));
    assert_eq!(
        app.ws.config.ignore_issues().non_default_branch,
        ["github.com/acme/feature@feat"]
    );
    assert_eq!(app.ws.config.ignore_issues().stashes, stashes);
    assert_eq!(menu_items(&app)[2..], ["n none: stop ignoring 2 issue(s)"]);
    // nothing remaining: a isn't there, and changes nothing
    assert!(app.on_key(key('a')).is_empty());
    assert_eq!(app.ws.config.ignore_issues().stashes, stashes);
    let fx = app.on_key(key('n'));
    assert!(matches!(fx.as_slice(), [Effect::SaveTags(_)]), "{fx:?}");
    assert!(app.ws.config.ignore_issues().non_default_branch.is_empty());
    assert_eq!(
        app.ws.config.ignore_issues().stashes[stashes.len()..],
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
    let stashes = app.ws.config.ignore_issues().stashes.clone();
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
    assert!(app.ws.config.ignore_issues().non_default_branch.is_empty());
    assert_eq!(app.ws.config.ignore_issues().stashes, stashes);
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
    let defaults = argus_core::Config::default()
        .ignore_issues()
        .stashes
        .clone();
    let own = |app: &App| app.ws.config.ignore_issues().stashes[defaults.len()..].to_vec();
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
    assert_eq!(label(&mut app, 't', 'f'), "f tag favourite");
    assert_eq!(label(&mut app, 't', 'I'), "I ignore repo");
    assert_eq!(label(&mut app, 't', 'i'), "i ignore issues…");
    app.act(Action::ToggleLabel(0));
    assert_eq!(label(&mut app, 't', 'f'), "f untag favourite");
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

    assert_eq!(
        label(&mut app, 't', 'p'),
        "p pull (with F a and argus pull)"
    );
    app.act(Action::TogglePull);
    assert_eq!(label(&mut app, 't', 'p'), "p stop pulling");
    app.act(Action::ToggleAttention);
    assert_eq!(label(&mut app, 'v', 'a'), "a show all repos");
    app.act(Action::ToggleHidden);
    assert_eq!(
        label(&mut app, 'v', 'h'),
        "h hide ignored and snoozed repos"
    );
    // shown while ignored, so it stays the first selected
    app.act(Action::ToggleAttention);
    app.act(Action::ToggleIgnore);
    assert_eq!(current(&app), "acme/feature");
    assert_eq!(label(&mut app, 't', 'I'), "I unignore repo");
}

#[test]
fn paging_and_home_end_move_by_the_page_and_to_the_ends() {
    let (_sb, mut app) = setup("");
    app.page = 1;
    for k in [code(KeyCode::PageDown), ctrl('v'), ctrl('d')] {
        app.cursor = 0;
        app.on_key(k);
        assert_eq!(app.cursor, 1, "{k:?}");
    }
    for k in [code(KeyCode::PageUp), alt('v'), ctrl('u')] {
        app.cursor = 2;
        app.on_key(k);
        assert_eq!(app.cursor, 1, "{k:?}");
    }
    app.page = 20;
    app.cursor = 0;
    app.on_key(code(KeyCode::PageDown));
    assert_eq!(app.cursor, 2, "clamped at the bottom");
    app.on_key(code(KeyCode::Home));
    assert_eq!(app.cursor, 0);
    app.on_key(code(KeyCode::End));
    assert_eq!(app.cursor, 2);
}

#[test]
fn ctrl_keys_quit_refresh_and_leave_repo_marks() {
    let (_sb, mut app) = setup("");
    assert!(matches!(app.on_key(ctrl('l')).as_slice(), [Effect::Refresh(p)] if p.len() == 3));
    // (C-g is for what's marked in details: a marked repo stays, U's)
    app.on_key(key(' '));
    app.on_key(ctrl('g'));
    assert_eq!(app.marked.len(), 1);
    app.on_key(key('U'));
    assert!(app.marked.is_empty());
    assert_eq!(app.on_key(ctrl('c')), vec![Effect::SaveState, Effect::Quit]);
}

#[test]
fn help_scrolls_with_arrows_vi_and_emacs_keys_and_any_other_key_closes_it() {
    let (_sb, mut app) = setup("");
    let scroll = |app: &App| match app.mode {
        Mode::Help { scroll } => scroll,
        _ => panic!("help closed"),
    };
    app.on_key(key('?'));
    for (down, up) in [
        (code(KeyCode::Down), code(KeyCode::Up)),
        (key('j'), key('k')),
        (ctrl('n'), ctrl('p')),
        (key('n'), key('p')),
    ] {
        app.on_key(down);
        app.on_key(down);
        assert_eq!(scroll(&app), 2, "{down:?}");
        app.on_key(up);
        assert_eq!(scroll(&app), 1, "{up:?}");
        app.on_key(up);
        app.on_key(up);
        assert_eq!(scroll(&app), 0, "stops at the top");
    }
    // help is open here: any other key closes it
    for k in ['x', 'q', '?'] {
        assert!(matches!(app.mode, Mode::Help { .. }), "open before {k}");
        app.on_key(key(k));
        assert!(matches!(app.mode, Mode::Normal), "{k} closes it");
        app.on_key(key('?'));
    }
}

#[test]
fn menu_bar_wraps_both_ways_and_enter_keeps_open_only_what_stays() {
    let (_sb, mut app) = setup("");
    let cursor = |app: &App| match app.mode {
        Mode::Menu { cursor, .. } => cursor,
        _ => panic!("menu closed"),
    };
    app.on_key(key('z')); // stash: two switches, then actions
    let n = menu_items(&app).len();
    for (down, up) in [
        (code(KeyCode::Down), code(KeyCode::Up)),
        (code(KeyCode::Tab), code(KeyCode::BackTab)),
        (ctrl('n'), ctrl('p')),
    ] {
        let start = cursor(&app);
        app.on_key(up);
        assert_eq!(cursor(&app), (start + n - 1) % n, "{up:?} wraps");
        app.on_key(down);
        app.on_key(down);
        assert_eq!(cursor(&app), (start + 1) % n, "{down:?}");
        app.on_key(up);
    }
    // the first item is a switch: Enter toggles it and the menu stays
    while cursor(&app) != 0 {
        app.on_key(code(KeyCode::Down));
    }
    app.on_key(code(KeyCode::Enter));
    assert_eq!(cursor(&app), 0, "still open");
    app.on_key(code(KeyCode::Enter));
    app.on_key(code(KeyCode::Down));
    app.on_key(code(KeyCode::Down));
    app.on_key(ctrl('g'));
    app.on_key(key('v')); // view: nothing stays
    app.on_key(code(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Normal));
    app.on_key(key('v'));
    assert!(
        matches!(app.on_key(key('g')).as_slice(), [Effect::Refresh(_)]),
        "plain g is the menu's item; only C-g closes"
    );
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
    cursor_on_detail(&mut app, "dirty", "    M README");
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
    cursor_on_detail(&mut app, "clean", "    build/");
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
        ["sub/a.log", "sub/b.log"]
            .map(|f| (argus_tui::details::Section::Untracked, f.to_string()))
            .into(),
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
    cursor_on_detail(&mut app, "clean", "    build/");
    app.act(Action::ToggleExpand);
    assert_eq!(below(&app, "    build/", 4), ["build/a.o", "build/sub/b.o"]);
    // a file in it: staged, gitignored, diffed like any untracked file
    cursor_on_detail(&mut app, "clean", "      build/a.o");
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
    cursor_on_detail(&mut app, "clean", "      build/a.o");
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
    cursor_on_detail(&mut app, "clean", "    stash@{0} keep this");
    app.act(Action::ToggleExpand);
    let held = below(&app, "    stash@{0} keep this", 4);
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
            // its hunk, the next file, then over the blank line to the
            // next section's own line
            "@@ -1 +1 @@",
            "M both",
            "staged (1)",
            "M both",
            "@@ -1 +1 @@"
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
    assert!(fix("commit").ends_with("  (c A)"), "{}", fix("commit"));
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

/// `clean` with two commits on no remote after the pushed "initial".
fn two_local_commits(sb: &Sandbox) {
    let c = sb.path("dev/clean");
    for m in ["local one", "local two"] {
        write(&c, "README", &format!("{m}\n"));
        commit(&c, m);
    }
}

fn rev(sb: &Sandbox, r: &str, spec: &str) -> String {
    git(&sb.path(r), &["rev-parse", spec]).trim().to_string()
}

#[test]
fn reset_picks_a_commit_starting_before_head() {
    let (sb, mut app) = setup_with("", two_local_commits);
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    assert_eq!(
        menu_items(&app),
        [
            "s soft: keep the changes staged",
            "m mixed: keep the changes, unstaged",
            "k keep: drop the commits' changes; refuse if uncommitted ones are in the way",
            "h hard: drop the commits' changes and every uncommitted one…",
        ]
    );
    app.on_key(key('s'));
    let short = |spec: &str| rev(&sb, "dev/clean", spec)[..7].to_string();
    match &app.mode {
        Mode::Menu { menu, cursor, .. } => {
            assert_eq!(menu.title, "Reset --soft · acme/clean · to which commit?");
            assert_eq!(*cursor, 1, "the commit before HEAD");
        }
        _ => panic!("no menu"),
    }
    assert_eq!(
        menu_items(&app),
        [
            format!("1 {} local two (HEAD)", short("HEAD")),
            format!("2 {} local one", short("HEAD~1")),
            format!("3 {} initial · on a remote", short("HEAD~2")),
        ]
    );
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Reset {
                mode: ResetMode::Soft,
                commit: rev(&sb, "dev/clean", "HEAD~1")
            }
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn reset_from_a_commit_starts_at_its_parent() {
    let (sb, mut app) = setup_with("", two_local_commits);
    cursor_on_detail(&mut app, "clean", "issue     2 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('n'));
    app.on_key(key('n'));
    assert!(here(&app).contains(" local one "), "{}", here(&app));
    app.on_key(key('X'));
    app.on_key(key('m'));
    assert!(matches!(app.mode, Mode::Menu { cursor: 2, .. }));
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Reset {
                mode: ResetMode::Mixed,
                commit: rev(&sb, "dev/clean", "HEAD~2")
            }
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_hard_reset_asks_first() {
    let (sb, mut app) = setup_with("", two_local_commits);
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    app.on_key(key('h'));
    assert!(app.on_key(key('2')).is_empty());
    match &app.mode {
        Mode::Confirm { question, .. } => assert_eq!(
            question,
            &format!(
                "Reset acme/clean --hard to {} (local one)? Uncommitted changes are lost.",
                &rev(&sb, "dev/clean", "HEAD~1")[..7]
            )
        ),
        _ => panic!("not asked"),
    }
    assert!(app.on_key(key('n')).is_empty(), "no: nothing done");
    app.on_key(key('X'));
    app.on_key(key('h'));
    app.on_key(key('2'));
    match app.on_key(key('y')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Reset {
                mode: ResetMode::Hard,
                commit: rev(&sb, "dev/clean", "HEAD~1")
            }
        ),
        other => panic!("{other:?}"),
    }
}

/// A mixed reset keeps the work tree but not the index: what's staged of
/// a file changed again since is nowhere else, so it's asked about.
#[test]
fn a_mixed_reset_asks_only_when_something_staged_would_be_lost() {
    let (sb, mut app) = setup_with("", two_local_commits);
    let c = sb.path("dev/clean");
    let reset = |app: &mut App| {
        while current(app) != "acme/clean" {
            app.act(Action::Down);
        }
        app.on_key(key('X'));
        app.on_key(key('m'));
        app.on_key(key('2'))
    };
    // staged, then changed again: asked
    write(&c, "README", "staged\n");
    git(&c, &["add", "README"]);
    write(&c, "README", "and changed again\n");
    app.ws.refresh(&c);
    app.rebuild();
    assert!(reset(&mut app).is_empty());
    match &app.mode {
        Mode::Confirm { question, .. } => assert_eq!(
            question,
            &format!(
                "Reset acme/clean --mixed to {} (local one)? What's staged of README \
                 differs from the work tree and is lost.",
                &rev(&sb, "dev/clean", "HEAD~1")[..7]
            )
        ),
        _ => panic!("not asked"),
    }
    assert!(app.on_key(key('n')).is_empty(), "no: nothing done");
    reset(&mut app);
    match app.on_key(key('y')).as_slice() {
        [Effect::Jobs(j)] => assert!(matches!(
            j[0].op,
            Op::Reset {
                mode: ResetMode::Mixed,
                ..
            }
        )),
        other => panic!("{other:?}"),
    }
    // staged for deletion and still there: nothing to lose either
    let (sb, mut app) = setup_with("", two_local_commits);
    let c = sb.path("dev/clean");
    git(&c, &["rm", "-q", "--cached", "README"]);
    app.ws.refresh(&c);
    app.rebuild();
    assert!(matches!(reset(&mut app).as_slice(), [Effect::Jobs(_)]));
    // inverse: staged, and the same in the work tree: nothing to lose
    let (sb, mut app) = setup_with("", two_local_commits);
    let c = sb.path("dev/clean");
    write(&c, "README", "staged\n");
    git(&c, &["add", "README"]);
    app.ws.refresh(&c);
    app.rebuild();
    assert!(matches!(reset(&mut app).as_slice(), [Effect::Jobs(_)]));
}

#[test]
fn reset_is_one_repo_at_a_time() {
    let (_sb, mut app) = setup("");
    app.act(Action::ToggleMark);
    app.act(Action::ToggleMark);
    app.on_key(key('X'));
    assert!(app.on_key(key('s')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.message.as_ref().unwrap().0, "reset one repo at a time");
}

#[test]
fn reset_with_one_commit_starts_on_it() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    app.on_key(key('k'));
    assert!(matches!(app.mode, Mode::Menu { cursor: 0, .. }));
    assert_eq!(menu_items(&app).len(), 1);
}

#[test]
fn n_p_j_k_move_through_lists_of_things_only() {
    let (_sb, mut app) = setup_with("", two_local_commits);
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    app.on_key(key('s'));
    let cursor = |app: &App| match app.mode {
        Mode::Menu { cursor, .. } => cursor,
        _ => panic!("menu closed"),
    };
    assert_eq!(cursor(&app), 1);
    for (k, at) in [('n', 2), ('p', 1), ('j', 2), ('k', 1)] {
        assert!(app.on_key(key(k)).is_empty(), "{k}: no reset");
        assert_eq!(cursor(&app), at, "{k}");
    }
    // menus of choices: keys only, never moving; F p pulls
    app.on_key(ctrl('g'));
    app.on_key(key('c'));
    app.on_key(key('j'));
    assert_eq!(cursor(&app), 0, "j isn't a Commit menu key: nothing");
    app.on_key(ctrl('g'));
    app.on_key(key('F'));
    assert!(matches!(app.on_key(key('p')).as_slice(), [Effect::Jobs(_)]));
}

#[test]
fn a_long_menu_scrolls_with_its_cursor() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        for n in 1..=20 {
            write(&c, "README", &format!("{n}\n"));
            commit(&c, &format!("change {n}"));
        }
    });
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    app.on_key(key('s'));
    let screen = render(&mut app, 80, 14);
    assert!(screen.contains("change 20 (HEAD)"), "{screen}");
    assert!(screen.contains("↓ 1"), "{screen}");
    assert!(!screen.contains("initial"), "{screen}");
    let more = |screen: &str, arrow: char| {
        screen
            .lines()
            .any(|l| l.contains(&format!("{arrow} ")) && l.contains(" more"))
    };
    assert!(!more(&screen, '↑'), "nothing above: {screen}");
    // its title names the repo already: no repo count after it
    assert!(screen.contains("to which commit? ─"), "{screen}");
    // as wide as it needs, not the whole window
    let top = screen.lines().find(|l| l.contains("Reset --soft")).unwrap();
    assert!(top.trim_end().chars().count() < 70, "{top}");
    // in the middle: hidden both ways
    for _ in 0..9 {
        app.on_key(key('n'));
    }
    let screen = render(&mut app, 80, 14);
    assert!(more(&screen, '↑') && more(&screen, '↓'), "{screen}");
    for _ in 0..9 {
        app.on_key(key('p'));
    }
    // to the last: wrapping back from the top
    app.on_key(key('p'));
    app.on_key(key('p'));
    let screen = render(&mut app, 80, 14);
    assert!(screen.contains("initial · on a remote"), "{screen}");
    assert!(screen.contains("↑ 1"), "{screen}");
    assert!(!more(&screen, '↓'), "nothing below: {screen}");
    // a full window, not half of one
    assert!(
        screen.contains("change 6\n") || screen.contains("change 6 "),
        "{screen}"
    );
    assert!(!screen.contains("change 20"), "{screen}");
}

/// Type `text` into the open prompt after clearing it (`C-a C-k`).
fn retype(app: &mut App, text: &str) {
    app.on_key(ctrl('a'));
    app.on_key(ctrl('k'));
    for c in text.chars() {
        app.on_key(key(c));
    }
}

#[test]
fn amend_starts_from_the_message_keeping_its_body() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        write(&c, "README", "x\n");
        git(&c, &["add", "README"]);
        git(
            &c,
            &["commit", "-q", "-m", "local summary", "-m", "the body"],
        );
    });
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    let amend = |app: &mut App, text: Option<&str>| {
        app.on_key(key('c'));
        app.on_key(key('a'));
        let screen = render(app, 100, 20);
        assert!(
            screen.contains("amend acme/clean: local summary"),
            "{screen}"
        );
        if let Some(t) = text {
            retype(app, t);
        }
        match app.on_key(code(KeyCode::Enter)).as_slice() {
            [Effect::Jobs(j)] => j[0].op.clone(),
            other => panic!("{other:?}"),
        }
    };
    // unchanged: the message stays as it is
    assert_eq!(amend(&mut app, None), Op::Amend { message: None });
    app.running.clear();
    assert_eq!(
        amend(&mut app, Some("better summary")),
        Op::Amend {
            message: Some("better summary\n\nthe body".into())
        }
    );
}

#[test]
fn amending_a_commit_on_a_remote_says_pushing_needs_force() {
    let (_sb, mut app) = setup("");
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('c'));
    app.on_key(key('a'));
    let screen = render(&mut app, 120, 20);
    assert!(
        screen.contains("amend acme/clean (on a remote: pushing it will need force): initial"),
        "{screen}"
    );
    // several repos: one at a time
    app.on_key(code(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
    app.act(Action::Top);
    app.act(Action::ToggleMark);
    app.act(Action::ToggleMark);
    app.act(Action::Amend);
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.message.as_ref().unwrap().0, "amend one repo at a time");
}

#[test]
fn the_fix_menu_offers_each_fix_that_applies() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    while current(&app) != "acme/dirty" {
        app.act(Action::Down);
    }
    app.on_key(key('x'));
    let items = menu_items(&app);
    assert_eq!(
        items[..4],
        [
            "x fix once acme/dirty: push",
            "a fix all once · 2 repo(s): push (1), switch-default (1)",
            "X fix auto acme/dirty: push",
            "A fix all auto · 2 repo(s): push (1), switch-default (1)"
        ]
    );
    assert_eq!(
        items[4..],
        [
            "s stash: 5 uncommitted path(s)",
            "c commit…: 5 uncommitted path(s)",
            "i gitignore…: 2 untracked path(s)",
            "p push: 1 commit(s) ahead of origin/main",
        ]
    );
    // nothing in progress: no abort
    assert!(!items.iter().any(|i| i.starts_with("k ")), "{items:?}");
    // one that asks first opens what it asks with
    app.on_key(key('i'));
    match &app.mode {
        Mode::Menu { menu, .. } => assert!(menu.title.starts_with("Gitignore"), "{}", menu.title),
        _ => panic!("no gitignore menu"),
    }
    // several repos: how many each applies to
    app.on_key(ctrl('g'));
    app.act(Action::Top);
    app.act(Action::ToggleMark);
    app.act(Action::ToggleMark);
    app.on_key(key('x'));
    assert!(
        menu_items(&app).contains(&"d switch-default (1 of 2 repos)".to_string()),
        "{:?}",
        menu_items(&app)
    );
}

#[test]
fn the_fix_menu_starts_on_the_fix_under_the_cursor() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    cursor_on_detail(&mut app, "dirty", "fix       gitignore");
    app.on_key(key('x'));
    match &app.mode {
        Mode::Menu { menu, cursor, .. } => {
            assert!(menu.items[*cursor].label.starts_with("gitignore…"))
        }
        _ => panic!("no menu"),
    }
    // elsewhere: the first item
    cursor_on_detail(&mut app, "dirty", "remote");
    app.on_key(ctrl('g'));
    app.on_key(key('x'));
    assert!(matches!(app.mode, Mode::Menu { cursor: 0, .. }));
}

#[test]
fn fixes_that_ask_first_ask_from_the_fix_menu_too() {
    let (_sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        git(&sb.path("dev/dirty"), &["branch", "old", "HEAD~1"]);
    });
    while current(&app) != "acme/dirty" {
        app.act(Action::Down);
    }
    app.on_key(key('x'));
    assert!(app.on_key(key('c')).is_empty());
    assert!(matches!(
        app.mode,
        Mode::Input {
            prompt: Prompt::CommitMessage(_, true),
            ..
        }
    ));
    app.on_key(code(KeyCode::Esc));
    app.on_key(key('x'));
    assert!(app.on_key(key('D')).is_empty());
    assert!(matches!(app.mode, Mode::Confirm { .. }));
}

#[test]
fn a_key_for_an_item_scrolled_out_of_sight_only_brings_it_into_view() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        for n in 1..=20 {
            write(&c, "README", &format!("{n}\n"));
            commit(&c, &format!("change {n}"));
        }
    });
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    app.on_key(key('s'));
    let screen = render(&mut app, 80, 14);
    assert!(!screen.contains("initial"), "{screen}");
    // `m` is the last commit's key: out of sight, so nothing happens but scrolling to it
    assert!(app.on_key(key('m')).is_empty());
    assert!(matches!(app.mode, Mode::Menu { cursor: 20, .. }));
    let screen = render(&mut app, 80, 14);
    assert!(screen.contains("m  "), "{screen}");
    assert!(screen.contains("initial"), "{screen}");
    // now that it's shown, the key picks it
    match app.on_key(key('m')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Reset {
                mode: ResetMode::Soft,
                commit: rev(&sb, "dev/clean", "HEAD~20")
            }
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn with_nothing_to_fix_the_fix_menu_doesnt_open() {
    let sb = Sandbox::new();
    sb.remote("acme", "w");
    sb.clone_as_github("acme", "w", "dev/w");
    let cfg = format!("roots = ['{}']", sb.path("dev").display());
    let mut ws = Workspace::new(Config::parse(&cfg).unwrap(), State::default()).unwrap();
    ws.scan();
    let mut app = App::new(ws);
    assert!(app.on_key(key('x')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "nothing to fix in the selection"
    );
}

#[test]
fn reset_with_no_commits_says_so() {
    let (_sb, mut app) = setup_with("", |sb| {
        git(&sb.path("dev"), &["init", "-q", "empty"]);
    });
    while !app.current().unwrap().path.ends_with("empty") {
        app.act(Action::Down);
    }
    app.on_key(key('X'));
    assert!(app.on_key(key('s')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.message.as_ref().unwrap().0, "no commits to reset to");
}

#[test]
fn a_ctrl_key_in_a_menu_isnt_its_letter() {
    let (_sb, mut app) = setup("");
    app.on_key(key('F'));
    // `a` pulls all tagged for pull; C-a doesn't
    assert!(app.on_key(ctrl('a')).is_empty());
    assert!(matches!(app.mode, Mode::Menu { .. }));
}

#[test]
fn all_leaves_issues_already_ignored_another_way() {
    let (_sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/feature");
        write(&r, "README", "fix's\n");
        git(&r, &["stash", "push", "-q", "-m", "argus 2026-01-02 09:30"]);
    });
    cursor_on_detail(&mut app, "feature", "issue     not on default branch");
    app.act(Action::IgnoreIssues);
    // not on the default branch: ignored on any branch (not its default, pinned)
    app.on_key(key('1'));
    app.on_key(key('1'));
    assert_eq!(
        app.ws.config.ignore_issues().non_default_branch,
        ["github.com/acme/feature"]
    );
    app.on_key(key('a'));
    assert_eq!(menu_items(&app)[1], "2 ● 1 stash(es) [ignored]");
    assert_eq!(
        app.ws.config.ignore_issues().non_default_branch,
        ["github.com/acme/feature"]
    );
}

#[test]
fn branch_lines_show_upstream_state_merged_and_unpushed_work() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        git(&c, &["switch", "-q", "-c", "gone"]);
        git(&c, &["push", "-q", "-u", "origin", "gone"]);
        git(&c, &["push", "-q", "origin", "--delete", "gone"]);
        git(&c, &["switch", "-q", "main"]);
        git(&c, &["branch", "merged"]);
        // on the remote, just not as its upstream: nothing unpushed
        git(&c, &["branch", "--no-track", "elsewhere", "origin/main"]);
        git(&c, &["branch", "--track", "behind", "origin/main"]);
        commit(&c, "two");
        git(&c, &["push", "-q", "origin", "main"]);
        git(
            &c,
            &["switch", "-q", "-c", "ahead", "--track", "origin/main"],
        );
        commit(&c, "three");
        git(&c, &["switch", "-q", "-c", "local", "main"]);
        commit(&c, "four");
        git(&c, &["switch", "-q", "main"]);
        git(&c, &["fetch", "-q", "--prune"]);
    });
    use argus_core::badges::Role::*;
    cursor_on_detail(&mut app, "clean", "branches");
    let branch = |name: &str| {
        app.rows[app.cursor..]
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. } if line.parts.get(1).is_some_and(|p| p.1 == name) => {
                    Some(line.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    let parts = |name: &str| branch(name).parts[2..].to_vec();
    let s = |r, t: &str| (Some(r), t.to_string());
    assert_eq!(branch("main").parts[1], s(DefaultBranch, "main"));
    assert_eq!(
        parts("main"),
        [s(Dim, "  origin/main")],
        "not merged into itself"
    );
    assert_eq!(parts("ahead"), [s(Dim, "  origin/main"), s(Ahead, " ↑1")]);
    assert_eq!(
        parts("behind"),
        [
            s(Dim, "  origin/main"),
            s(Behind, " ↓1"),
            s(Dim, "  merged")
        ]
    );
    assert_eq!(
        parts("gone"),
        [s(Warning, "  origin/gone gone"), s(Dim, "  merged")]
    );
    assert_eq!(
        parts("merged"),
        [s(Dim, "  (no upstream)"), s(Dim, "  merged")]
    );
    assert_eq!(
        parts("elsewhere"),
        [s(Dim, "  (no upstream)"), s(Dim, "  merged")]
    );
    assert_eq!(
        parts("local"),
        [s(Dim, "  (no upstream)"), s(Dim, "  1 on no remote")]
    );
    // the commits on no remote unfold from the branch, and only there
    for (name, unfolds) in [
        ("ahead", true),
        ("local", true),
        ("elsewhere", false),
        ("main", false),
    ] {
        assert_eq!(branch(name).expand.is_some(), unfolds, "{name}");
    }
}

#[test]
fn detail_lines_carry_their_issue_fix_and_unfolding() {
    use argus_core::attention::Issue;
    let (_sb, mut app) = setup_with("[tags.ignore.issues]\nbehind = ['acme/clean']", |sb| {
        let seed = sb.path("seed/acme/clean");
        commit(&seed, "upstream");
        let bare = sb.path("forge/acme/clean.git");
        git(&seed, &["push", "-q", bare.to_str().unwrap(), "main"]);
        git(&sb.path("dev/clean"), &["fetch", "-q"]);
    });
    let line = |app: &App, text: &str| {
        app.rows
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. }
                    if line
                        .parts
                        .iter()
                        .map(|p| p.1.as_str())
                        .collect::<String>()
                        .starts_with(text) =>
                {
                    Some(line.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {text:?}: {:?}", detail_texts(app)))
    };
    cursor_on_detail(&mut app, "clean", "issue");
    let behind = line(&app, "issue     1 commit(s) behind");
    assert!(behind.ignored, "{:?}", behind.parts);
    assert_eq!(behind.issue, Some(Issue::Behind(1)));
    assert!(behind.expand.is_some(), "its commits unfold");
    let activity = line(&app, "activity  ");
    let text: String = activity.parts.iter().map(|p| p.1.as_str()).collect();
    assert!(
        text.starts_with("activity  commit ") && text.contains("s ago"),
        "{text}"
    );
    assert!(
        !detail_texts(&app).iter().any(|t| t.starts_with("flags")),
        "no flags, no line"
    );
    app.act(Action::ToggleLabel(0));
    cursor_on_detail(&mut app, "clean", "flags");
    assert_eq!(
        detail_texts(&app)
            .iter()
            .find(|t| t.starts_with("flags"))
            .unwrap(),
        "flags     favourite"
    );

    cursor_on_detail(&mut app, "dirty", "issue");
    let dirty = line(&app, "issue     ");
    assert_eq!(dirty.issue, Some(Issue::Dirty(1)));
    assert!(!dirty.ignored);
    let stash = line(&app, "fix       stash");
    assert_eq!(stash.fix, Some(argus_core::fix::FixKind::Stash));
}

#[test]
fn change_lines_are_coloured_by_state_and_the_rest_counted() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        write(&c, "t.txt", "t\n");
        commit(&c, "t");
        git(&c, &["switch", "-q", "-c", "other"]);
        write(&c, "README", "other\n");
        commit(&c, "other");
        git(&c, &["switch", "-q", "main"]);
        write(&c, "README", "main\n");
        commit(&c, "main");
        assert!(!argus_core::testkit::git_may_fail(&c, &["merge", "other"]));
        write(&c, "s.txt", "s\n");
        git(&c, &["add", "s.txt"]);
        write(&c, "t.txt", "changed\n");
        write(&c, "u.txt", "u\n");
    });
    use argus_core::badges::Role::*;
    cursor_on_detail(&mut app, "clean", "untracked");
    // each file under its section, its letter (an untracked one, its
    // path) in that section's colour
    let role = |path: &str| {
        app.rows
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. } if line.file.as_deref() == Some(path) => {
                    Some((line.section.unwrap(), line.parts[1].clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {path}"))
    };
    use argus_tui::details::Section;
    let part = |role, text: &str| (Some(role), text.to_string());
    assert_eq!(role("README"), (Section::Unstaged, part(Conflict, "U")));
    // (a conflicted file is under unstaged only)
    assert!(detail_texts(&app).contains(&"staged (1)".to_string()));
    assert!(detail_texts(&app).contains(&"unstaged (2)".to_string()));
    assert_eq!(role("s.txt"), (Section::Staged, part(Staged, "A")));
    assert_eq!(role("t.txt"), (Section::Unstaged, part(Unstaged, "M")));
    assert_eq!(
        role("u.txt"),
        (Section::Untracked, part(Untracked, "u.txt"))
    );

    // the dirty repo: 25 paths of a section fit, a 26th is counted
    let d = sb.path("dev/dirty");
    for n in 1..=25 {
        write(&d, &format!("new{n:02}.txt"), "x\n");
    }
    let more = |app: &mut App| {
        app.ws.refresh(&d);
        app.rebuild();
        cursor_on_detail(app, "dirty", "untracked");
        detail_texts(app)
            .into_iter()
            .find(|t| t.trim_start().starts_with('…'))
    };
    assert_eq!(more(&mut app), None, "25 shown");
    write(&d, "new26.txt", "x\n");
    assert_eq!(more(&mut app).as_deref(), Some("    … 1 more"));
    // (its own count: the modified README is in another section)
    assert!(detail_texts(&app).contains(&"untracked (26)".to_string()));
    assert!(detail_texts(&app).contains(&"unstaged (1)".to_string()));
    // the line saying so is its section's: nothing to stop at, and gone
    // when the section is folded
    let more_line = app.rows.iter().find_map(|r| match r {
        Row::Detail { line, .. }
            if line.parts.iter().any(|p| p.1.trim_start().starts_with('…')) =>
        {
            Some((line.section, line.is_body()))
        }
        _ => None,
    });
    assert_eq!(more_line, Some((Some(Section::Untracked), true)));
    app.cursor = (0..app.rows.len())
        .find(|&n| {
            matches!(&app.rows[n], Row::Detail { repo, line }
            if app.ws.entries[*repo].path == d
                && Section::heads(line) == Some(Section::Untracked))
        })
        .unwrap();
    app.on_key(code(KeyCode::Tab));
    assert_eq!(more(&mut app), None, "folded with its section");
    assert!(detail_texts(&app).contains(&"untracked (26)".to_string()));
}

/// The terminal after drawing `app` (mono theme, Unicode icons), for
/// styles and the cursor.
fn draw(app: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
    let (theme, _) = Theme::build("mono", &Default::default());
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons: argus_core::icons::UNICODE,
        tick: 0,
    };
    t.draw(|f| drop(ui::draw(f, app, &view))).unwrap();
    t
}

#[test]
fn truncation_keeps_what_fits_with_an_ellipsis() {
    for (s, w, want) in [
        ("abcd", 4, "abcd"),
        ("abcdef", 4, "abc…"),
        ("日本語", 6, "日本語"),
        ("日本語", 5, "日本…"),
        ("日本語", 4, "日…"),
        ("abc", 1, "…"),
    ] {
        assert_eq!(ui::truncate(s, w), want, "{s} in {w}");
    }
}

#[test]
fn repo_columns_line_up_and_nested_names_are_indented_with_their_icon() {
    let (_sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        let wt = sb.path("dev/clean-worktree-x");
        git(&c, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        let bare = sb.path("forge/acme/feature.git");
        git(
            &c,
            &["submodule", "add", "-q", bare.to_str().unwrap(), "sub"],
        );
        commit(&c, "sub");
        // a second badge on dirty: work on no remote, beside the change
        let d = sb.path("dev/dirty");
        write(&d, "a.txt", "a\n");
        git(&d, &["add", "a.txt"]);
        git(&d, &["commit", "-q", "-m", "a"]);
    });
    app.act(Action::Bottom);
    app.act(Action::ToggleLabel(0));
    app.act(Action::Top);
    let screen = render(&mut app, 100, 24);
    let row = |name: &str| {
        screen
            .lines()
            .find(|l| l.split_whitespace().any(|w| w == name))
            .unwrap_or_else(|| panic!("no {name}:\n{screen}"))
            .to_string()
    };
    // the widest name sets the column, with one space after it
    let wt = row("clean-worktree-x");
    assert!(
        wt.contains("├ clean-worktree-x clean-worktree-x"),
        "{screen}"
    );
    assert!(row("sub").contains("§ sub "), "{}", row("sub"));
    let branch_col = |name: &str, branch: &str| {
        let l = row(name);
        let after = l.find(name).unwrap() + name.len();
        l[..after + l[after..].find(branch).unwrap()]
            .chars()
            .count()
    };
    let col = branch_col("acme/dirty", "main");
    assert_eq!(branch_col("acme/feature", "feat"), col, "{screen}");
    assert_eq!(branch_col("acme/clean", "main"), col);
    assert_eq!(branch_col("clean-worktree-x", "clean-worktree-x"), col);
    assert_eq!(branch_col("sub", "main"), col);
    let name_col = |name: &str| {
        let l = row(name);
        l[..l.find(name).unwrap()].chars().count()
    };
    assert_eq!(
        name_col("acme/clean"),
        name_col("acme/dirty"),
        "a flag keeps its column's width:\n{screen}"
    );
    assert!(row("acme/dirty").contains("↑1 ±1"), "{}", row("acme/dirty"));
    let dirty = row("acme/dirty");
    assert_eq!(
        dirty[..dirty.find('↑').unwrap()].chars().count(),
        col + "clean-worktree-x".len() + 1,
        "badges one space after the widest branch:\n{screen}"
    );
    // at most half the width: the widest name is cut
    let narrow = render(&mut app, 30, 24);
    assert!(narrow.contains("clean-work…"), "{narrow}");
}

#[test]
fn the_selection_bar_spans_the_width_and_the_cursor_stays_in_view() {
    let (_sb, mut app) = setup("");
    app.act(Action::ExpandAll);
    let (w, h) = (60, 10);
    let t = draw(&mut app, w, h);
    let reversed = |y: u16| {
        t.backend().buffer()[(w - 1, y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    };
    assert!(reversed(1), "the cursor's row, to the last column");
    assert!(!reversed(2));
    // one page down, more rows below: the cursor's row is the last shown
    let body = app.page + 2;
    app.cursor = 2 * body;
    assert!(app.rows.len() > app.cursor + body, "enough rows below");
    draw(&mut app, w, h);
    assert_eq!(app.scroll, app.cursor + 1 - body);
    app.act(Action::Bottom);
    let t = draw(&mut app, w, h);
    assert_eq!(
        app.scroll,
        app.cursor + 1 - body,
        "the cursor on the last row"
    );
    let last = (1 + app.cursor - app.scroll) as u16;
    assert!(
        t.backend().buffer()[(w - 1, last)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
}

#[test]
fn the_widest_top_level_name_sets_the_column_and_a_repo_without_a_branch_keeps_it() {
    let (_sb, mut app) = setup_with("", |sb| {
        // unreadable: no branch badge, only an error
        let b = sb.path("dev/broken");
        std::fs::create_dir_all(&b).unwrap();
        git(&b, &["init", "-q"]);
        std::fs::write(b.join(".git/HEAD"), "nonsense\n").unwrap();
    });
    let screen = render(&mut app, 80, 12);
    let (_plain_sb, mut plain) = setup("");
    let plain_screen = render(&mut plain, 80, 12);
    assert!(
        plain_screen.contains("acme/feature   feat"),
        "{plain_screen}"
    );
    let col = |text: &str, mark: &str| {
        let l = screen.lines().find(|l| l.contains(text)).unwrap();
        l[..l.find(mark).unwrap()].chars().count()
    };
    assert_eq!(col("acme/dirty", "±1"), col("!!", "!!"), "{screen}");
}

#[test]
fn the_log_takes_a_third_and_shows_short_or_failed_output() {
    let (_sb, mut app) = setup("");
    app.show_log = true;
    let entry = |label: &str, ok: bool, lines: usize| argus_tui::app::LogEntry {
        at: jiff::Timestamp::now(),
        repo: "acme/clean".into(),
        label: label.into(),
        ok,
        output: (1..=lines).map(|n| format!("{label}-{n}\n")).collect(),
    };
    app.log = vec![
        entry("short", true, 3),
        entry("long", true, 4),
        entry("failed", false, 4),
    ];
    let screen = render(&mut app, 80, 30);
    for l in ["short-3", "failed-4"] {
        assert!(screen.contains(l), "{l}:\n{screen}");
    }
    assert!(
        !screen.contains("long-1"),
        "succeeded, 4 lines: hidden\n{screen}"
    );
    let top = screen
        .lines()
        .position(|l| l.contains("process log"))
        .unwrap();
    assert_eq!(
        30 - 1 - top,
        10,
        "a third of 30 rows, above the status line"
    );
}

#[test]
fn the_prompt_puts_the_cursor_after_the_text() {
    let (_sb, mut app) = setup("");
    app.on_key(key('z'));
    app.on_key(key('M'));
    app.on_key(key('z'));
    for c in "ab".chars() {
        app.on_key(key(c));
    }
    app.on_key(code(KeyCode::Left));
    let mut t = draw(&mut app, 80, 10);
    let screen: String = (0..80)
        .map(|x| t.backend().buffer()[(x, 9)].symbol().to_string())
        .collect();
    let label = screen.find("ab").unwrap();
    let pos = t.get_cursor_position().unwrap();
    assert_eq!((pos.x as usize, pos.y), (label + 1, 9), "{screen}");
}

#[test]
fn a_menu_highlights_only_its_bar_and_fits_its_lines() {
    let (_sb, mut app) = setup("");
    app.on_key(key('v'));
    app.on_key(code(KeyCode::Down));
    let t = draw(&mut app, 80, 30);
    let buf = t.backend().buffer().clone();
    let row_of = |text: &str| {
        (0..30u16)
            .find(|&y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .contains(text)
            })
            .unwrap()
    };
    let rev = |y: u16| {
        (0..80).any(|x| {
            buf[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        })
    };
    assert!(rev(row_of("show ignored")), "the bar, on the second item");
    assert!(!rev(row_of("show only")));
    let (top, bottom) = (row_of("╭ View"), row_of("╰"));
    let items = menu_items(&app).len() as u16;
    assert_eq!(
        bottom - top + 1,
        items + 1 + 2,
        "items, the footer and borders"
    );
}

#[test]
fn help_sits_inside_a_margin() {
    let (_sb, mut app) = setup("");
    app.on_key(key('?'));
    let screen = render(&mut app, 80, 20);
    let top = screen.lines().nth(1).unwrap();
    assert!(top.starts_with("  ╭ keys"), "{top:?}");
}

#[test]
fn unfolding_commit_files_mode_changes_staged_files_and_emptied_ranges() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        for f in ["gone.txt", "t.sh", "w.sh"] {
            write(&c, f, "x\n");
        }
        commit(&c, "base");
        git(&c, &["push", "-q", "origin", "main"]);
        git(&c, &["switch", "-q", "-c", "local"]);
        write(&c, "a.txt", "a\n");
        std::fs::remove_file(c.join("gone.txt")).unwrap();
        std::fs::set_permissions(
            c.join("t.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        git(&c, &["add", "-A"]);
        git(&c, &["commit", "-q", "-m", "mixed"]);
        git(&c, &["switch", "-q", "-c", "pushed", "main"]);
        commit(&c, "soon pushed");
        git(&c, &["switch", "-q", "main"]);
        // in the worktree: a mode change only, and a staged-only file
        git(&c, &["update-index", "--chmod=+x", "w.sh"]);
        git(&c, &["reset", "-q", "--", "w.sh"]);
        std::fs::set_permissions(
            c.join("w.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        write(&c, "s.txt", "s\n");
        git(&c, &["add", "s.txt"]);
    });
    use argus_core::badges::Role::*;
    let c = sb.path("dev/clean");
    let short = git(&c, &["rev-parse", "--short=7", "local"]);
    cursor_on_detail(&mut app, "clean", "    local");
    app.act(Action::ToggleExpand);
    cursor_on_detail(&mut app, "clean", &format!("      {}", short.trim()));
    app.act(Action::ToggleExpand);
    let file_role = |app: &App, path: &str| {
        app.rows
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. }
                    if line.parts.last().is_some_and(|p| p.1 == format!(" {path}")) =>
                {
                    Some(line.parts[line.parts.len() - 2].clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {path}: {:?}", detail_texts(app)))
    };
    assert_eq!(file_role(&app, "a.txt"), (Some(Ahead), "  A".into()));
    assert_eq!(file_role(&app, "gone.txt"), (Some(Behind), "  D".into()));
    assert_eq!(file_role(&app, "t.sh"), (Some(Unstaged), "  M".into()));
    cursor_on_detail(&mut app, "clean", "          M t.sh");
    app.act(Action::ToggleExpand);
    assert_eq!(
        below(&app, "          M t.sh", 10),
        ["no change in content"]
    );

    to_file(&mut app, "w.sh");
    app.act(Action::ToggleExpand);
    let t = trimmed(&app);
    let at = t.iter().position(|l| l.ends_with("w.sh")).unwrap();
    assert_eq!(t[at + 1], "no change in content");
    to_file(&mut app, "s.txt");
    app.act(Action::ToggleExpand);
    let t = trimmed(&app);
    let at = t.iter().position(|l| l.ends_with("s.txt")).unwrap();
    assert_eq!(t[at - 1], "staged (1)", "staged only: under staged");
    assert_eq!(t[at + 1..at + 3], ["@@ -0,0 +1 @@", "+s"]);

    // pushed since the status was read: its range is empty when unfolded
    git(&c, &["push", "-q", "origin", "pushed"]);
    cursor_on_detail(&mut app, "clean", "    pushed");
    app.act(Action::ToggleExpand);
    assert_eq!(below(&app, "    pushed", 4), ["no commits"]);
}

#[test]
fn fix_results_are_logged_failures_reported_and_the_log_capped() {
    use argus_core::fix::{Applied, Fix, FixKind};
    use argus_core::ops::OpOutcome;
    let (sb, mut app) = setup("");
    let c = sb.path("dev/clean");
    let done = |success: bool| Msg::FixDone {
        path: c.clone(),
        applied: Ok(Some(Applied {
            fix: Fix {
                kind: FixKind::Push,
                op: Op::Push,
                reason: "1 unpushed".into(),
                blocked: None,
            },
            outcome: OpOutcome {
                success,
                code: Some(if success { 0 } else { 1 }),
                output: "out".into(),
                duration: std::time::Duration::ZERO,
            },
        })),
    };
    app.on_msg(done(true));
    assert!(!app.message.as_ref().unwrap().1, "{:?}", app.message);
    assert!(app.log.last().unwrap().ok);
    app.on_msg(done(false));
    assert_eq!(
        app.message.clone().unwrap(),
        ("acme/clean: push failed ($ for log)".to_string(), true)
    );
    // at most 1,000 entries: past that, the oldest 100 go
    let entry = app.log[0].clone();
    app.log = vec![entry; 999];
    app.on_msg(done(true));
    assert_eq!(app.log.len(), 1000);
    app.on_msg(done(true));
    assert_eq!(app.log.len(), 901);
}

#[test]
fn the_filter_matches_the_identity_too_and_a_status_refreshes_unfolded_commits() {
    let (sb, mut app) = setup("");
    app.on_key(key('/'));
    for c in "github.com/acme/f".chars() {
        app.on_key(key(c));
    }
    assert_eq!(names(&app), vec!["acme/feature"], "by key, not name");
    app.on_key(code(KeyCode::Esc));
    app.on_key(code(KeyCode::Esc));

    let f = sb.path("dev/feature");
    commit(&f, "one");
    app.ws.refresh(&f);
    app.rebuild();
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit(s)");
    app.act(Action::ToggleExpand);
    assert_eq!(below(&app, "issue     1 unpushed", 0).len(), 1);
    commit(&f, "two");
    app.ws.refresh(&f);
    app.on_msg(Msg::Status(
        f.clone(),
        Box::new(Ok(app.ws.get(&f).unwrap().status().unwrap().clone())),
    ));
    assert_eq!(
        below(&app, "issue     2 unpushed", 0).len(),
        2,
        "unfolded afresh: {:?}",
        detail_texts(&app)
    );
}

#[test]
fn several_repos_untracked_files_go_to_each_files_directory() {
    let (_sb, mut app) = setup_with("", |sb| {
        for n in ["clean", "feature"] {
            write(&sb.path(&format!("dev/{n}")), "logs/x.log", "x\n");
        }
    });
    let pick = |app: &mut App, name: &str| {
        while current(app) != name {
            app.act(Action::Down);
        }
        app.act(Action::ToggleMark);
    };
    app.act(Action::Top);
    pick(&mut app, "acme/feature");
    app.act(Action::Top);
    pick(&mut app, "acme/clean");
    app.act(Action::GitignoreMenu);
    let d = menu_items(&app)
        .into_iter()
        .find(|l| l.starts_with("d "))
        .unwrap();
    assert!(d.ends_with("to each file's directory's .gitignore"), "{d}");
}

#[test]
fn when_the_diff_under_the_cursor_goes_the_cursor_moves_within_its_section() {
    let (sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    // on the unstaged hunk's line, then that hunk goes (the staged one stays)
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some("both") && line.parts.iter().any(|p| p.1 == "+three")))
        .unwrap();
    let d = sb.path("dev/dirty");
    git(&d, &["checkout", "--", "both"]);
    app.ws.refresh(&d);
    app.on_msg(Msg::Status(
        d.clone(),
        Box::new(Ok(app.ws.get(&d).unwrap().status().unwrap().clone())),
    ));
    // the file has left the section: the one before it there (none is
    // after), not the file's line under staged
    match &app.rows[app.cursor] {
        Row::Detail { line, .. } => {
            assert_eq!(line.file.as_deref(), Some("README"));
            assert_eq!(line.section, Some(argus_tui::details::Section::Unstaged));
            assert!(line.diff.is_none(), "a file's own line: {:?}", line.parts);
        }
        r => panic!("{r:?}"),
    }
    // a hunk's lines gone from under the cursor: its header
    write(&d, "README", "hello there\nand more\n");
    app.ws.refresh(&d);
    app.rebuild();
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    app.cursor += 2;
    assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. } if line.diff.is_some()));
    fold_hunk_under_cursor(&mut app);
    match &app.rows[app.cursor] {
        Row::Detail { line, .. } => {
            assert_eq!(line.file.as_deref(), Some("README"));
            assert!(line.diff.as_ref().is_some_and(|d| d.line.is_none()));
        }
        r => panic!("{r:?}"),
    }
}

/// An action in a submodule or worktree changes its parent's status too
/// (the submodule's commit, the shared branches): parents are refreshed with it.
#[test]
fn a_childs_parents_are_refreshed_with_it() {
    let (sb, mut app) = setup("");
    let [clean, dirty, feature] =
        ["clean", "dirty", "feature"].map(|n| sb.path(&format!("dev/{n}")));
    // feature inside dirty inside clean, as nested submodules are
    for e in &mut app.ws.entries {
        e.parent = match e.path.file_name().unwrap().to_str().unwrap() {
            "dirty" => Some(clean.clone()),
            "feature" => Some(dirty.clone()),
            _ => None,
        };
    }
    let finished = |path: &std::path::PathBuf| Msg::JobFinished {
        path: path.clone(),
        label: "fetch".into(),
        op: Op::Fetch,
        outcome: argus_core::ops::OpOutcome {
            success: true,
            code: Some(0),
            output: String::new(),
            duration: Default::default(),
        },
    };
    assert_eq!(
        app.on_msg(finished(&feature)),
        [Effect::Refresh(vec![
            feature.clone(),
            dirty.clone(),
            clean.clone()
        ])]
    );
    assert_eq!(
        app.on_msg(Msg::FixDone {
            path: dirty.clone(),
            applied: Ok(None),
        }),
        [Effect::Refresh(vec![dirty.clone(), clean.clone()])]
    );
    // inverse: a repo with no parent refreshes alone
    assert_eq!(
        app.on_msg(finished(&clean)),
        [Effect::Refresh(vec![clean.clone()])]
    );
    // back from a shell or git UI in the repo
    let current = app.current().unwrap().path.clone();
    let expect = match current.file_name().unwrap().to_str().unwrap() {
        "feature" => vec![feature.clone(), dirty.clone(), clean.clone()],
        "dirty" => vec![dirty.clone(), clean.clone()],
        _ => vec![clean.clone()],
    };
    assert_eq!(
        app.on_msg(Msg::Suspended(Ok(()))),
        [Effect::Refresh(expect)]
    );
}

/// A repo nested untracked in another's work tree unfolds into its own
/// details, whose lines act on it; it isn't listed itself.
#[test]
fn a_nested_repo_unfolds_into_its_own_details_and_acts_there() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        for rel in ["nested", "tools/deep"] {
            let n = c.join(rel);
            std::fs::create_dir_all(&n).unwrap();
            git(&n, &["init", "-q"]);
            write(&n, "a", "1\n");
            commit(&n, "a");
            write(&n, "a", "2\n");
        }
        // listed, and named like the first: not what that unfolds into
        let other = sb.init("dev/a/nested");
        commit(&other, "o");
    });
    let listed = names(&app);
    cursor_on_detail(&mut app, "clean", "    nested");
    app.act(Action::ToggleExpand);
    let under = below(&app, "    nested", 4);
    assert!(under.iter().any(|l| l == "M a"), "{under:?}");
    let path_of = |under: &[String]| under.iter().find(|l| l.starts_with("path")).cloned();
    assert!(
        path_of(&under).unwrap().ends_with("/dev/clean/nested"),
        "{under:?}"
    );
    assert_eq!(names(&app), listed, "not a row of its own");
    // its file's line acts in it
    let nested = sb.path("dev/clean/nested").canonicalize().unwrap();
    app.cursor = app
        .rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { repo, line }
                if app.ws.entries[*repo].path == nested && line.file.as_deref() == Some("a"))
        })
        .unwrap();
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => {
            assert_eq!(j[0].path, nested);
            assert_eq!(
                j[0].op,
                Op::AddTracked {
                    paths: vec!["a".into()]
                }
            );
        }
        other => panic!("{other:?}"),
    }
    // and what changes there is refreshed with the repo it's in
    let clean = sb.path("dev/clean").canonicalize().unwrap();
    assert_eq!(
        app.ws.with_parents(&nested),
        [nested.clone(), clean.clone()]
    );
    // Tab on one of its lines folds it, the repo it's in staying open
    app.cursor = app
        .rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { repo, line }
                if app.ws.entries[*repo].path == nested && line.file.is_none() && line.expand.is_none())
        })
        .unwrap();
    app.act(Action::ToggleExpand);
    assert_eq!(current(&app), "acme/clean");
    assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. }
        if matches!(line.expand, Some(argus_tui::details::Expand::Repo(_)))));
    assert_eq!(below(&app, "    nested", 4), Vec::<String>::new());
    assert!(!app.expanded.contains(&nested));
    app.act(Action::ToggleExpand);
    assert!(!below(&app, "    nested", 4).is_empty());
    // one inside an untracked directory unfolds from there
    cursor_on_detail(&mut app, "clean", "    tools/");
    app.act(Action::ToggleExpand);
    cursor_on_detail(&mut app, "clean", "      tools/deep");
    app.act(Action::ToggleExpand);
    let under = below(&app, "      tools/deep", 6);
    assert!(under.iter().any(|l| l == "M a"), "{under:?}");
    // its own, with the other still unfolded
    assert!(
        path_of(&under).unwrap().ends_with("/dev/clean/tools/deep"),
        "{under:?}"
    );
    // folded again, its lines go (the entry stays, unlisted)
    cursor_on_detail(&mut app, "clean", "    nested");
    app.act(Action::ToggleExpand);
    assert_eq!(below(&app, "    nested", 4), Vec::<String>::new());
}

/// Clearing says what's left: tags that patterns (or tracking) still give.
#[test]
fn clearing_tags_says_what_patterns_still_give() {
    use argus_core::rules::Tag;
    let (_sb, mut app) = setup("tags.favourite.repos = ['acme/c*']");
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    let at = |app: &App, n: &str| {
        app.ws
            .entries
            .iter()
            .position(|e| e.path.ends_with(n))
            .unwrap()
    };
    let (clean, dirty) = (at(&app, "clean"), at(&app, "dirty"));
    app.ws.set_tag(&[clean, dirty], Tag::Ignore, true).unwrap();
    app.marked = [clean, dirty]
        .map(|i| app.ws.entries[i].path.clone())
        .into();
    app.act(Action::ClearTags);
    app.on_key(key('y'));
    assert_eq!(
        msg(&app),
        "tags cleared for 2 repos; still: acme/clean favourite (acme/c*)"
    );
    // inverse: nothing left to mention
    app.marked.clear();
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if *i == dirty))
        .unwrap();
    app.ws.set_tag(&[dirty], Tag::Label(0), true).unwrap();
    app.act(Action::ClearTags);
    assert_eq!(msg(&app), "tags cleared");
    // the menu item
    app.act(Action::Top);
    app.on_key(key('t'));
    assert!(
        menu_items(&app).iter().any(|l| l == "c clear tags"),
        "{:?}",
        menu_items(&app)
    );
}

/// The flag column: favourite, then pull (the commonest, last); names stay
/// aligned however many flags a row has, in every icon set.
#[test]
fn rows_show_the_pull_tag_and_names_stay_aligned() {
    let (_sb, mut app) = setup(
        "tags.favourite.repos = ['acme/clean']\ntags.pull.repos = ['acme/clean', 'acme/dirty']",
    );
    for (icons, set) in [
        (argus_core::icons::UNICODE, "unicode"),
        (argus_core::icons::NERD, "nerd"),
        (argus_core::icons::ASCII, "ascii"),
    ] {
        // tags' icons follow the configured set
        app.ws.config.ui.icons = set.into();
        let fav = app.ws.config.tag_icon("favourite", set);
        let pull = icons.pull.to_string();
        let screen = render_with(&mut app, 100, 8, icons);
        let row = |name: &str| {
            screen
                .lines()
                .find(|l| l.contains(name))
                .unwrap_or_else(|| panic!("{name} in {screen}"))
                .to_string()
        };
        let (clean, dirty, feature) = (row("acme/clean"), row("acme/dirty"), row("acme/feature"));
        assert!(clean.contains(&format!("{fav}{pull}")), "{clean}");
        assert!(dirty.contains(&pull) && !dirty.contains(&fav), "{dirty}");
        assert!(!feature.contains(&pull), "{feature}");
        let col =
            |l: &str, n: &str| unicode_width::UnicodeWidthStr::width(&l[..l.find(n).unwrap()]);
        assert_eq!(col(&clean, "acme/"), col(&feature, "acme/"), "{screen}");
        assert_eq!(col(&dirty, "acme/"), col(&feature, "acme/"), "{screen}");
    }
    // the column is as wide as the row with the most flags: all of them
    // show (favourite, new, snoozed, pull for acme/clean), names align
    let clean = app
        .ws
        .entries
        .iter()
        .position(|e| e.path.ends_with("clean"))
        .unwrap();
    let k = app.ws.entries[clean].key.clone();
    app.ws.state.new.insert(k.clone());
    app.ws.state.snoozed.insert(
        k,
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    app.ws.resolve_flags();
    app.ws.config.ui.icons = "unicode".into();
    app.view.show_hidden = true;
    app.rebuild();
    let i = argus_core::icons::UNICODE;
    let screen = render_with(&mut app, 100, 12, i.clone());
    let all = format!("{}{}{}{}", "◆", i.new, i.snoozed, i.pull);
    assert!(screen.contains(&all), "{screen}");
    let start = |l: &str, s: &str| unicode_width::UnicodeWidthStr::width(&l[..l.find(s).unwrap()]);
    let names: Vec<usize> = screen
        .lines()
        .filter(|l| l.contains("acme/"))
        .map(|l| start(l, "acme/"))
        .collect();
    assert_eq!(names.len(), 3, "{screen}");
    assert!(names.iter().all(|&s| s == names[0]), "{screen}");
    // hidden repos count too, so the column doesn't move as they come and go
    app.view.show_hidden = false;
    app.rebuild();
    let hiding = render_with(&mut app, 100, 12, i.clone());
    let dirty = hiding
        .lines()
        .find(|l| l.contains("acme/dirty"))
        .expect("listed");
    assert_eq!(start(dirty, "acme/"), names[0], "{hiding}");
    app.view.show_hidden = true;
    app.rebuild();
    // details sit two in from the names, as with the narrower column
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(x) if *x == clean))
        .unwrap();
    app.act(Action::ToggleExpand);
    let screen = render_with(&mut app, 100, 30, i);
    let path = screen
        .lines()
        .find(|l| l.trim_start().starts_with("path"))
        .expect("details");
    assert_eq!(start(path, "path"), names[0] + 2, "{screen}");
}

/// Staging a nested repo's line says why nothing happens.
#[test]
fn staging_a_nested_repo_explains_itself() {
    let (_sb, mut app) = setup_with("", |sb| {
        sb.init("dev/clean/nested");
    });
    cursor_on_detail(&mut app, "clean", "    nested");
    assert!(app.act(Action::StageFiles).is_empty());
    let msg = app.message.as_ref().unwrap().0.clone();
    assert!(msg.contains("nested") && msg.contains("embedded"), "{msg}");
    // inverse: unstaging it: it isn't staged
    assert!(app.act(Action::UnstageFiles).is_empty());
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "that's not staged: s stages it"
    );
}

/// `t o` tags the selected repo's owner as a whole, with the same keys.
#[test]
fn an_owner_is_tagged_from_the_tag_menu() {
    let (_sb, mut app) = setup("");
    app.on_key(key('t'));
    app.on_key(key('o'));
    match &app.mode {
        Mode::Menu { menu, .. } => {
            assert!(menu.title.contains("github.com/acme"), "{}", menu.title)
        }
        _ => panic!("no menu"),
    }
    assert!(
        menu_items(&app).iter().any(|l| l == "f tag favourite"),
        "{:?}",
        menu_items(&app)
    );
    let before = app.ws.config.clone();
    let fx = app.on_key(key('f'));
    assert!(fx.contains(&Effect::SaveTags(Box::new(before))), "{fx:?}");
    assert_eq!(app.ws.config.tags["favourite"].repos, ["github.com/acme/*"]);
    assert!(app.ws.entries.iter().all(|e| e.flags.has("favourite")));
    app.on_key(key('t'));
    app.on_key(key('o'));
    assert!(
        menu_items(&app).iter().any(|l| l == "f untag favourite"),
        "{:?}",
        menu_items(&app)
    );
    app.on_key(ctrl('g'));
    // a repo without an identity has no owner: not offered
    let (_sb, mut app) = setup_with("", |sb| {
        sb.init("dev/zlocal");
    });
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("zlocal")))
        .unwrap();
    assert!(app.current().unwrap().id.is_none());
    app.on_key(key('t'));
    let items = menu_items(&app);
    assert!(
        !items.iter().any(|l| l.starts_with("o ")),
        "not offered: {items:?}"
    );
    assert!(items.iter().any(|l| l.starts_with("f ")), "{items:?}");
}

/// `c f`: what's staged as a fixup of a commit picked from HEAD's history,
/// starting at HEAD; offered only when something is staged.
#[test]
fn a_fixup_is_made_of_a_picked_commit() {
    let (sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        git(&d, &["add", "README"]);
    });
    let dirty = sb.path("dev/dirty");
    let head = git(&dirty, &["rev-parse", "HEAD"]).trim().to_string();
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("dirty")))
        .unwrap();
    app.on_key(key('c'));
    assert!(
        menu_items(&app).iter().any(|l| l.starts_with("f fixup")),
        "{:?}",
        menu_items(&app)
    );
    app.on_key(key('f'));
    let (title, at) = match &app.mode {
        Mode::Menu { menu, cursor } => (menu.title.clone(), *cursor),
        _ => panic!("no commit list"),
    };
    assert!(
        title.contains("Fixup") && title.contains("acme/dirty"),
        "{title}"
    );
    assert_eq!(at, 0, "starts at HEAD");
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::Fixup {
                commit: head.clone(),
                squash: None,
                instant: false,
            }
        ),
        other => panic!("{other:?}"),
    }
    // inverse: nothing staged, not offered
    app.running.clear();
    let clean = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("clean")))
        .unwrap();
    app.cursor = clean;
    app.on_key(key('c'));
    assert!(
        !menu_items(&app).iter().any(|l| l.starts_with("f fixup")),
        "{:?}",
        menu_items(&app)
    );
}

/// `b b` checks out a branch picked from a list (local ones, then remote
/// ones without a local copy); `b -` the previous one.
#[test]
fn branches_are_checked_out_from_a_list_or_the_previous_one() {
    let (_sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        git(&f, &["switch", "-q", "-c", "topic"]);
        commit(&f, "topic");
        git(&f, &["push", "-q", "-u", "origin", "topic"]);
        git(&f, &["switch", "-q", "feat"]);
        git(&f, &["branch", "-q", "-D", "topic"]);
    });
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("feature")))
        .unwrap();
    app.on_key(key('b'));
    let items = menu_items(&app);
    assert!(
        items.iter().any(|l| l.starts_with("b checkout")),
        "{items:?}"
    );
    assert!(
        items
            .iter()
            .any(|l| l.starts_with("- checkout the previous")),
        "{items:?}"
    );
    // `b` again: pick by typing
    app.on_key(key('b'));
    let shown = |app: &App| match &app.mode {
        Mode::Pick {
            labels,
            shown,
            title,
            ..
        } => {
            assert!(title.contains("acme/feature"), "{title}");
            shown.iter().map(|&i| labels[i].clone()).collect::<Vec<_>>()
        }
        _ => panic!("no picker"),
    };
    assert_eq!(
        shown(&app),
        ["main", "origin/topic (creates a local branch tracking it)"],
        "all to start with; not the current branch"
    );
    for c in "tpc".chars() {
        app.on_key(key(c));
    }
    assert_eq!(
        shown(&app),
        ["origin/topic (creates a local branch tracking it)"]
    );
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::SwitchTrack {
                remote_branch: "origin/topic".into()
            }
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(app.mode, Mode::Normal));
    // C-n moves down the matches, C-g cancels without a job
    app.running.clear();
    app.on_key(key('b'));
    app.on_key(key('b'));
    app.on_key(ctrl('n'));
    assert!(matches!(&app.mode, Mode::Pick { cursor: 1, .. }));
    let at = |app: &App| match &app.mode {
        Mode::Pick { cursor, .. } => *cursor,
        _ => panic!("no picker"),
    };
    // the arrows too, kept to the list
    for (k, want) in [
        (code(KeyCode::Down), 1),
        (code(KeyCode::Up), 0),
        (code(KeyCode::Up), 0),
        (code(KeyCode::Down), 1),
        (ctrl('p'), 0),
        (ctrl('n'), 1),
        (ctrl('n'), 1),
    ] {
        app.on_key(k);
        assert_eq!(at(&app), want, "{k:?}");
    }
    // n and p alone are typed, narrowing
    app.on_key(key('p'));
    assert_eq!(
        shown(&app),
        ["origin/topic (creates a local branch tracking it)"]
    );
    app.on_key(code(KeyCode::Backspace));
    app.on_key(key('n'));
    assert!(matches!(&app.mode, Mode::Pick { editor, .. } if editor.text() == "n"));
    assert_eq!(
        shown(&app),
        ["main", "origin/topic (creates a local branch tracking it)"]
    );
    assert!(app.on_key(ctrl('g')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    app.running.clear();
    app.on_key(key('b'));
    match app.on_key(key('-')).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j[0].op, Op::Switch { branch: "-".into() }),
        other => panic!("{other:?}"),
    }
}

/// `b c` creates a branch at HEAD and checks it out, on any branch, not
/// only a detached HEAD (the branch fix).
#[test]
fn a_branch_is_created_from_any_head() {
    let (_sb, mut app) = setup("");
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("clean")))
        .unwrap();
    app.on_key(key('b'));
    assert!(
        menu_items(&app)
            .iter()
            .any(|l| l == "c create a branch at HEAD and check it out…"),
        "{:?}",
        menu_items(&app)
    );
    app.on_key(key('c'));
    assert!(matches!(app.mode, Mode::Input { .. }));
    for c in "wip".chars() {
        app.on_key(key(c));
    }
    match app.on_key(code(KeyCode::Enter)).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j[0].op, Op::CreateBranch { name: "wip".into() }),
        other => panic!("{other:?}"),
    }
}

/// `b s` / `b S`: spin-off and spin-out, asking for the new branch's name.
#[test]
fn spin_off_and_spin_out_ask_for_the_name() {
    let (_sb, mut app) = setup("");
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("clean")))
        .unwrap();
    for (k, stay) in [('s', false), ('S', true)] {
        app.on_key(key('b'));
        app.on_key(key(k));
        assert!(matches!(app.mode, Mode::Input { .. }), "{k}");
        for c in "feat".chars() {
            app.on_key(key(c));
        }
        match app.on_key(code(KeyCode::Enter)).as_slice() {
            [Effect::Jobs(j)] => assert_eq!(
                j[0].op,
                Op::SpinOff {
                    name: "feat".into(),
                    stay
                }
            ),
            other => panic!("{k}: {other:?}"),
        }
        app.running.clear();
    }
}

/// `c F` instant fixup, `c s` squash (asking for its message), `c S`
/// instant squash: the same commit list as `c f`.
#[test]
fn instant_fixups_and_squashes_from_the_commit_menu() {
    let (sb, mut app) = setup_with("", |sb| {
        git(&sb.path("dev/dirty"), &["add", "README"]);
    });
    let head = git(&sb.path("dev/dirty"), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let to_dirty = |app: &mut App| {
        app.cursor = app
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("dirty")))
            .unwrap();
    };
    let title = |app: &App| match &app.mode {
        Mode::Menu { menu, .. } => menu.title.clone(),
        _ => panic!("no commit list"),
    };
    for (k, squash, instant, want) in [
        ('F', false, true, "Instant fixup"),
        ('s', true, false, "Squash"),
        ('S', true, true, "Instant squash"),
    ] {
        to_dirty(&mut app);
        app.on_key(key('c'));
        app.on_key(key(k));
        assert!(title(&app).starts_with(want), "{k}: {}", title(&app));
        let fx = app.on_key(code(KeyCode::Enter));
        let fx = if squash {
            assert!(
                fx.is_empty() && matches!(app.mode, Mode::Input { .. }),
                "{k}: asks"
            );
            for c in "why".chars() {
                app.on_key(key(c));
            }
            app.on_key(code(KeyCode::Enter))
        } else {
            fx
        };
        match fx.as_slice() {
            [Effect::Jobs(j)] => assert_eq!(
                j[0].op,
                Op::Fixup {
                    commit: head.clone(),
                    squash: squash.then(|| "why".to_string()),
                    instant,
                },
                "{k}"
            ),
            other => panic!("{k}: {other:?}"),
        }
        app.running.clear();
    }
}

/// In a diff, `s`/`u`/`K` act on the hunk under the cursor (its header or
/// a context line), or on the one added or removed line under it.
#[test]
fn hunks_and_lines_are_staged_unstaged_and_discarded() {
    use argus_core::diff::LineKind;
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    let at = |app: &App, f: &dyn Fn(&argus_tui::details::DiffRef) -> bool| {
        app.rows
            .iter()
            .position(|r| matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some("README") && line.diff.as_deref().is_some_and(f)))
            .expect("row")
    };
    let apply = |fx: Vec<Effect>| match fx.as_slice() {
        [Effect::Jobs(j)] => match &j[0].op {
            Op::Apply {
                patch,
                index,
                reverse,
                what,
            } => (patch.clone(), *index, *reverse, what.clone()),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    // the hunk's header: the whole hunk
    app.cursor = at(&app, &|d| d.hunk.is_some() && d.line.is_none());
    let (patch, index, reverse, what) = apply(app.act(Action::StageFiles));
    assert!((index, reverse) == (true, false), "{patch}");
    assert!(patch.contains("-hello world\n+hello there\n"), "{patch}");
    assert_eq!(what, "stage a hunk of README");
    app.running.clear();
    // on one of its lines, nothing marked: still the whole hunk
    app.cursor = at(&app, &|d| {
        d.line.as_ref().is_some_and(|l| l.kind == LineKind::Add)
    });
    let (patch, _, _, what) = apply(app.act(Action::StageFiles));
    assert!(patch.contains("-hello world\n+hello there\n"), "{patch}");
    assert_eq!(what, "stage a hunk of README");
    app.running.clear();
    // marked, the added line alone: the removed one stays as context
    app.on_key(key(' '));
    let (patch, _, _, _) = apply(app.act(Action::StageFiles));
    assert!(patch.contains(" hello world\n+hello there\n"), "{patch}");
    app.running.clear();
    app.marked_lines.clear();
    // discarding asks first
    app.cursor = at(&app, &|d| d.hunk.is_some() && d.line.is_none());
    assert!(app.act(Action::DiscardFiles).is_empty());
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.contains("hunk of README"))
    );
    let (_, index, reverse, _) = apply(app.on_key(key('y')));
    assert_eq!(
        (index, reverse),
        (false, true),
        "from the work tree, backwards"
    );
    app.running.clear();
    // on one of its lines, nothing marked: still the whole hunk that's
    // discarded, as for staging
    app.cursor = at(&app, &|d| {
        d.line.as_ref().is_some_and(|l| l.kind == LineKind::Add)
    });
    assert!(app.act(Action::DiscardFiles).is_empty());
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question == "Discard this hunk of README? It's lost.")
    );
    let (patch, index, reverse, what) = apply(app.on_key(key('y')));
    assert_eq!((index, reverse), (false, true));
    assert_eq!(what, "discard a hunk of README");
    assert!(patch.contains("-hello world\n+hello there\n"), "{patch}");
    app.running.clear();
    // marked, only that line goes: the removed one is put back as it was
    app.on_key(key(' '));
    assert!(app.act(Action::DiscardFiles).is_empty());
    let (patch, _, reverse, _) = apply(app.on_key(key('y')));
    assert!(reverse && !patch.contains("-hello world"), "{patch}");
    assert!(patch.contains("+hello there"), "{patch}");
    app.running.clear();
    app.marked_lines.clear();
    // (space moved on, to the blank line after the diff: nothing by
    // itself, so nothing to take)
    assert!(app.act(Action::UnstageFiles).is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("move to a file"));
    // unstaging an unstaged hunk says what to do instead
    app.cursor = diff_row(&app, "hello there");
    assert!(app.act(Action::UnstageFiles).is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("not staged"));
    // and staging or discarding a staged one
    to_staged(&mut app, "both");
    app.act(Action::ToggleExpand);
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.file.as_deref() == Some("both")
            && line.diff.as_deref().is_some_and(|d| d.hunk.is_some() && d.side == argus_core::diff::Side::Staged)))
        .unwrap();
    assert!(app.act(Action::StageFiles).is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("staged already"));
    assert!(app.act(Action::DiscardFiles).is_empty());
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .contains("u unstages it, then K")
    );
    assert!(!matches!(app.mode, Mode::Confirm { .. }));
}

/// Space marks added or removed lines; `s`/`u`/`K` then act on just those,
/// across hunks, in one patch per file.
#[test]
fn marked_lines_are_staged_together() {
    marked_lines_case(Then::Stage)
}

/// Marks and a region point into diffs as they were read: a rescan or a
/// new status of their repo, which reads them again, drops them.
#[test]
fn marks_and_a_region_end_when_their_diff_is_read_again() {
    marked_lines_case(Then::Rescan);
    marked_lines_case(Then::Status("dirty"));
}

/// Inverse: another repo's status leaves them.
#[test]
fn marks_and_a_region_outlive_another_repos_status() {
    marked_lines_case(Then::Status("clean"));
}

enum Then {
    Stage,
    Rescan,
    Status(&'static str),
}

fn marked_lines_case(then: Then) {
    let (_sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        write(&d, "f", &lines);
        commit(&d, "f");
        let changed = lines
            .replace("line 3\n", "line three\n")
            .replace("line 28\n", "line 28\nadded\n");
        write(&d, "f", &changed);
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let row_of = |app: &App, text: &str| {
        app.rows
            .iter()
            .position(|r| matches!(r, Row::Detail { line, .. }
                if line.diff.as_deref().and_then(|d| d.line.as_ref()).is_some_and(|l| l.text == text)
                    && line.diff.as_deref().is_some_and(|d| d.side == argus_core::diff::Side::Unstaged)))
            .unwrap_or_else(|| panic!("{text}"))
    };
    for text in ["line three", "added"] {
        app.cursor = row_of(&app, text);
        app.on_key(key(' '));
    }
    assert_eq!(app.marked_lines.len(), 2);
    match then {
        Then::Stage => {}
        Then::Rescan => {
            app.on_key(ctrl(' '));
            assert!(app.region.is_some());
            let entries = app.ws.entries.clone();
            app.on_msg(Msg::Scanned {
                entries,
                truncated: vec![],
            });
            assert!(app.marked_lines.is_empty() && app.region.is_none());
            return;
        }
        Then::Status(name) => {
            app.on_key(ctrl(' '));
            assert!(app.region.is_some());
            let p = app
                .ws
                .entries
                .iter()
                .find(|e| e.path.ends_with(name))
                .unwrap();
            let (path, st) = (p.path.clone(), p.status().unwrap().clone());
            app.on_msg(Msg::Status(path, Box::new(Ok(st))));
            assert_eq!(
                (app.marked_lines.is_empty(), app.region.is_none()),
                (name == "dirty", name == "dirty")
            );
            return;
        }
    }
    let screen = render(&mut app, 100, 60);
    let marked = screen
        .lines()
        .filter(|l| l.contains(&*argus_core::icons::UNICODE.marked))
        .count();
    assert!(marked >= 2, "{screen}");
    // u: they aren't staged
    assert!(app.act(Action::UnstageFiles).is_empty());
    assert!(app.message.as_ref().unwrap().0.contains("aren't staged"));
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => match &j[0].op {
            Op::Apply {
                patch,
                what,
                index: true,
                reverse: false,
            } => {
                assert_eq!(what, "stage 2 lines of f");
                assert!(
                    patch.contains("+line three") && patch.contains("+added"),
                    "{patch}"
                );
                assert!(
                    patch.contains(" line 3\n"),
                    "line 3's removal left out: {patch}"
                );
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert!(app.marked_lines.is_empty(), "acted on, unmarked");
}

/// `dirty` with `f` changed in two hunks: line 3 replaced (a removed and
/// an added line), and a line added after line 28.
fn two_hunks(sb: &Sandbox) {
    let d = sb.path("dev/dirty");
    let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    write(&d, "f", &lines);
    commit(&d, "f");
    let changed = lines
        .replace("line 3\n", "line three\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&d, "f", &changed);
}

/// The row of `f`'s diff with this text: a line's, or a hunk's header
/// (`@@ -1,6`: how it starts).
fn shown_row(app: &App, text: &str) -> usize {
    let found = app.rows.iter().position(|r| {
        let Row::Detail { line, .. } = r else {
            return false;
        };
        let shown: String = line.parts.iter().map(|p| p.1.as_str()).collect();
        line.diff.is_some() && shown.trim_start().starts_with(text.trim_start())
    });
    found.unwrap_or_else(|| panic!("{text}: {:?}", trimmed(app)))
}

/// How the rows showing `text` are marked: `M` wholly, `p` partly, `-` not.
fn marks_of(app: &mut App, texts: &[&str]) -> String {
    let screen = render(app, 100, 60);
    let icons = argus_core::icons::UNICODE;
    texts
        .iter()
        .map(|text| {
            let line = screen
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text}: {screen}"));
            if line.contains(&*icons.marked) {
                'M'
            } else if line.contains(&*icons.partly_marked) {
                'p'
            } else {
                '-'
            }
        })
        .collect()
}

/// A file's mark is its hunks' and lines': marking the file shows them
/// all marked, a line or hunk taken out of it leaves the file (and the
/// hunk) partly marked, and space on something partly marked unmarks
/// it, then marks it all.
#[test]
fn marks_of_a_file_its_hunks_and_its_lines_are_one() {
    use argus_tui::details::Section;
    let (_sb, mut app) = setup_with("", two_hunks);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let file = app.cursor;
    // the file, its two hunks, their three changed lines, and one unchanged
    let rows = [
        "M f",
        "@@ -1,6",
        "-line 3",
        "+line three",
        "@@ -26,5",
        "+added",
        " line 29",
    ];
    let dirty = app.current().unwrap().path.clone();
    assert_eq!(marks_of(&mut app, &rows), "-------");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "MMMMMM-");
    assert_eq!(
        app.marked_files[&dirty],
        [(Section::Unstaged, "f".to_string())].into()
    );
    assert!(app.marked_lines.is_empty());
    // a line out of it: its hunk has none left, the file is partly marked
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "pMMM---");
    assert!(app.marked_files.is_empty());
    assert_eq!(app.marked_lines.len(), 2);
    // a line out of the other hunk: that's partly marked too
    app.cursor = shown_row(&app, "+line three");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "ppM----");
    // (and back into it, its other line marked or not)
    app.cursor = shown_row(&app, "+line three");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "pMMM---");
    app.cursor = shown_row(&app, "+line three");
    app.on_key(key(' '));
    // space on a hunk partly marked unmarks it; again marks it all
    let hunk = shown_row(&app, "@@ -1,6");
    app.cursor = hunk;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    assert_eq!(app.cursor, hunk + 1, "moves on");
    app.cursor = hunk;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "pMMM---");
    // and on a hunk wholly marked unmarks it
    app.cursor = hunk;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    app.cursor = hunk;
    app.on_key(key(' '));
    // every line marked one way or another: the file shows as marked
    app.cursor = shown_row(&app, "@@ -26,5");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "MMMMMM-");
    assert!(app.marked_files.is_empty() && app.marked_lines.len() == 3);
    // the file: wholly marked to unmarked, to marked, to unmarked
    app.cursor = file;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    assert!(app.marked_files.is_empty() && app.marked_lines.is_empty());
    app.cursor = file;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "MMMMMM-");
    app.cursor = file;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    // partly marked: to unmarked first
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    app.cursor = file;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    assert!(app.marked_files.is_empty() && app.marked_lines.is_empty());
    // a hunk out of a marked file
    app.cursor = file;
    app.on_key(key(' '));
    app.cursor = hunk;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "p---MM-");
    app.on_key(code(KeyCode::Esc));
    // an unchanged line is nothing to mark: space only moves on
    let unchanged = shown_row(&app, " line 29");
    app.cursor = unchanged;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-------");
    assert_eq!(app.cursor, unchanged + 1);
    assert!(app.marked.is_empty() && app.marked_files.is_empty());
}

/// A file listed twice (staged, and changed since) is marked where it's
/// marked: its other line isn't. An untracked file is marked whole, from
/// any line of it.
#[test]
fn a_file_is_marked_in_its_section_and_an_untracked_one_whole() {
    use argus_tui::details::Section;
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_staged(&mut app, "both");
    let staged = app.cursor;
    app.on_key(key(' '));
    to_file(&mut app, "both");
    let unstaged = app.cursor;
    assert_ne!(staged, unstaged);
    let icon = argus_core::icons::UNICODE.marked;
    let marked = |app: &mut App| -> Vec<usize> {
        let top = app.scroll;
        let screen = render(app, 100, 60);
        // (the rows start under the title line)
        let rows = screen.lines().skip(1).enumerate();
        rows.filter(|(_, l)| l.contains(&*icon))
            .map(|(i, _)| i + top)
            .collect()
    };
    // (and its section's heading, as the only file there)
    assert_eq!(marked(&mut app), [staged - 1, staged]);
    let dirty = app.current().unwrap().path.clone();
    assert_eq!(
        app.marked_files[&dirty],
        [(Section::Staged, "both".to_string())].into()
    );
    app.on_key(code(KeyCode::Esc));
    // an untracked file's lines aren't staged by themselves
    to_file(&mut app, "new");
    let new = app.cursor;
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+b");
    app.on_key(key(' '));
    assert_eq!(
        app.marked_files[&dirty],
        [(Section::Untracked, "new".to_string())].into()
    );
    assert!(app.marked_lines.is_empty());
    assert!(marked(&mut app).contains(&new));
    app.cursor = shown_row(&app, "+a");
    app.on_key(key(' '));
    assert!(app.marked_files.is_empty(), "and unmarked from any");
}

/// A file's mark goes when the file leaves the section it was marked in.
#[test]
fn a_files_mark_goes_with_its_place_in_its_section() {
    let (sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "both");
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.on_key(key(' '));
    let d = sb.path("dev/dirty");
    // staged: `both` is still changed, but no longer under unstaged
    git(&d, &["add", "both"]);
    app.ws.refresh(&d);
    app.rebuild();
    assert_eq!(
        app.marked_files[&d],
        [(argus_tui::details::Section::Unstaged, "README".to_string())].into()
    );
}

/// `s` `u` `K` take marked files and marked lines together: the lines as
/// patches, the files whole.
#[test]
fn marked_files_and_marked_lines_are_acted_on_together() {
    let (_sb, mut app) = setup_with("", |sb| {
        two_hunks(sb);
        write(&sb.path("dev/dirty"), "README", "changed again\n");
        write(&sb.path("dev/dirty"), "new", "a\n");
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.on_key(key(' '));
    to_file(&mut app, "new");
    app.on_key(key(' '));
    app.act(Action::Top);
    let what = |ops: &[Op]| -> Vec<String> {
        ops.iter()
            .map(|op| match op {
                Op::Apply { what, .. } => what.clone(),
                other => other.label(),
            })
            .collect()
    };
    // K asks once, for all of it
    assert!(app.on_key(key('K')).is_empty());
    assert!(
        render(&mut app, 140, 30).contains(
            "Discard 1 marked line, unstaged changes to 1 file and delete 1 untracked? They're lost. (y/n)"
        ),
        "{}",
        render(&mut app, 140, 30)
    );
    app.on_key(key('n'));
    assert!(
        !app.marked_files.is_empty() && !app.marked_lines.is_empty(),
        "kept on no"
    );
    let fx = app.on_key(key('K'));
    assert!(fx.is_empty());
    let fx = app.on_key(key('y'));
    let ops = job_ops(&fx);
    assert_eq!(
        what(&ops),
        [
            "discard 1 line of f",
            "discard README",
            "delete untracked new"
        ]
    );
    // (confirmed and started: the lines are unmarked, as when staged)
    assert!(app.marked_lines.is_empty());
    app.running.clear();
    // s: the line, then the files
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    let fx = app.on_key(key('s'));
    let ops = job_ops(&fx);
    assert_eq!(what(&ops)[0], "stage 1 line of f");
    assert_eq!(
        ops[1..],
        [
            Op::AddTracked {
                paths: paths(&["README"])
            },
            Op::Add {
                paths: paths(&["new"])
            }
        ]
    );
}

/// A file marked where the key doesn't apply (staged for `s` or `K`, not
/// staged for `u`) isn't taken by its other side: it's said, and nothing
/// is done.
#[test]
fn a_file_marked_where_the_key_does_not_apply_is_refused() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    let said = |app: &App| app.message.as_ref().unwrap().0.clone();
    to_staged(&mut app, "both");
    app.on_key(key(' '));
    assert!(app.on_key(key('s')).is_empty());
    assert_eq!(
        said(&app),
        "some marked files are staged already: unmark them (space) first"
    );
    assert!(app.on_key(key('K')).is_empty());
    assert!(matches!(app.mode, Mode::Normal), "nothing asked");
    assert_eq!(
        said(&app),
        "some marked files are staged (u unstages them, then K discards them): unmark them (space) first"
    );
    // with a line marked too: the same, and the line isn't taken either
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    assert!(app.on_key(key('s')).is_empty());
    assert!(said(&app).starts_with("some marked files are staged already"));
    assert!(app.on_key(key('K')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.marked_lines.len(), 1, "kept");
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    // inverse: u is the key for it
    assert_eq!(
        job_ops(&app.on_key(key('u'))),
        [Op::Restore {
            staged: true,
            worktree: false,
            from_head: false,
            paths: paths(&["both"])
        }]
    );
    app.running.clear();
    app.on_key(ctrl('g'));
    // marked under unstaged: not for u, and s and K are the keys for it
    to_file(&mut app, "both");
    app.on_key(key(' '));
    assert!(app.on_key(key('u')).is_empty());
    assert_eq!(
        said(&app),
        "some marked files aren't staged: unmark them (space) first"
    );
    assert_eq!(
        job_ops(&app.on_key(key('s'))),
        [Op::AddTracked {
            paths: paths(&["both"])
        }]
    );
    app.running.clear();
    app.on_key(key('K'));
    assert!(render(&mut app, 100, 30).contains("Discard unstaged changes to both?"));
}

/// Space on a section's heading marks its files: all of them when none
/// is, none when some or all are. It's not the repo that's marked.
#[test]
fn space_on_a_sections_heading_marks_its_files() {
    use argus_tui::details::Section;
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    let dirty = app.current().unwrap().path.clone();
    let rows = ["untracked (2)", "unstaged (2)", "M README", "staged (1)"];
    to_section(&mut app, Section::Unstaged);
    let heading = app.cursor;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-MM-");
    assert_eq!(app.cursor, heading + 1, "moves on, into it");
    assert!(app.marked.is_empty(), "the repo isn't marked");
    assert_eq!(
        app.marked_files[&dirty],
        [
            (Section::Unstaged, "README".to_string()),
            (Section::Unstaged, "both".to_string())
        ]
        .into()
    );
    // wholly marked: unmarked
    app.cursor = heading;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "----");
    assert!(app.marked_files.is_empty());
    // partly (a line of one of its files): unmarked, lines and all
    to_file(&mut app, "both");
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "-pp-");
    app.cursor = heading;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "----");
    assert!(app.marked_files.is_empty() && app.marked_lines.is_empty());
    // (the other section's lines aren't its to unmark)
    to_staged(&mut app, "both");
    app.act(Action::ToggleExpand);
    let staged_line = (0..app.rows.len())
        .rev()
        .find(|n| {
            matches!(&app.rows[*n], Row::Detail { line, .. }
            if line.diff.as_deref().is_some_and(|d| d.at.is_some()
                && d.side == argus_core::diff::Side::Staged
                && d.line.as_ref().is_some_and(|l| l.text == "two")))
        })
        .expect("the staged line");
    app.cursor = staged_line;
    app.on_key(key(' '));
    to_section(&mut app, Section::Unstaged);
    app.on_key(key(' '));
    to_section(&mut app, Section::Unstaged);
    app.on_key(key(' '));
    assert_eq!(app.marked_lines.len(), 1, "the staged line's mark stays");
    app.on_key(ctrl('g'));
    // what's marked that way is what s takes
    to_section(&mut app, Section::Unstaged);
    app.on_key(key(' '));
    app.act(Action::Top);
    assert_eq!(
        job_ops(&app.on_key(key('s'))),
        [Op::AddTracked {
            paths: paths(&["README", "both"])
        }]
    );
}

/// A row that stands for its file (a binary file's note) shows the file's
/// mark, as space there sets it.
#[test]
fn a_row_standing_for_its_file_shows_the_files_mark() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "bin");
    app.act(Action::ToggleExpand);
    app.cursor += 1;
    let note = app.cursor;
    app.on_key(key(' '));
    let top = app.scroll;
    let screen = render(&mut app, 100, 60);
    let shown = |row: usize| screen.lines().nth(row - top + 1).unwrap().contains('▌');
    assert!(shown(note - 1) && shown(note), "{screen}");
}

/// The blank line after a diff is nothing to mark: space only moves on.
#[test]
fn space_on_the_blank_line_after_a_diff_marks_nothing() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    let blank = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.id.last() == Some(&Seg::Blank)))
        .expect("a blank line after the diff");
    app.cursor = blank;
    app.on_key(key(' '));
    assert_eq!(app.marked_lines.len(), 1);
    assert!(app.marked_files.is_empty() && app.marked.is_empty());
    assert_eq!(app.cursor, blank + 1);
}

/// An untracked file is marked whole, so no region starts in its diff.
#[test]
fn no_region_starts_in_an_untracked_file() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "new");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+a");
    app.on_key(ctrl(' '));
    assert!(app.region.is_none());
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "an untracked file is marked whole: space marks it"
    );
    // inverse: one starts in a changed file's
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(ctrl(' '));
    assert!(app.region.is_some());
}

/// Marks stay when what was asked for them doesn't start (the repo busy).
#[test]
fn marks_stay_when_nothing_starts() {
    let (_sb, mut app) = setup_with("", two_hunks);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    let dirty = app.current().unwrap().path.clone();
    app.running.insert(dirty, "busy".into());
    assert!(app.on_key(key('s')).is_empty());
    assert_eq!(app.marked_lines.len(), 1);
    app.on_key(key('z'));
    assert!(app.on_key(key('m')).is_empty());
    assert_eq!(app.marked_lines.len(), 1);
    // inverse: started, they go
    app.running.clear();
    assert!(!app.on_key(key('s')).is_empty());
    assert!(app.marked_lines.is_empty());
}

/// `e` opens a file marked whole as its hunks, with any marked lines.
#[test]
fn a_file_marked_whole_is_edited_as_its_hunks() {
    let (_sb, mut app) = setup_with("", |sb| {
        two_hunks(sb);
        write(&sb.path("dev/dirty"), "README", "changed again\n");
    });
    to_file(&mut app, "f");
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+changed again");
    app.on_key(key(' '));
    app.act(Action::Top);
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("+line three") && text.contains("+added") && text.contains("-line 3"),
        "{text}"
    );
    assert!(
        text.contains("+changed again") && !text.contains("-changed"),
        "{text}"
    );
}

/// What's marked twice over is one thing to act on: a region's lines in
/// a file marked whole, a file in a folder marked whole.
#[test]
fn what_is_marked_twice_over_counts_once() {
    let (_sb, mut app) = setup_with("", |sb| {
        two_hunks(sb);
        write(&sb.path("dev/dirty"), "build/a", "a\n");
        write(&sb.path("dev/dirty"), "build/b", "b\n");
    });
    to_file(&mut app, "f");
    let file = app.cursor;
    app.act(Action::ToggleExpand);
    app.on_key(key(' '));
    app.cursor = shown_row(&app, "-line 3");
    app.on_key(ctrl(' '));
    app.on_key(key('j'));
    app.on_key(ctrl(' '));
    assert!(app.marked_lines.is_empty() && app.region.is_none());
    app.on_key(key('K'));
    assert!(
        render(&mut app, 100, 40).contains("Discard unstaged changes to f? They're lost."),
        "{}",
        render(&mut app, 100, 40)
    );
    app.on_key(key('n'));
    app.cursor = file;
    app.on_key(key(' '));
    assert!(app.marked_files.is_empty());
    // a folder marked: its files show it, and aren't marked besides
    to_file(&mut app, "build/");
    let folder = app.cursor;
    app.on_key(key(' '));
    app.cursor = folder;
    app.act(Action::ToggleExpand);
    assert_eq!(marks_of(&mut app, &["build/", "build/a", "build/b"]), "MMM");
    app.cursor = folder + 1;
    app.on_key(key(' '));
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "its folder is marked: unmark that to mark files in it"
    );
    assert_eq!(marks_of(&mut app, &["build/", "build/a", "build/b"]), "MMM");
    // a file in it marked first, then the folder: one thing to delete
    app.cursor = folder;
    app.on_key(key(' '));
    app.cursor = folder + 1;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &["build/", "build/a", "build/b"]), "pM-");
    // (the folder partly marked: unmarked, then marked whole)
    app.cursor = folder;
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &["build/", "build/a", "build/b"]), "---");
    app.cursor = folder;
    app.on_key(key(' '));
    app.on_key(key('K'));
    assert!(
        render(&mut app, 100, 40).contains("Delete untracked build/? It's lost."),
        "{}",
        render(&mut app, 100, 40)
    );
}

/// `z m` with lines marked stashes those lines by themselves; a file
/// marked whole beside them goes as its lines. What can't go that way (an
/// untracked file, staged lines) is said, and nothing stashed.
#[test]
fn marked_lines_can_be_stashed() {
    let (_sb, mut app) = setup_with("", |sb| {
        two_hunks(sb);
        write(&sb.path("dev/dirty"), "README", "changed again\n");
        write(&sb.path("dev/dirty"), "new", "a\n");
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let stash_marked = |app: &mut App| {
        app.on_key(key('z'));
        assert!(
            menu_items(app).contains(&"m stash marked*".to_string()),
            "{:?}",
            menu_items(app)
        );
        app.on_key(key('m'))
    };
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    match job_ops(&stash_marked(&mut app)).as_slice() {
        [
            Op::StashLines {
                message,
                stage,
                discard,
                lines: 1,
            },
        ] => {
            assert!(message.starts_with("argus "), "{message}");
            assert!(
                stage.contains("+added") && !stage.contains("three"),
                "{stage}"
            );
            assert!(discard.contains("+added"), "{discard}");
        }
        other => panic!("{other:?}"),
    }
    assert!(app.marked_lines.is_empty(), "acted on, unmarked");
    app.running.clear();
    // a file marked whole with them (its diff not even open): its lines
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.on_key(key(' '));
    match job_ops(&stash_marked(&mut app)).as_slice() {
        [
            Op::StashLines {
                stage, lines: 3, ..
            },
        ] => {
            assert!(
                stage.contains("a/f")
                    && stage.contains("+added")
                    && stage.contains("+changed again"),
                "{stage}"
            );
        }
        other => panic!("{other:?}"),
    }
    app.running.clear();
    // an untracked file can't go in a stash of lines
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    to_file(&mut app, "new");
    app.on_key(key(' '));
    assert!(stash_marked(&mut app).is_empty());
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "not stashing: new is marked whole and lines of other files are too: \
         a stash of lines takes changed text files (unmark one or the other)"
    );
    assert_eq!(app.marked_lines.len(), 1, "kept");
    app.on_key(ctrl('g'));
    // nor lines that are staged
    let d = app.current().unwrap().path.clone();
    git(&d, &["add", "f"]);
    app.ws.refresh(&d);
    app.rebuild();
    to_staged(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = app.rows.len() - 1;
    while !matches!(&app.rows[app.cursor], Row::Detail { line, .. }
        if line.diff.as_deref().is_some_and(|d| d.at.is_some() && d.side == argus_core::diff::Side::Staged
            && d.line.as_ref().is_some_and(|l| l.text == "added")))
    {
        app.cursor -= 1;
    }
    app.on_key(key(' '));
    assert_eq!(app.marked_lines.len(), 1);
    assert!(stash_marked(&mut app).is_empty());
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "not stashing: some marked lines are staged (z g stashes what's staged): unmark them (space) first"
    );
}

/// `M` marks the repos listed (a filter's, say) and `U` unmarks them,
/// wherever the cursor is; neither touches what's marked in details.
/// `C-g` and Esc cancel that (and a region), in every repo, and never
/// the repos' marks.
#[test]
fn m_and_u_are_for_repos_and_c_g_for_what_is_marked_in_details() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    assert_eq!(names(&app).len(), 3);
    to_file(&mut app, "README");
    let dirty = app.current().unwrap().path.clone();
    app.on_key(key(' '));
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    // in the details too, M is the repos
    app.on_key(key('M'));
    assert_eq!(app.marked.len(), 3);
    assert_eq!(app.marked_files[&dirty].len(), 1, "the file's mark stays");
    app.on_key(key('U'));
    assert!(app.marked.is_empty());
    assert_eq!(app.marked_files[&dirty].len(), 1);
    // those listed: within a filter, the ones it leaves
    app.act(Action::Top);
    let filter = |app: &mut App| {
        for c in "/dty".chars() {
            app.on_key(key(c));
        }
        app.on_key(code(KeyCode::Enter));
    };
    filter(&mut app);
    assert_eq!(names(&app), ["acme/dirty"]);
    app.on_key(key('M'));
    assert_eq!(app.marked, [dirty.clone()].into());
    app.on_key(code(KeyCode::Esc)); // the filter
    app.on_key(key('M'));
    assert_eq!(app.marked.len(), 3);
    filter(&mut app);
    app.on_key(key('U'));
    assert!(app.marked.len() == 2 && !app.marked.contains(&dirty));
    app.on_key(code(KeyCode::Esc));
    // C-g: what's marked in details (from anywhere), a region too; the
    // repos stay
    to_file(&mut app, "README");
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    app.on_key(ctrl(' '));
    assert!(app.region.is_some() && app.marked_lines.len() == 1);
    app.act(Action::Bottom);
    app.on_key(ctrl('g'));
    assert!(app.marked_files.is_empty() && app.marked_lines.is_empty() && app.region.is_none());
    assert_eq!(app.marked.len(), 2);
    // Esc does as C-g; neither ever unmarks the repos, which is U's
    to_file(&mut app, "README");
    app.on_key(key(' '));
    app.on_key(code(KeyCode::Esc));
    assert!(app.marked_files.is_empty());
    app.on_key(code(KeyCode::Esc));
    app.on_key(ctrl('g'));
    assert_eq!(app.marked.len(), 2);
}

/// `C-space` starts a region in a diff: the added and removed lines
/// between it and the cursor (unchanged ones ignored) are acted on with the
/// marked ones; `C-space` again keeps them as marks, `C-g` drops it.
#[test]
fn a_region_of_diff_lines_is_staged() {
    let (_sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        write(&d, "f", &lines);
        commit(&d, "f");
        let changed = lines
            .replace("line 3\n", "line three\n")
            .replace("line 28\n", "line 28\nadded\n");
        write(&d, "f", &changed);
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let row_of = |app: &App, text: &str| {
        app.rows
            .iter()
            .position(|r| matches!(r, Row::Detail { line, .. }
                if line.diff.as_deref().and_then(|d| d.line.as_ref()).is_some_and(|l| l.text == text)))
            .unwrap_or_else(|| panic!("{text}"))
    };
    // from "line three" down to "added", past unchanged lines and a header
    app.cursor = row_of(&app, "line three");
    app.on_key(ctrl(' '));
    while app.cursor < row_of(&app, "added") {
        app.on_key(code(KeyCode::Down));
    }
    // (its lines show as they will be: marked)
    assert_eq!(
        marks_of(&mut app, &["+line three", " line 4", "+added"]),
        "M-M"
    );
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => match &j[0].op {
            Op::Apply { patch, what, .. } => {
                assert_eq!(what, "stage 2 lines of f");
                assert!(
                    patch.contains("+line three") && patch.contains("+added"),
                    "{patch}"
                );
                assert!(
                    patch.contains(" line 3\n"),
                    "the line above the region left out: {patch}"
                );
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert!(
        app.region.is_none() && app.marked_lines.is_empty(),
        "acted on, gone"
    );
    app.running.clear();
    // C-space again keeps the region's lines as marks
    app.cursor = row_of(&app, "line 3");
    app.on_key(ctrl(' '));
    app.on_key(code(KeyCode::Down));
    app.on_key(ctrl(' '));
    assert!(app.region.is_none());
    assert_eq!(app.marked_lines.len(), 2, "line 3 and line three");
    // C-g drops a region (and the marks)
    app.on_key(ctrl(' '));
    app.on_key(ctrl('g'));
    assert!(app.region.is_none() && app.marked_lines.is_empty());
}

/// A region keeps to its file and side: lines of the other side (or file)
/// it runs over aren't in it.
#[test]
fn a_region_keeps_to_its_file_and_side() {
    use argus_core::diff::{LineKind, Side};
    let (_sb, mut app) = setup_with("", diff_fixture);
    // README's unstaged lines and both's staged ones are around it
    for f in ["README", "both"] {
        to_file(&mut app, f);
        app.act(Action::ToggleExpand);
    }
    to_staged(&mut app, "both");
    app.act(Action::ToggleExpand);
    let changed = |app: &App, file: &str, side| -> Vec<usize> {
        (0..app.rows.len())
            .filter(|&i| {
                matches!(&app.rows[i], Row::Detail { line, .. } if line.file.as_deref() == Some(file)
                && line.diff.as_deref().is_some_and(|d| d.side == side
                    && d.line.as_ref().is_some_and(|l| l.kind != LineKind::Context)))
            })
            .collect()
    };
    let own = changed(&app, "both", Side::Unstaged);
    assert!(!changed(&app, "README", Side::Unstaged).is_empty());
    assert!(!changed(&app, "both", Side::Staged).is_empty());
    app.cursor = own[0];
    app.on_key(ctrl(' '));
    // to either end, over the other side's lines and the other file's
    // up: only the first is in range; down: all of them
    for (end, n) in [(0, 1), (app.rows.len() - 1, own.len())] {
        app.cursor = end;
        assert_eq!(app.region_lines().len(), n, "to row {end}");
    }
}

/// `f`: 30 lines committed, then line 3 changed and a line added after 28
/// (two hunks); `g`: one line changed.
fn edit_fixture(sb: &Sandbox) {
    let d = sb.path("dev/dirty");
    let lines: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    write(&d, "f", &lines);
    write(&d, "g", "old\n");
    commit(&d, "f");
    let changed = lines
        .replace("line 3\n", "line three\n")
        .replace("line 28\n", "line 28\nadded\n");
    write(&d, "f", &changed);
    write(&d, "g", "new\n");
}

fn diff_row(app: &App, text: &str) -> usize {
    app.rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { line, .. }
            if line.diff.as_deref().and_then(|d| d.line.as_ref()).is_some_and(|l| l.text == text))
        })
        .unwrap_or_else(|| panic!("{text}"))
}

/// `e`'s editor runs through `sh` on the patch file, its last argument.
fn editing_file(fx: &[Effect]) -> PathBuf {
    match fx {
        [Effect::Suspend { program, args, .. }] => {
            assert_eq!(program, "sh");
            assert!(args[1].contains("git var GIT_EDITOR"), "{args:?}");
            PathBuf::from(args.last().unwrap())
        }
        other => panic!("{other:?}"),
    }
}

fn apply_op(fx: &[Effect]) -> (PathBuf, Op) {
    match fx {
        [Effect::Jobs(j)] => (j[0].path.clone(), j[0].op.clone()),
        other => panic!("{other:?}"),
    }
}

/// Run the job for real and tell the app how it went.
fn finish(app: &mut App, path: PathBuf, op: Op) -> bool {
    let outcome = argus_core::ops::run(&path, &op);
    let ok = outcome.success;
    app.on_msg(Msg::JobFinished {
        path,
        label: "apply".into(),
        op,
        outcome,
    });
    ok
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap()
}

/// `e` in a diff opens the line (or hunk, or marked lines) as a patch in
/// git's editor; what's saved is staged, the rest of the diff left alone.
#[test]
fn an_edited_line_is_staged_and_the_rest_kept() {
    let (sb, mut app) = setup_with("", edit_fixture);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "added");
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("diff --git a/f b/f\n"), "{text}");
    assert!(
        text.contains("+added\n") && !text.contains("line three"),
        "{text}"
    );
    assert!(
        text.contains("\n# ") && text.contains("staged"),
        "instructions: {text}"
    );
    std::fs::write(&file, text.replace("+added\n", "+edited\n")).unwrap();
    let (path, op) = apply_op(&app.on_msg(Msg::Suspended(Ok(()))));
    match &op {
        Op::Apply {
            patch,
            index,
            reverse,
            what,
        } => {
            assert!(*index && !*reverse);
            assert_eq!(what, "stage an edited patch of f");
            assert!(!patch.contains('#'), "instructions dropped: {patch}");
        }
        other => panic!("{other:?}"),
    }
    assert!(finish(&mut app, path, op));
    let d = sb.path("dev/dirty");
    let staged = git_out(&d, &["diff", "--cached"]);
    assert!(
        staged.contains("+edited") && !staged.contains("three"),
        "{staged}"
    );
    let left = git_out(&d, &["diff"]);
    assert!(
        left.contains("-edited\n+added") && left.contains("+line three"),
        "the rest of the diff kept: {left}"
    );
    assert!(app.editing.is_none() && !file.exists(), "done with");
}

/// Marked lines of several files make one patch.
#[test]
fn marked_lines_of_two_files_are_edited_as_one_patch() {
    let (sb, mut app) = setup_with("", edit_fixture);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    to_file(&mut app, "g");
    app.act(Action::ToggleExpand);
    for t in ["line three", "new"] {
        app.cursor = diff_row(&app, t);
        app.on_key(key(' '));
    }
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("diff --git a/f b/f") && text.contains("diff --git a/g b/g"));
    let (path, op) = apply_op(&app.on_msg(Msg::Suspended(Ok(()))));
    assert!(matches!(&op, Op::Apply { what, .. } if what == "stage an edited patch of 2 files"));
    assert!(finish(&mut app, path, op));
    let staged = git_out(&sb.path("dev/dirty"), &["diff", "--cached"]);
    assert!(
        staged.contains("+line three") && staged.contains("+new") && !staged.contains("added"),
        "{staged}"
    );
}

/// On the staged side, `e` unstages what's saved: a `+` line made context
/// stays staged.
#[test]
fn an_edited_hunk_is_unstaged() {
    let (sb, mut app) = setup_with("", |sb| {
        edit_fixture(sb);
        let d = sb.path("dev/dirty");
        let f = std::fs::read_to_string(d.join("f")).unwrap();
        write(&d, "f", &f.replace("added\n", "added\nalso\n"));
        git(&d, &["add", "f"]);
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    // the second hunk's header
    app.cursor = diff_row(&app, "added") - 4;
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("+added\n+also\n") && !text.contains("three"),
        "{text}"
    );
    assert!(text.contains("unstaged"), "{text}");
    std::fs::write(&file, text.replace("+added\n", " added\n")).unwrap();
    let (path, op) = apply_op(&app.on_msg(Msg::Suspended(Ok(()))));
    assert!(
        matches!(&op, Op::Apply { index: true, reverse: true, what, .. }
        if what == "unstage an edited patch of f")
    );
    assert!(finish(&mut app, path, op));
    let d = sb.path("dev/dirty");
    let staged = git_out(&d, &["diff", "--cached"]);
    assert!(
        staged.contains("+added") && staged.contains("+line three") && !staged.contains("also"),
        "{staged}"
    );
    assert!(
        git_out(&d, &["diff"]).contains("+also"),
        "back in the work tree's diff"
    );
    assert!(app.editing.is_none() && !file.exists());
}

/// A patch that doesn't apply asks to edit it again, with the edit kept;
/// no drops it, leaving everything as it was.
#[test]
fn an_edit_that_does_not_apply_can_be_edited_again_or_dropped() {
    let (sb, mut app) = setup_with("", edit_fixture);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "added");
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    let broken = text.replace(" line 27\n", " line 27 changed\n");
    std::fs::write(&file, &broken).unwrap();
    let (path, op) = apply_op(&app.on_msg(Msg::Suspended(Ok(()))));
    assert!(!finish(&mut app, path, op));
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.contains("doesn't apply"))
    );
    // y: the same file again, the edit in it
    assert_eq!(editing_file(&app.on_key(key('y'))), file);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
    // still broken: n drops it
    let (path, op) = apply_op(&app.on_msg(Msg::Suspended(Ok(()))));
    assert!(!finish(&mut app, path, op));
    assert!(app.on_key(key('n')).is_empty());
    assert!(app.editing.is_none() && !file.exists());
    assert!(git_out(&sb.path("dev/dirty"), &["diff", "--cached"]).is_empty());
}

/// What `e` won't do: an emptied patch, one naming another file or
/// changing anything but hunks, an editor that failed, an untracked file,
/// lines of both sides, outside a diff.
#[test]
fn edits_that_are_refused() {
    let (sb, mut app) = setup_with("", |sb| {
        edit_fixture(sb);
        write(&sb.path("dev/dirty"), "new", "a\n");
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let start = |app: &mut App| {
        app.cursor = diff_row(app, "added");
        editing_file(&app.on_key(key('e')))
    };
    let msg = |app: &App| app.message.clone().unwrap().0;

    // emptied, or only the instructions left: nothing happens
    let file = start(&mut app);
    let text = std::fs::read_to_string(&file).unwrap();
    let instructions: String = text
        .lines()
        .filter(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&file, instructions).unwrap();
    assert!(app.on_msg(Msg::Suspended(Ok(()))).is_empty());
    assert!(msg(&app).contains("empty"), "{}", msg(&app));
    assert!(app.editing.is_none() && !file.exists());

    // another file, or a header line that isn't the file's: edit again?
    for bad in [
        text.replace("diff --git a/f b/f", "diff --git a/g b/g"),
        text.replace("+++ b/f", "+++ b/g"),
        text.replacen("@@", "new file mode 100755\n@@", 1),
    ] {
        let file = start(&mut app);
        std::fs::write(&file, &bad).unwrap();
        assert!(app.on_msg(Msg::Suspended(Ok(()))).is_empty(), "{bad}");
        assert!(matches!(&app.mode, Mode::Confirm { .. }), "{bad}");
        app.on_key(key('n'));
        assert!(app.editing.is_none() && !file.exists());
    }

    // the editor failed
    let file = start(&mut app);
    assert!(
        app.on_msg(Msg::Suspended(Err("vi exited with 1".into())))
            .is_empty()
    );
    assert!(app.message.as_ref().unwrap().1, "an error");
    assert!(app.editing.is_none() && !file.exists());

    // untracked: no patch to edit
    to_file(&mut app, "new");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "a");
    assert!(app.on_key(key('e')).is_empty());
    assert!(msg(&app).contains("untracked"), "{}", msg(&app));

    // outside a diff
    app.cursor = 0;
    assert!(app.on_key(key('e')).is_empty());
    assert!(msg(&app).contains("diff"), "{}", msg(&app));
    assert!(git_out(&sb.path("dev/dirty"), &["diff", "--cached"]).is_empty());
}

/// Marked lines must all be on one side.
#[test]
fn editing_marked_lines_of_both_sides_is_refused() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "both");
    app.act(Action::ToggleExpand);
    to_staged(&mut app, "both");
    app.act(Action::ToggleExpand);
    for t in ["three", "two"] {
        let i = app
            .rows
            .iter()
            .rposition(|r| {
                matches!(r, Row::Detail { line, .. }
                if line.diff.as_deref().and_then(|d| d.line.as_ref())
                    .is_some_and(|l| l.text == t && l.kind == argus_core::diff::LineKind::Add))
            })
            .unwrap();
        app.cursor = i;
        app.on_key(key(' '));
    }
    assert_eq!(app.marked_lines.len(), 2);
    assert!(app.on_key(key('e')).is_empty());
    assert!(
        app.message
            .clone()
            .unwrap()
            .0
            .contains("staged and unstaged")
    );
    assert!(app.editing.is_none());
}

/// On an unchanged line (or the header), `e` takes the whole hunk.
/// A patch file's name already taken (left by an argus that had this
/// process's number) isn't written over, nor does it stop the edit.
#[test]
fn the_patch_file_passes_over_names_already_taken() {
    let (_sb, mut app) = setup_with("", edit_fixture);
    let name =
        |n: usize| std::env::temp_dir().join(format!("argus-{}-{n}.diff", std::process::id()));
    for n in 0..3 {
        std::fs::write(name(n), "somebody's").unwrap();
    }
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "line 2");
    let file = editing_file(&app.on_key(key('e')));
    assert_eq!(file, name(3));
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("+line three")
    );
    for n in 0..3 {
        assert_eq!(std::fs::read_to_string(name(n)).unwrap(), "somebody's");
        std::fs::remove_file(name(n)).unwrap();
    }
    std::fs::remove_file(file).unwrap();
}

#[test]
fn editing_on_an_unchanged_line_takes_the_hunk() {
    let (_sb, mut app) = setup_with("", edit_fixture);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "line 2");
    let file = editing_file(&app.on_key(key('e')));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("-line 3\n+line three\n"), "{text}");
    std::fs::write(&file, "").unwrap();
    app.on_msg(Msg::Suspended(Ok(())));
    assert!(app.editing.is_none() && !file.exists());
}

/// `x W` on a finished worktree asks first; no leaves it, yes removes it.
#[test]
fn removing_a_finished_worktree_asks_first() {
    use argus_core::fix::FixKind;
    let (sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        let wt = sb.path("dev/clean-wt");
        git(
            &r,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        commit(&wt, "work");
        git(&wt, &["push", "-q", "-u", "origin", "wt"]);
        git(&r, &["merge", "-q", "--no-edit", "--no-ff", "wt"]);
        git(&r, &["push", "-q"]);
        git(&r, &["push", "-q", "origin", "--delete", "wt"]);
        git(&r, &["fetch", "-q", "--prune"]);
    });
    let wt = sb.path("dev/clean-wt");
    // not one: said, nothing asked
    app.cursor = 0;
    assert!(
        app.act(Action::ApplyFix(FixKind::RemoveWorktree))
            .is_empty()
    );
    assert!(!matches!(app.mode, Mode::Confirm { .. }));
    assert!(
        app.message
            .clone()
            .unwrap()
            .0
            .contains("no finished worktree")
    );
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == wt))
        .unwrap();
    assert!(
        app.act(Action::ApplyFix(FixKind::RemoveWorktree))
            .is_empty()
    );
    assert!(matches!(&app.mode, Mode::Confirm { question, .. }
            if question.contains("clean-wt") && question.contains("ignored files")),);
    assert!(app.on_key(key('n')).is_empty());
    assert!(wt.exists());
    app.act(Action::ApplyFix(FixKind::RemoveWorktree));
    let (path, op) = match app.on_key(key('y')).as_slice() {
        [Effect::Jobs(j)] => (j[0].path.clone(), j[0].op.clone()),
        other => panic!("{other:?}"),
    };
    assert_eq!((&path, &op), (&wt, &Op::RemoveWorktree));
    // removed from argus: its row goes at once, nothing to acknowledge
    assert!(finish(&mut app, path, op));
    assert!(app.ws.index_of(&wt).is_none());
    assert!(
        !app.rows
            .iter()
            .any(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == wt))
    );
}

fn issue_row(app: &App, name: &str) -> usize {
    app.rows
        .iter()
        .position(|r| {
            matches!(r, Row::Detail { line, .. }
            if line.issue.is_some_and(|i| i.name() == name))
        })
        .unwrap_or_else(|| panic!("{name}: {:?}", trimmed(app)))
}

/// `t z` on an issue line snoozes that issue only; `t u` there ends it,
/// and on the repo ends the repo's snooze and its issues'.
#[test]
fn an_issue_is_snoozed_from_its_line() {
    let (_sb, mut app) = setup("");
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Repo(i) if app.ws.entries[*i].display_name() == "acme/dirty"),
        )
        .unwrap();
    app.on_key(code(KeyCode::Tab));
    let key = app.current().unwrap().key.clone();
    app.cursor = issue_row(&app, "dirty");
    app.act(Action::Snooze);
    assert!(
        matches!(&app.mode, Mode::Input { prompt: Prompt::SnoozeIssue(k, i), .. }
            if *k == key && i.name() == "dirty")
    );
    app.on_key(code(KeyCode::Enter)); // the default, 3d
    assert!(app.ws.state.snoozed_issues[&key].contains_key("dirty"));
    assert!(!app.ws.state.snoozed.contains_key(&key), "not the repo");
    let e = app.current().unwrap();
    assert!(e.issues.is_empty() && e.snoozed_issues.len() == 1);
    let Row::Detail { line, .. } = &app.rows[issue_row(&app, "dirty")] else {
        unreachable!()
    };
    let line: String = line.parts.iter().map(|p| p.1.as_str()).collect();
    assert!(line.ends_with("(snoozed, 3d left)"), "{line}");

    // t u on its line
    app.cursor = issue_row(&app, "dirty");
    app.act(Action::Unsnooze);
    assert!(app.ws.state.snoozed_issues.is_empty());
    assert!(!app.current().unwrap().issues.is_empty());

    // t u on the repo: its issues' snoozes too
    app.cursor = issue_row(&app, "dirty");
    app.act(Action::Snooze);
    app.on_key(code(KeyCode::Enter));
    app.ws.state.snoozed.insert(
        key.clone(),
        jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1),
    );
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].key == key))
        .unwrap();
    app.act(Action::Unsnooze);
    assert!(app.ws.state.snoozed_issues.is_empty() && app.ws.state.snoozed.is_empty());
}

/// A snoozed issue's line still unfolds, as it did before.
#[test]
fn a_snoozed_issue_still_unfolds() {
    let (_sb, mut app) = setup_with("", |sb| commit(&sb.path("dev/dirty"), "local"));
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Repo(i) if app.ws.entries[*i].display_name() == "acme/dirty"),
        )
        .unwrap();
    app.on_key(code(KeyCode::Tab));
    app.cursor = issue_row(&app, "unpushed");
    app.act(Action::Snooze);
    app.on_key(code(KeyCode::Enter));
    let Row::Detail { line, .. } = &app.rows[issue_row(&app, "unpushed")] else {
        unreachable!()
    };
    assert!(line.expand.is_some(), "{line:?}");
}

fn repo_row(app: &App, path: &std::path::Path) -> usize {
    app.rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path == path))
        .unwrap_or_else(|| panic!("{}", path.display()))
}

/// A refresh's answer for `path`, as the runtime sends it.
fn refreshed(app: &mut App, path: &std::path::Path) {
    let r = argus_core::status::status(path, Default::default()).map_err(|e| e.to_string());
    app.on_msg(Msg::Status(path.to_path_buf(), Box::new(r)));
}

/// A repo deleted outside argus shows as an error at the next refresh,
/// with fixes to drop it or rescan; the refresh after that drops it.
#[test]
fn a_repo_deleted_outside_argus_is_seen_then_dropped() {
    let (sb, mut app) = setup("");
    let clean = sb.path("dev/clean");
    std::fs::remove_dir_all(&clean).unwrap();
    refreshed(&mut app, &clean);
    app.cursor = repo_row(&app, &clean);
    assert!(app.current().unwrap().error().is_some());
    app.on_key(code(KeyCode::Tab));
    assert!(
        trimmed(&app)
            .iter()
            .any(|l| l.contains("x g") && l.contains("x G")),
        "{:?}",
        trimmed(&app)
    );
    app.cursor = repo_row(&app, &clean);
    app.on_key(key('x'));
    let items = menu_items(&app);
    assert!(items.iter().any(|i| i.starts_with("g drop")), "{items:?}");
    assert!(items.iter().any(|i| i.starts_with("G rescan")), "{items:?}");
    app.mode = Mode::Normal;
    // drop: gone from the list
    app.act(Action::DropGone);
    assert!(app.ws.index_of(&clean).is_none());

    // or the second refresh
    let dirty = sb.path("dev/dirty");
    std::fs::remove_dir_all(&dirty).unwrap();
    refreshed(&mut app, &dirty);
    assert!(app.ws.index_of(&dirty).is_some());
    refreshed(&mut app, &dirty);
    assert!(app.ws.index_of(&dirty).is_none());
    // nothing gone selected: drop says so
    app.cursor = 0;
    assert!(app.act(Action::DropGone).is_empty());
    assert!(app.message.clone().unwrap().0.contains("gone"));
}

/// A worktree folder deleted by hand: its row (an error) offers pruning
/// its record, run in the main repo, which shows the issue too; pruned,
/// the row goes. Dropping isn't offered: the record would bring it back.
#[test]
fn a_stale_worktree_row_prunes_its_record() {
    use argus_core::fix::FixKind;
    let (sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        let wt = sb.path("dev/clean-wt");
        git(
            &r,
            &["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()],
        );
        std::fs::remove_dir_all(&wt).unwrap();
    });
    let (main, wt) = (sb.path("dev/clean"), sb.path("dev/clean-wt"));
    app.cursor = repo_row(&app, &wt);
    assert!(app.current().unwrap().error().is_some());
    app.on_key(key('x'));
    let items = menu_items(&app);
    assert!(
        items.iter().any(|i| i.starts_with("w prune-worktrees")),
        "{items:?}"
    );
    assert!(!items.iter().any(|i| i.starts_with("g drop")), "{items:?}");
    app.mode = Mode::Normal;
    app.on_key(code(KeyCode::Tab));
    assert!(
        trimmed(&app).iter().any(|l| l.contains("x w")),
        "{:?}",
        trimmed(&app)
    );
    app.cursor = repo_row(&app, &wt);
    let (path, op) = apply_op(&app.act(Action::ApplyFix(FixKind::PruneWorktrees)));
    assert_eq!(path, main, "run in the main repo");
    assert_eq!(op, Op::PruneWorktree { path: wt.clone() });
    assert!(finish(&mut app, path, op));
    assert!(app.ws.index_of(&wt).is_none());
    refreshed(&mut app, &main);
    assert!(app.ws.index_of(&wt).is_none(), "not brought back");
}

/// `x w` on the main repo prunes every stale record, and their rows go.
#[test]
fn pruning_from_the_main_repo_drops_the_stale_rows() {
    use argus_core::fix::FixKind;
    let (sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        for n in ["a", "b"] {
            let wt = sb.path(&format!("dev/clean-{n}"));
            git(
                &r,
                &["worktree", "add", "-q", "-b", n, wt.to_str().unwrap()],
            );
            std::fs::remove_dir_all(&wt).unwrap();
        }
    });
    let main = sb.path("dev/clean");
    let rows = |app: &App| {
        ["a", "b"]
            .iter()
            .filter(|n| {
                app.ws
                    .index_of(&sb.path(&format!("dev/clean-{n}")))
                    .is_some()
            })
            .count()
    };
    assert_eq!(rows(&app), 2);
    app.cursor = repo_row(&app, &main);
    let (path, op) = apply_op(&app.act(Action::ApplyFix(FixKind::PruneWorktrees)));
    assert_eq!((&path, &op), (&main, &Op::PruneWorktrees));
    assert!(finish(&mut app, path, op));
    assert_eq!(rows(&app), 0);
}

/// The picker: a box over the list, the matches with the cursor's
/// highlighted, a footer that fits; the query on the status line.
#[test]
fn the_branch_picker_draws_its_matches_and_query() {
    use argus_tui::theme::Ui;
    let (_sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        git(&f, &["branch", "-q", "mother"]);
    });
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("feature")))
        .unwrap();
    app.act(Action::Checkout);
    app.on_key(key('m'));
    let (theme, _) = Theme::build("mono", &Default::default());
    let (w, h) = (100, 30);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    let view = ui::View {
        theme: &theme,
        icons: argus_core::icons::UNICODE,
        tick: 0,
    };
    t.draw(|f| drop(ui::draw(f, &mut app, &view))).unwrap();
    let buf = t.backend().buffer().clone();
    let lines: Vec<String> = (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect();
    let row = |text: &str| {
        lines
            .iter()
            .position(|l| l.contains(text))
            .unwrap_or_else(|| panic!("{text}:\n{}", lines.join("\n")))
    };
    // the query, cursor after it
    let status = lines
        .iter()
        .position(|l| l.starts_with("checkout: m"))
        .unwrap();
    assert_eq!(t.get_cursor_position().unwrap(), (11, status as u16).into());
    // matches, the first under the cursor
    let first = row("│ main");
    let second = row("│ mother");
    assert_eq!(second, first + 1);
    let x = lines[first].find("main").unwrap();
    let x = lines[first][..x].chars().count() as u16;
    let sel = theme.ui(Ui::Selection);
    assert!(!sel.add_modifier.is_empty());
    assert!(
        buf[(x, first as u16)].modifier.contains(sel.add_modifier),
        "highlighted"
    );
    assert!(!buf[(x, second as u16)].modifier.contains(sel.add_modifier));
    // the footer fits the box, which ends below it
    let foot = row(" of ");
    assert!(lines[foot].contains("Esc/C-g cancel  │"), "{}", lines[foot]);
    assert!(lines[foot + 1].contains('╰'), "{}", lines[foot + 1]);
}

/// Some terminals send C-space as NUL: it starts a region too.
#[test]
fn nul_starts_a_region() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = diff_row(&app, "hello there");
    app.on_key(code(KeyCode::Null));
    assert!(app.region.is_some());
}

/// Space on a diff line marks it and moves down a line, as magit does.
#[test]
fn marking_a_diff_line_moves_down() {
    let (_sb, mut app) = setup_with("", edit_fixture);
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    let at = diff_row(&app, "line three");
    app.cursor = at;
    app.on_key(key(' '));
    assert_eq!(app.cursor, at + 1);
    assert_eq!(app.marked_lines.len(), 1);
}

/// A repo's line marks go when its status (so its diff) changes; other
/// repos' stay.
#[test]
fn line_marks_go_with_their_repos_status() {
    let (sb, mut app) = setup_with("", |sb| {
        edit_fixture(sb);
        write(&sb.path("dev/clean"), "README", "changed\n");
    });
    for (repo, file, text) in [
        ("dev/dirty", "f", "line three"),
        ("dev/clean", "README", "changed"),
    ] {
        let p = sb.path(repo);
        app.cursor = repo_row(&app, &p);
        to_file(&mut app, file);
        app.act(Action::ToggleExpand);
        app.cursor = diff_row(&app, text);
        app.on_key(key(' '));
    }
    assert_eq!(app.marked_lines.len(), 2);
    let dirty = sb.path("dev/dirty");
    refreshed(&mut app, &dirty);
    let left: Vec<_> = app.marked_lines.iter().map(|k| k.0.clone()).collect();
    assert_eq!(left, [sb.path("dev/clean")]);
}

/// A branch number past the list does nothing.
#[test]
fn picking_past_the_branch_list_does_nothing() {
    let (_sb, mut app) = setup("");
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].path.ends_with("feature")))
        .unwrap();
    app.act(Action::Checkout);
    let n = match &app.mode {
        Mode::Pick { labels, .. } => labels.len(),
        _ => panic!("no picker"),
    };
    assert!(app.act(Action::PickBranch(n)).is_empty());
    app.act(Action::Checkout);
    assert!(
        app.act(Action::PickBranch(n - 1))
            .iter()
            .any(|e| matches!(e, Effect::Jobs(_)))
    );
}

/// A tag of your own: in the Tag menu by its key, toggled there; its
/// icon in the flag column, its name in the details, its line in the
/// legend. One without a key isn't in the menu.
#[test]
fn a_tag_of_your_own_is_set_from_the_tag_menu_and_shown() {
    let (_sb, mut app) = setup(
        "[tags.work]\nkey = 'w'\nicon = 'W'\n[tags.later]\nicon = 'L'\nrepos = ['acme/dirty']\n",
    );
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Repo(i) if app.ws.entries[*i].display_name() == "acme/clean"),
        )
        .unwrap();
    app.on_key(key('t'));
    let items = menu_items(&app);
    assert_eq!(items[..2], ["f tag favourite", "w tag work"], "{items:?}");
    assert!(
        !items.iter().any(|i| i.contains("later")),
        "no key: {items:?}"
    );
    app.on_key(key('w'));
    assert!(app.current().unwrap().flags.has("work"));
    assert_eq!(app.ws.config.tags["work"].repos, ["github.com/acme/clean"]);
    app.on_key(key('t'));
    assert!(menu_items(&app).contains(&"w untag work".to_string()));
    app.on_key(ctrl('g'));
    let out = render(&mut app, 80, 10);
    let row = out.lines().find(|l| l.contains("acme/clean")).unwrap();
    assert!(
        row.contains("W acme/clean") || row.contains("W  acme/clean"),
        "{row}"
    );
    let row = out.lines().find(|l| l.contains("acme/dirty")).unwrap();
    assert!(row.contains('L'), "{row}");
    app.on_key(code(KeyCode::Tab));
    assert!(
        trimmed(&app)
            .iter()
            .any(|l| l.starts_with("flags") && l.contains("work")),
        "{:?}",
        trimmed(&app)
    );
    app.on_key(key('?'));
    let help = render(&mut app, 100, 200);
    assert!(
        help.contains("W  tag: work") && help.contains("L  tag: later"),
        "{help}"
    );
}

/// Marks in a repo's details show on what holds them: the repo's own row
/// (folded or not) and the title, a section's heading, an untracked
/// folder's line. And K from another repo says where it would discard.
#[test]
fn marks_show_on_what_holds_them_and_k_says_where() {
    let (_sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        write(&sb.path("dev/dirty"), "build/a", "a\n");
        write(&sb.path("dev/dirty"), "build/b", "b\n");
    });
    let title = |app: &mut App| render(app, 140, 60).lines().next().unwrap().to_string();
    assert_eq!(marks_of(&mut app, &["acme/dirty"]), "-");
    to_file(&mut app, "README");
    app.on_key(key(' '));
    let rows = ["acme/dirty", "untracked (3)", "unstaged (2)", "staged (1)"];
    assert_eq!(marks_of(&mut app, &rows), "p-p-");
    assert!(
        title(&mut app)
            .trim_end()
            .ends_with(" · marked in details: 1 file"),
        "{}",
        title(&mut app)
    );
    // every file of a section: its heading shows it marked
    to_file(&mut app, "both");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "p-M-");
    assert!(title(&mut app).contains("marked in details: 2 files"));
    // lines count too, and a file in a folder marks the folder partly
    app.on_key(ctrl('g'));
    to_file(&mut app, "README");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+hello there");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &rows), "p-p-");
    assert!(
        title(&mut app).contains("marked in details: 1 line"),
        "{}",
        title(&mut app)
    );
    to_file(&mut app, "build/");
    app.act(Action::ToggleExpand);
    app.cursor += 1;
    app.on_key(key(' '));
    assert_eq!(
        marks_of(&mut app, &["untracked (3)", "build/", "build/a"]),
        "ppM"
    );
    assert!(title(&mut app).contains("marked in details: 1 file, 1 line"));
    // folded away, the repo's row still shows there are some
    app.on_key(ctrl('g'));
    to_file(&mut app, "README");
    app.on_key(key(' '));
    app.act(Action::Top);
    while trimmed(&app).iter().any(|t| t == "M README") {
        app.act(Action::ToggleExpand);
    }
    assert_eq!(marks_of(&mut app, &["acme/dirty", "acme/clean"]), "p-");
    // a marked repo shows that its own way, before its name, beside what's
    // marked in it
    let repo_mark = |app: &mut App, name: &str| {
        let screen = render(app, 140, 60);
        screen.lines().any(|l| l.contains(&format!("•{name}")))
    };
    assert!(!repo_mark(&mut app, "acme/dirty"));
    app.on_key(key(' '));
    assert!(repo_mark(&mut app, "acme/dirty") && !repo_mark(&mut app, "acme/clean"));
    assert_eq!(marks_of(&mut app, &["acme/dirty"]), "p");
    assert!(title(&mut app).contains(" · 1 repo marked · marked in details: 1 file"));
    app.act(Action::Top);
    app.on_key(key(' '));
    // s with the cursor in another repo says where it staged
    cursor_on_detail(&mut app, "clean", "path");
    assert!(!app.on_key(key('s')).is_empty());
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "running 1 job(s)… · in acme/dirty"
    );
    app.running.clear();
    // so does stashing them; stashing a repo says no such thing
    app.on_key(key('z'));
    assert!(!app.on_key(key('m')).is_empty());
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .ends_with(" · in acme/dirty"),
        "{:?}",
        app.message
    );
    app.running.clear();
    app.on_key(ctrl('g'));
    cursor_on_detail(&mut app, "dirty", "path");
    app.on_key(key('z'));
    app.on_key(key('z'));
    assert!(
        !app.message.as_ref().unwrap().0.contains(" · in "),
        "{:?}",
        app.message
    );
    app.running.clear();
    to_file(&mut app, "README");
    app.on_key(key(' '));
    app.act(Action::Top);
    while trimmed(&app).iter().any(|t| t == "M README") {
        app.act(Action::ToggleExpand);
    }
    cursor_on_detail(&mut app, "clean", "path");
    // and K where the files are
    app.on_key(key('K'));
    assert!(
        render(&mut app, 140, 60)
            .contains("In acme/dirty: discard unstaged changes to README? They're lost. (y/n)"),
        "{}",
        render(&mut app, 140, 60)
    );
    app.on_key(key('n'));
    // inverse: in its own repo, it needn't
    cursor_on_detail(&mut app, "dirty", "path");
    app.on_key(key('K'));
    assert!(
        render(&mut app, 140, 60).contains(" Discard unstaged changes to README? They're lost.")
    );
    app.on_key(key('n'));
    assert!(!app.on_key(key('s')).is_empty());
    assert_eq!(app.message.as_ref().unwrap().0, "running 1 job(s)…");
}

/// K on files marked in several repos says so, wherever the cursor is.
#[test]
fn discarding_in_several_repos_says_how_many() {
    let (_sb, mut app) = setup_with("", |sb| {
        write(&sb.path("dev/dirty"), "new", "a\n");
        write(&sb.path("dev/clean"), "other", "b\n");
    });
    cursor_on_detail(&mut app, "dirty", "path");
    to_file(&mut app, "new");
    app.on_key(key(' '));
    cursor_on_detail(&mut app, "clean", "path");
    to_file(&mut app, "other");
    app.on_key(key(' '));
    // all of one repo's changes, some of the other's; and the title says
    // they're in two
    assert_eq!(marks_of(&mut app, &["acme/dirty", "acme/clean"]), "pM");
    let title = render(&mut app, 140, 60)
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(
        title
            .trim_end()
            .ends_with("marked in details: 2 files in 2 repos"),
        "{title}"
    );
    app.on_key(key('K'));
    assert!(
        render(&mut app, 140, 60).contains("In 2 repos: delete 2 untracked files? They're lost."),
        "{}",
        render(&mut app, 140, 60)
    );
}

/// The uncommitted issue's line, which unfolds into the changes, shows
/// what's marked of them as the repo's own row does.
#[test]
fn the_uncommitted_issues_line_shows_what_is_marked() {
    let (_sb, mut app) = setup_with("", |sb| write(&sb.path("dev/dirty"), "new", "a\n"));
    to_file(&mut app, "README");
    let issue = ["issue     2 uncommitted path(s)"];
    assert_eq!(marks_of(&mut app, &issue), "-");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &issue), "p");
    to_file(&mut app, "new");
    app.on_key(key(' '));
    assert_eq!(marks_of(&mut app, &issue), "M");
    // inverse: another line of the details shows nothing
    assert_eq!(marks_of(&mut app, &["fix       stash"]), "-");
}

/// `M` marks the repos listed but for hidden ones shown (`H`), as every
/// "all" leaves those out; `U` unmarks whatever is listed.
#[test]
fn mark_all_leaves_hidden_repos_out() {
    let (_sb, mut app) = setup("tags.ignore.repos = ['acme/feature']");
    app.on_key(key('H'));
    assert_eq!(names(&app).len(), 3);
    app.on_key(key('M'));
    assert_eq!(app.marked.len(), 2);
    assert!(!app.marked.iter().any(|p| p.ends_with("feature")));
    // one marked by hand is unmarked with the rest
    app.act(Action::Bottom);
    while !app.current().unwrap().path.ends_with("feature") {
        app.act(Action::Up);
    }
    app.on_key(key(' '));
    assert_eq!(app.marked.len(), 3);
    app.on_key(key('U'));
    assert!(app.marked.is_empty());
}

/// The stashes heading is in the stash colour while they count as an
/// issue: not once that issue is snoozed.
#[test]
fn the_stashes_heading_is_plain_while_the_issue_is_snoozed() {
    let (_sb, mut app) = setup_with("[tags.ignore.issues]\nstashes = []", |sb| {
        let r = sb.path("dev/clean");
        write(&r, "README", "x\n");
        git(&r, &["stash", "push", "-q", "-m", "s"]);
    });
    let role = |app: &mut App| {
        cursor_on_detail(app, "clean", "stashes");
        let Row::Detail { line, .. } = &app.rows[app.cursor] else {
            panic!("the heading");
        };
        line.parts[0].0
    };
    assert_eq!(role(&mut app), Some(argus_core::badges::Role::Stash));
    app.cursor = issue_row(&app, "stashes");
    app.act(Action::Snooze);
    app.on_key(code(KeyCode::Enter));
    assert_eq!(role(&mut app), Some(argus_core::badges::Role::Dim));
}

/// The legend shows pull's icon in the pull tag's colour, wherever that
/// tag stands among them.
#[test]
fn the_legend_shows_pull_in_its_tags_colour() {
    use argus_core::badges::Role;
    let c = Config::parse("[tags.work]\n").unwrap();
    let tags = c.tag_icons();
    let at = tags.iter().position(|(n, _)| n == "pull").unwrap();
    assert_ne!(at, 0, "not the first, which a wrong lookup would give");
    let icons = argus_core::icons::UNICODE;
    let legend = ui::legend(&icons, &tags);
    let pulled = legend.iter().find(|l| l.2.starts_with("pulled by"));
    assert_eq!(pulled.unwrap().1, Role::Tag(at));
}

/// K on marked lines and files says what goes of each kind, and only of
/// the kinds there are.
#[test]
fn discarding_marked_lines_and_files_says_which_kinds() {
    let (_sb, mut app) = setup_with("", |sb| {
        two_hunks(sb);
        write(&sb.path("dev/dirty"), "README", "changed again\n");
        write(&sb.path("dev/dirty"), "new", "a\n");
    });
    to_file(&mut app, "f");
    app.act(Action::ToggleExpand);
    app.cursor = shown_row(&app, "+added");
    app.on_key(key(' '));
    app.cursor = shown_row(&app, "+line three");
    app.on_key(key(' '));
    let asked = |app: &mut App, file: &str| {
        to_file(app, file);
        app.on_key(key(' '));
        app.on_key(key('K'));
        let screen = render(app, 140, 40);
        app.on_key(key('n'));
        to_file(app, file);
        app.on_key(key(' '));
        let line = screen
            .lines()
            .find(|l| l.contains("Discard"))
            .unwrap_or_default();
        line.trim().to_string()
    };
    assert_eq!(
        asked(&mut app, "README"),
        "Discard 2 marked lines and unstaged changes to README? They're lost. (y/n)"
    );
    assert_eq!(
        asked(&mut app, "new"),
        "Discard 2 marked lines and delete untracked new? They're lost. (y/n)"
    );
}

/// A squash-merged branch shows as merged that way, and deleting it says
/// its commits go (it's forced).
#[test]
fn a_squash_merged_branch_is_shown_and_its_deletion_says_so() {
    let (_sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        git(&r, &["switch", "-q", "-c", "sq"]);
        write(&r, "f", "feature\n");
        commit(&r, "work");
        git(&r, &["push", "-q", "-u", "origin", "sq"]);
        git(&r, &["switch", "-q", "main"]);
        git(&r, &["merge", "-q", "--squash", "sq"]);
        git(&r, &["commit", "-q", "-m", "sq (#1)"]);
        git(&r, &["push", "-q"]);
        git(&r, &["push", "-q", "origin", "--delete", "sq"]);
        git(&r, &["fetch", "-q", "--prune"]);
        git(&r, &["branch", "done"]);
    });
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Repo(i) if app.ws.entries[*i].display_name() == "acme/clean"),
        )
        .unwrap();
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert!(
        t.iter()
            .any(|l| l.contains("sq") && l.ends_with("merged (squashed or rebased)")),
        "{t:?}"
    );
    assert!(
        t.iter()
            .any(|l| l.contains("done") && l.ends_with("  merged")),
        "{t:?}"
    );
    app.cursor = 0;
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Repo(i) if app.ws.entries[*i].display_name() == "acme/clean"),
        )
        .unwrap();
    app.act(Action::DeleteMergedBranches);
    match &app.mode {
        Mode::Confirm { question, .. } => assert!(
            question.starts_with(
                "Delete 2 merged branch(es) (1 squashed or rebased: their commits go): "
            ),
            "{question}"
        ),
        _ => panic!("no question"),
    }
    // all of them that way: no count
    app.on_key(key('n'));
    let clean = app.current().unwrap().path.clone();
    git(&clean, &["branch", "-q", "-d", "done"]);
    refreshed(&mut app, &clean);
    app.cursor = repo_row(&app, &clean);
    app.act(Action::DeleteMergedBranches);
    assert!(matches!(&app.mode, Mode::Confirm { question, .. }
        if question.starts_with("Delete 1 merged branch(es) (squashed or rebased: their commits go): sq?")));
}

/// The cursor on the details line of branch `name` of the current repo.
fn to_branch(app: &mut App, name: &str) {
    app.cursor = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.branch.as_deref() == Some(name)))
        .unwrap_or_else(|| panic!("{name}: {:?}", trimmed(app)))
}

/// `b k` deletes the branch under the cursor, asked first, saying what
/// argus knows of its commits: merged, on a remote, or on none and not
/// found merged (which it may still be). Forced, at the commit seen.
#[test]
fn the_branch_under_the_cursor_is_deleted_saying_what_is_known() {
    let (sb, mut app) = setup_with("", |sb| {
        let r = sb.path("dev/clean");
        // pushed, not merged
        git(&r, &["switch", "-q", "-c", "pushed"]);
        write(&r, "p", "p\n");
        commit(&r, "p");
        git(&r, &["push", "-q", "-u", "origin", "pushed"]);
        // local only: two commits on no remote
        git(&r, &["switch", "-q", "-c", "local", "main"]);
        write(&r, "l", "l\n");
        commit(&r, "l1");
        commit(&r, "l2");
        // squash-merged, its remote branch gone
        git(&r, &["switch", "-q", "-c", "sq", "main"]);
        write(&r, "s", "s\n");
        commit(&r, "s");
        git(&r, &["switch", "-q", "main"]);
        git(&r, &["merge", "-q", "--squash", "sq"]);
        git(&r, &["commit", "-q", "-m", "sq (#1)"]);
        git(&r, &["push", "-q"]);
        git(&r, &["branch", "done"]);
        // its own upstream gone, its commits on another remote branch
        git(&r, &["branch", "copy", "pushed"]);
        git(&r, &["config", "branch.copy.remote", "origin"]);
        git(&r, &["config", "branch.copy.merge", "refs/heads/copy"]);
    });
    let clean = sb.path("dev/clean");
    app.cursor = repo_row(&app, &clean);
    // not on a branch line: said, nothing asked
    assert!(app.act(Action::DeleteBranch).is_empty());
    assert!(
        app.message
            .clone()
            .unwrap()
            .0
            .contains("branch in the details")
    );
    app.on_key(code(KeyCode::Tab));
    let ask = |app: &mut App, name: &str| -> String {
        to_branch(app, name);
        assert!(app.act(Action::DeleteBranch).is_empty());
        match std::mem::replace(&mut app.mode, Mode::Normal) {
            Mode::Confirm { question, .. } => question,
            _ => panic!("{name}: no question: {:?}", app.message),
        }
    };
    assert_eq!(
        ask(&mut app, "done"),
        "Delete branch done? It's merged into main."
    );
    assert_eq!(
        ask(&mut app, "sq"),
        "Delete branch sq? Its changes are in main (squashed or rebased); its commits go."
    );
    assert_eq!(
        ask(&mut app, "pushed"),
        "Delete branch pushed? Its commits are on a remote (origin/pushed): nothing is lost."
    );
    assert_eq!(
        ask(&mut app, "local"),
        "Delete branch local? 2 commits are on no remote, and argus couldn't find them merged \
         into main (they may be, in a way it can't see). They'd be lost."
    );
    assert_eq!(
        ask(&mut app, "copy"),
        "Delete branch copy? Its commits are on a remote: nothing is lost.",
        "not naming its gone upstream"
    );
    // the branch you're on: refused
    to_branch(&mut app, "main");
    assert!(app.act(Action::DeleteBranch).is_empty());
    assert!(!matches!(app.mode, Mode::Confirm { .. }));
    assert!(app.message.clone().unwrap().0.contains("you're on"));
    // n: kept; y: forced, at the commit seen
    to_branch(&mut app, "local");
    app.on_key(key('b'));
    assert!(
        menu_items(&app)
            .iter()
            .any(|i| i.starts_with("k delete the branch under the cursor"))
    );
    app.on_key(key('k'));
    assert!(app.on_key(key('n')).is_empty());
    app.act(Action::DeleteBranch);
    let (path, op) = apply_op(&app.on_key(key('y')));
    let tip = git(&clean, &["rev-parse", "local"]).trim().to_string();
    assert_eq!(
        op,
        Op::DeleteBranch {
            branch: "local".into(),
            force_at: Some(tip)
        }
    );
    assert!(finish(&mut app, path, op));
    assert!(git_out(&clean, &["branch", "--list", "local"]).is_empty());
}

/// Without any remote, nothing is on one: `b k` says the commits are only
/// here.
#[test]
fn deleting_a_branch_of_a_repo_without_a_remote_says_its_commits_are_only_here() {
    let (sb, mut app) = setup_with("", |sb| {
        let r = sb.init("dev/alone");
        commit(&r, "one");
        git(&r, &["switch", "-q", "-c", "side"]);
        write(&r, "f", "x\n");
        commit(&r, "two");
        git(&r, &["switch", "-q", "-"]);
    });
    let alone = sb.path("dev/alone");
    app.cursor = repo_row(&app, &alone);
    app.on_key(code(KeyCode::Tab));
    to_branch(&mut app, "side");
    app.act(Action::DeleteBranch);
    assert!(matches!(&app.mode, Mode::Confirm { question, .. }
        if question.contains("has no remote") && question.ends_with("They'd be lost.")));
}

#[test]
fn a_textconv_diff_is_shown_converted_and_acts_as_its_file() {
    use std::os::unix::fs::PermissionsExt;
    let (_sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        let conv = sb.path("conv.sh");
        std::fs::write(&conv, "#!/bin/sh\ntr -d '\\000' < \"$1\" | tr a-z A-Z\n").unwrap();
        std::fs::set_permissions(&conv, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&d, ".gitattributes", "*.dat diff=upper\n");
        std::fs::write(d.join("f.dat"), b"\0one\ntwo\n").unwrap();
        commit(&d, "data");
        std::fs::write(d.join("f.dat"), b"\0one\nthree\n").unwrap();
        git(
            &d,
            &["config", "diff.upper.textconv", conv.to_str().unwrap()],
        );
    });
    to_file(&mut app, "f.dat");
    app.act(Action::ToggleExpand);
    let t = trimmed(&app);
    let at = t.iter().position(|l| l.ends_with("f.dat")).unwrap();
    assert_eq!(
        t[at + 1..at + 6],
        [
            "converted to text (textconv): staged or discarded as a whole file",
            "@@ -1,2 +1,2 @@",
            "ONE",
            "-TWO",
            "+THREE"
        ]
    );
    // none of it is a hunk or a line to act on by itself
    let of_hunk = |r: &Row| {
        matches!(r, Row::Detail { line, .. }
        if line.diff.as_ref().is_some_and(|d| d.hunk.is_some() || d.line.is_some()))
    };
    assert!(!app.rows.iter().any(of_hunk));
    // on a line of it: the whole file is staged, there being no patch of
    // the real content to apply
    app.cursor = app
        .rows
        .iter()
        .position(
            |r| matches!(r, Row::Detail { line, .. } if line.parts.iter().any(|p| p.1 == "+THREE")),
        )
        .unwrap();
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(
            j[0].op,
            Op::AddTracked {
                paths: vec!["f.dat".into()]
            }
        ),
        other => panic!("{other:?}"),
    }
    app.running.clear();
    // and there's no hunk to edit
    assert!(app.act(Action::EditPatch).is_empty());
    assert!(app.editing.is_none());
    // its lines are body text: `n` from the file goes past them
    to_file(&mut app, "f.dat");
    app.act(Action::NextSection);
    let Row::Detail { line, .. } = &app.rows[app.cursor] else {
        panic!()
    };
    assert!(line.diff.as_ref().is_none_or(|d| d.hunk.is_none()) && !line.is_body());
    assert!(!line.parts.iter().any(|p| p.1.contains("TWO")));
    // every line of the converted diff, its hunk header included
    for text in ["@@ -1,2 +1,2 @@", " ONE", "-TWO", "+THREE"] {
        let body = app.rows.iter().find_map(|r| match r {
            Row::Detail { line, .. } if line.parts.iter().any(|p| p.1 == text) => {
                Some(line.is_body())
            }
            _ => None,
        });
        assert_eq!(body, Some(true), "{text}");
    }
}

#[test]
fn a_commits_textconv_diff_says_it_is_converted() {
    use std::os::unix::fs::PermissionsExt;
    let (sb, mut app) = setup_with("", |sb| {
        let f = sb.path("dev/feature");
        let conv = sb.path("conv.sh");
        std::fs::write(&conv, "#!/bin/sh\ntr -d '\\000' < \"$1\" | tr a-z A-Z\n").unwrap();
        std::fs::set_permissions(&conv, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &f,
            &["config", "diff.upper.textconv", conv.to_str().unwrap()],
        );
        write(&f, ".gitattributes", "*.dat diff=upper\n");
        std::fs::write(f.join("f.dat"), b"\0one\n").unwrap();
        write(&f, "plain", "x\n");
        git(&f, &["add", "."]);
        git(&f, &["commit", "-q", "-m", "Add data"]);
    });
    let f = sb.path("dev/feature");
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit(s)");
    app.act(Action::ToggleExpand);
    let id = git(&f, &["rev-parse", "HEAD"]);
    cursor_on_detail(&mut app, "feature", &format!("      {}", &id[..7]));
    app.act(Action::ToggleExpand);
    cursor_on_detail(&mut app, "feature", "          A f.dat");
    app.act(Action::ToggleExpand);
    assert_eq!(
        below(&app, "          A f.dat", 10),
        ["converted to text (textconv)", "@@ -0,0 +1 @@", "+ONE"]
    );
    // inverse: an ordinary file's diff says nothing of the kind
    cursor_on_detail(&mut app, "feature", "          A plain");
    app.act(Action::ToggleExpand);
    assert_eq!(
        below(&app, "          A plain", 10),
        ["@@ -0,0 +1 @@", "+x"]
    );
}

/// `feature` on `feat` with a commit to push: push, then switch to main.
fn two_step(sb: &Sandbox) {
    commit(&sb.path("dev/feature"), "unpushed");
}

/// Run the fix an `Effect::Fix` asks for, for real, and hand back its
/// messages: done, then the repo's status as refreshed.
fn run_fix(app: &mut App, fx: Vec<Effect>) -> Vec<Effect> {
    let [Effect::Fix { paths, kinds }] = fx.as_slice() else {
        panic!("not one fix: {fx:?}")
    };
    let path = paths[0].clone();
    let applied = argus_core::fix::apply_fix(&path, kinds, &app.ws.config.stash).unwrap();
    let fx = app.on_msg(Msg::FixDone {
        path: path.clone(),
        applied: Ok(applied),
    });
    assert!(matches!(fx.as_slice(), [Effect::Refresh(_)]), "{fx:?}");
    let st = argus_core::status::status(&path, Default::default());
    app.on_msg(Msg::Status(path, Box::new(st.map_err(|e| e.to_string()))))
}

#[test]
fn fix_auto_goes_on_to_the_next_fix_until_none_applies() {
    let (sb, mut app) = setup_with("", two_step);
    let f = sb.path("dev/feature");
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    app.on_key(key('x'));
    let items = menu_items(&app);
    assert_eq!(items[0], "x fix once acme/feature: push");
    assert_eq!(items[2], "X fix auto acme/feature: push, then switch main");
    let fx = app.on_key(key('X'));
    assert_eq!(msg(&app), "fix auto acme/feature: push, then switch main…");
    // pushed: not done yet, and the refreshed status starts the next fix
    let fx = run_fix(&mut app, fx);
    assert!(!msg(&app).ends_with("done"), "{}", msg(&app));
    assert!(app.running.contains_key(&f));
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "feat");
    // switched: nothing more applies, so now it's done
    let fx = run_fix(&mut app, fx);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Fix { .. })),
        "{fx:?}"
    );
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "main");
    assert_eq!(
        msg(&app),
        "fix auto acme/feature: push, then switch main: done"
    );
    assert!(!app.running.contains_key(&f));
    // and a later refresh starts nothing
    let st = argus_core::status::status(&f, Default::default()).unwrap();
    assert!(
        app.on_msg(Msg::Status(f.clone(), Box::new(Ok(st))))
            .is_empty()
    );
}

#[test]
fn fix_once_stops_after_its_one_fix() {
    let (sb, mut app) = setup_with("", two_step);
    let f = sb.path("dev/feature");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let fx = app.act(Action::AutoFix(Scope::Selected));
    let fx = run_fix(&mut app, fx);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Fix { .. })),
        "{fx:?}"
    );
    assert_eq!(git(&f, &["branch", "--show-current"]).trim(), "feat");
    assert_eq!(
        app.message.as_ref().unwrap().0,
        "fix once acme/feature: push: done"
    );
}

#[test]
fn fix_auto_stops_at_a_failure_and_waits_for_a_running_fix() {
    let (sb, mut app) = setup_with("", two_step);
    let f = sb.path("dev/feature");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let status =
        || Box::new(argus_core::status::status(&f, Default::default()).map_err(|e| e.to_string()));
    let fx = app.act(Action::FixAuto(Scope::Selected));
    let [Effect::Fix { kinds, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    // a refresh arriving while the fix runs doesn't start another
    assert!(
        !app.on_msg(Msg::Status(f.clone(), status()))
            .iter()
            .any(|e| matches!(e, Effect::Fix { .. }))
    );
    // the push fails: no switch after it
    let fix = argus_core::fix::choose(
        app.ws.get(&f).unwrap().status().unwrap(),
        kinds,
        &app.ws.config.stash,
    )
    .unwrap();
    app.on_msg(Msg::FixDone {
        path: f.clone(),
        applied: Ok(Some(argus_core::fix::Applied {
            fix,
            outcome: argus_core::ops::OpOutcome {
                success: false,
                code: Some(1),
                output: "rejected".into(),
                duration: Default::default(),
            },
        })),
    });
    assert!(app.message.as_ref().unwrap().0.contains("push failed"));
    assert!(
        !app.on_msg(Msg::Status(f.clone(), status()))
            .iter()
            .any(|e| matches!(e, Effect::Fix { .. }))
    );
    assert!(!app.running.contains_key(&f));
}

#[test]
fn details_for_the_command_line_name_commands_not_keys() {
    use argus_tui::details::{Hints, details};
    let (_sb, app) = setup_with("", |sb| {
        write(&sb.path("dev/feature"), "notes.txt", "untracked\n");
    });
    let lines = |hints: Hints| -> Vec<String> {
        let e = app
            .ws
            .entries
            .iter()
            .find(|e| e.display_name() == "acme/feature")
            .unwrap();
        let c = &app.ws.config;
        details(
            e,
            &c.fix.auto,
            &c.stash,
            &argus_core::icons::UNICODE,
            &app.ws.state,
            hints,
            None,
        )
        .iter()
        .map(|l| l.parts.iter().map(|p| p.1.as_str()).collect::<String>())
        .filter(|l| l.contains("fix  "))
        .collect()
    };
    assert_eq!(
        lines(Hints::Commands { describe_limit: 9 }),
        [
            // what plain `argus fix` runs: no command to name
            "auto-fix  switch main  feat has nothing unpushed",
            "fix       stash  1 uncommitted path(s)  (argus fix -k stash)",
            // needing input the command line can't take: where it can be given
            "fix       commit…  1 uncommitted path(s)  (in the TUI: c A)",
            "fix       gitignore…  1 untracked path(s)  (in the TUI: i)",
        ]
    );
    // inverse: the TUI names keys, and only for those that ask first
    assert_eq!(
        lines(Hints::Keys),
        [
            "auto-fix  switch main  feat has nothing unpushed",
            "fix       stash  1 uncommitted path(s)",
            "fix       commit…  1 uncommitted path(s)  (c A)",
            "fix       gitignore…  1 untracked path(s)  (i)",
        ]
    );
}

/// `feature` open with forge lookups on: the lookup its opening asks for.
fn forge_setup(extra: &str) -> (Sandbox, App, Vec<Effect>) {
    let (sb, mut app) = setup_with(&format!("ui.forge_info = true\n{extra}"), |sb| {
        git(&sb.path("dev/feature"), &["branch", "-q", "old"]);
    });
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let fx = app.act(Action::ToggleExpand);
    (sb, app, fx)
}

fn forge_line(app: &App) -> Option<String> {
    detail_texts(app)
        .into_iter()
        .find(|t| t.starts_with("forge"))
}

fn a_pull(branch: &str, number: u64) -> argus_core::forge::PullRequest {
    argus_core::forge::PullRequest {
        branch: branch.into(),
        number,
        title: format!("Title {number}"),
        state: argus_core::forge::PullState::Open,
        draft: false,
        url: format!("https://github.com/acme/feature/pull/{number}"),
        review: None,
        checks: Some(argus_core::forge::Checks::Failing),
        into: None,
    }
}

#[test]
fn opening_details_asks_the_forge_once_and_shows_what_it_says() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    // asked about the branches that aren't the default, the current first
    let [
        Effect::Forge {
            path,
            kind,
            id,
            branches,
            owners,
        },
    ] = fx.as_slice()
    else {
        panic!("{fx:?}")
    };
    assert!(owners.is_empty(), "no other remote");
    assert_eq!((path, kind.as_str()), (&f, "github"));
    assert_eq!((id.owner.as_str(), id.name.as_str()), ("acme", "feature"));
    assert_eq!(branches, &["feat", "old"]);
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     looking up pull requests and issues…"
    );
    // not asked again while that's on its way, nor once it's answered
    let st = || {
        Box::new(Ok(
            argus_core::status::status(&f, Default::default()).unwrap()
        ))
    };
    assert!(app.on_msg(Msg::Status(f.clone(), st())).is_empty());
    let info = argus_core::forge::RepoInfo {
        open_issues: 1,
        open_pulls: 3,
        pulls: vec![a_pull("feat", 12)],
        parent: None,
    };
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Ok(Some(info)),
    });
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     1 open issue, 3 open pull requests"
    );
    let texts = detail_texts(&app);
    let feat = texts.iter().find(|t| t.starts_with("  * feat")).unwrap();
    assert!(
        feat.ends_with("  #12 open, checks failing  Title 12"),
        "{feat}"
    );
    // inverse: a branch with no pull request says nothing of one
    let old = texts.iter().find(|t| t.starts_with("    old")).unwrap();
    assert!(!old.contains('#'), "{old}");
    assert!(app.on_msg(Msg::Status(f.clone(), st())).is_empty());
    // folding and opening again doesn't ask again either
    app.act(Action::ToggleExpand);
    assert!(app.act(Action::ToggleExpand).is_empty());
    // a new branch is one it hasn't been asked about: asked again, what's
    // known shown meanwhile
    git(&f, &["branch", "-q", "newer"]);
    let fx = app.on_msg(Msg::Status(f.clone(), st()));
    assert!(
        matches!(fx.as_slice(), [Effect::Forge { branches, .. }] if branches == &["feat", "newer", "old"]),
        "{fx:?}"
    );
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     1 open issue, 3 open pull requests"
    );
}

#[test]
fn a_forge_that_fails_or_has_nothing_to_say() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Err("authentication: no token".into()),
    });
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     authentication: no token"
    );
    // refreshing asks again, for the repos whose details are open
    let fx = app.act(Action::Refresh);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Forge { path, .. } if path == &f))
    );
    assert_eq!(
        fx.iter()
            .filter(|e| matches!(e, Effect::Forge { .. }))
            .count(),
        1
    );
    // nothing to say: no line at all
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Ok(None),
    });
    assert_eq!(forge_line(&app), None);
    // counts agree with their nouns
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Ok(Some(argus_core::forge::RepoInfo {
            open_issues: 2,
            open_pulls: 1,
            pulls: vec![],
            parent: Some(argus_core::forge::ParentInfo {
                name: "up/feature".into(),
                open_issues: 9,
                open_pulls: 0,
            }),
        })),
    });
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     up/feature: 9 open issues, 0 open pull requests; this fork: 2 open issues, 1 open pull request"
    );
    // a second remote's owner is one whose pull requests count: asked with
    git(
        &f,
        &["remote", "add", "mine", "git@github.com:me/feature.git"],
    );
    app.ws.refresh(&f);
    let fx = app.act(Action::Refresh);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Forge { owners, .. } if owners == &["me"])),
        "{fx:?}"
    );
}

#[test]
fn the_forge_is_not_asked_when_turned_off_or_unknown() {
    // turned off
    let (_sb, mut app) = setup("ui.forge_info = false");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    assert!(app.act(Action::ToggleExpand).is_empty());
    assert_eq!(forge_line(&app), None);
    assert!(
        !app.act(Action::Refresh)
            .iter()
            .any(|e| matches!(e, Effect::Forge { .. }))
    );
    // a repo with no identity on a forge
    let (_sb, mut app) = setup_with("ui.forge_info = true", |sb| {
        sb.init("dev/local");
    });
    app.act(Action::Top);
    while !app.current().unwrap().path.ends_with("local") {
        app.act(Action::Down);
    }
    assert!(app.act(Action::ToggleExpand).is_empty());
    assert_eq!(forge_line(&app), None);
    // opening every repo asks about each that has one
    let fx = app.act(Action::ExpandAll);
    assert_eq!(
        fx.iter()
            .filter(|e| matches!(e, Effect::Forge { .. }))
            .count(),
        3,
        "{fx:?}"
    );
}

#[test]
fn browse_on_a_branch_with_a_pull_request_opens_it() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Ok(Some(argus_core::forge::RepoInfo {
            pulls: vec![a_pull("feat", 12)],
            ..Default::default()
        })),
    });
    cursor_on_detail(&mut app, "feature", "  * feat");
    let fx = app.act(Action::Browse);
    assert!(
        matches!(fx.as_slice(), [Effect::Open(u)] if u == "https://github.com/acme/feature/pull/12"),
        "{fx:?}"
    );
    // inverse: on a branch without one, and on the repo: the repo's page
    for at in ["    old", "path"] {
        cursor_on_detail(&mut app, "feature", at);
        let fx = app.act(Action::Browse);
        assert!(
            matches!(fx.as_slice(), [Effect::Open(u)] if u == "https://github.com/acme/feature"),
            "{at}: {fx:?}"
        );
    }
}

#[test]
fn a_pull_request_is_coloured_by_how_it_stands() {
    use argus_core::badges::Role;
    use argus_core::forge::{Checks, PullState, Review};
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    let mut role = |change: &dyn Fn(&mut argus_core::forge::PullRequest)| {
        let mut p = a_pull("feat", 12);
        p.checks = None;
        change(&mut p);
        app.on_msg(Msg::Forge {
            path: f.clone(),
            branches: branches.clone(),
            info: Ok(Some(argus_core::forge::RepoInfo {
                pulls: vec![p],
                ..Default::default()
            })),
        });
        app.rows
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. } => {
                    line.parts.iter().find(|p| p.1.contains("#12")).map(|p| p.0)
                }
                _ => None,
            })
            .unwrap()
    };
    // open and nothing wrong: as things ahead are
    assert_eq!(role(&|_| {}), Some(Role::Ahead));
    assert_eq!(
        role(&|p| p.review = Some(Review::Approved)),
        Some(Role::Ahead)
    );
    assert_eq!(
        role(&|p| p.checks = Some(Checks::Pending)),
        Some(Role::Ahead)
    );
    // open with something to do: a warning
    assert_eq!(
        role(&|p| p.review = Some(Review::ChangesRequested)),
        Some(Role::Warning)
    );
    assert_eq!(
        role(&|p| p.checks = Some(Checks::Failing)),
        Some(Role::Warning)
    );
    // over: dim, whatever its checks said
    for state in [PullState::Merged, PullState::Closed] {
        let r = role(&|p| {
            p.state = state;
            p.checks = Some(Checks::Failing);
            p.review = Some(Review::ChangesRequested);
        });
        assert_eq!(r, Some(Role::Dim), "{state:?}");
    }
}

#[test]
fn pull_and_ignore_take_their_tags_keys_in_the_tag_menus() {
    let (_sb, mut app) = setup("[tags.pull]\nkey = 'P'\n[tags.ignore]\nkey = 'x'\n");
    while current(&app) != "acme/clean" {
        app.act(Action::Down);
    }
    app.on_key(key('t'));
    let items = menu_items(&app);
    assert!(
        items.contains(&"P pull (with F a and argus pull)".to_string()),
        "{items:?}"
    );
    assert!(items.contains(&"x ignore repo".to_string()), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|i| i.starts_with("p ") || i.starts_with("I ")),
        "{items:?}"
    );
    // and the keys do it, the label then saying how to undo it
    app.on_key(key('P'));
    assert!(app.current().unwrap().flags.pull);
    app.on_key(key('t'));
    assert!(menu_items(&app).contains(&"P stop pulling".to_string()));
    // the owner's menu has them on the same keys
    app.on_key(key('o'));
    let items = menu_items(&app);
    // each on its own item, not the other's
    let of = |k: &str| items.iter().find(|i| i.starts_with(k)).cloned().unwrap();
    assert!(of("P ").contains("pull"), "{items:?}");
    assert!(of("x ").contains("ignor"), "{items:?}");
    assert!(
        !items
            .iter()
            .any(|i| i.starts_with("p ") || i.starts_with("I ")),
        "{items:?}"
    );
    app.on_key(ctrl('g'));
    // inverse: by default they are where they were
    let (_sb, mut app) = setup("");
    app.on_key(key('t'));
    let items = menu_items(&app);
    assert!(items.iter().any(|i| i.starts_with("p ")) && items.iter().any(|i| i.starts_with("I ")));
}

#[test]
fn a_pull_says_what_became_of_orphans_and_asks_before_following_a_rename() {
    use argus_core::identity::RepoId;
    use argus_core::pull::{Orphan, OrphanState, PullPlan};
    let (sb, mut app) = setup("[tags.pull]\nrepos = ['!acme/*']\n");
    let orphan = |name: &str, state: OrphanState| Orphan {
        path: sb.path(&format!("dev/{name}")),
        id: RepoId::new("github.com", "acme", name),
        forge: "github".into(),
        state,
    };
    let plan = |orphans: Vec<Orphan>| Msg::PullPlanned {
        tracks: app_state(),
        plan: PullPlan {
            orphans,
            ..Default::default()
        },
        errors: vec![],
    };
    fn app_state() -> argus_core::State {
        argus_core::State::default()
    }
    let renamed = OrphanState::Renamed(RepoId::new("github.com", "other", "fresh"));
    let msg = plan(vec![
        orphan("dirty", renamed.clone()),
        orphan("feature", OrphanState::Gone),
    ]);
    let fx = app.on_msg(msg);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Jobs(_))), "{fx:?}");
    let logged: Vec<(String, String)> = app
        .log
        .iter()
        .map(|l| (l.repo.clone(), l.label.clone()))
        .collect();
    assert!(
        logged.contains(&("acme/dirty".into(), "orphan: now other/fresh".into())),
        "{logged:?}"
    );
    assert!(
        logged.contains(&(
            "acme/feature".into(),
            "orphan: gone from the forge, or out of reach (t I ignores it)".into()
        )),
        "{logged:?}"
    );
    // asked first; no leaves the remote as it is
    let Mode::Confirm { question, .. } = &app.mode else {
        panic!("not asked")
    };
    assert_eq!(
        question,
        "acme/dirty is now other/fresh on the forge: point its remote there?"
    );
    assert!(app.on_key(key('n')).is_empty());
    assert!(matches!(app.mode, Mode::Normal));
    // yes runs it
    app.on_msg(plan(vec![orphan("dirty", renamed.clone())]));
    let fx = app.on_key(key('y'));
    match fx.as_slice() {
        [Effect::Jobs(j)] => {
            assert_eq!(j.len(), 1);
            assert_eq!(j[0].path, sb.path("dev/dirty"));
            assert_eq!(
                j[0].op,
                Op::SetRemoteUrl {
                    remote: "origin".into(),
                    url: "https://github.com/other/fresh.git".into()
                }
            );
        }
        other => panic!("{other:?}"),
    }
    app.running.clear();
    // several: counted; and with none renamed, nothing is asked
    app.on_msg(plan(vec![
        orphan("dirty", renamed.clone()),
        orphan("clean", renamed),
    ]));
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question == "2 repos have new names on the forge (acme/dirty, acme/clean): point their remotes there?")
    );
    app.on_key(key('n'));
    app.on_msg(plan(vec![orphan("feature", OrphanState::Gone)]));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn changes_are_listed_by_section_each_file_with_that_sides_letter() {
    use argus_core::badges::Role;
    use argus_tui::details::Section;
    let (_sb, mut app) = setup_with("", diff_fixture);
    to_file(&mut app, "both");
    // from the first section's line down: text, section, whether it heads one
    let from = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if Section::heads(line).is_some()))
        .unwrap();
    let listed: Vec<(String, Option<Section>, bool)> = app.rows[from..]
        .iter()
        .map_while(|r| match r {
            Row::Detail { line, .. } => Some((
                line.parts.iter().map(|p| p.1.as_str()).collect::<String>(),
                line.section,
                Section::heads(line).is_some(),
            )),
            Row::Repo(_) => None,
        })
        .collect();
    let want = [
        ("untracked (2)", Some(Section::Untracked), true),
        ("    bin", Some(Section::Untracked), false),
        ("    new", Some(Section::Untracked), false),
        ("unstaged (2)", Some(Section::Unstaged), true),
        ("    M README", Some(Section::Unstaged), false),
        ("    M both", Some(Section::Unstaged), false),
        ("staged (1)", Some(Section::Staged), true),
        ("    M both", Some(Section::Staged), false),
    ];
    let want: Vec<_> = want
        .iter()
        .map(|(t, s, h)| (t.to_string(), *s, *h))
        .collect();
    assert_eq!(listed, want);
    // each letter in its side's colour; an untracked path in its own
    let role = |text: &str, section: Section| {
        app.rows.iter().find_map(|r| match r {
            Row::Detail { line, .. }
                if line.section == Some(section)
                    && line.parts.iter().map(|p| p.1.as_str()).collect::<String>() == text =>
            {
                line.parts[1].0
            }
            _ => None,
        })
    };
    assert_eq!(role("    M both", Section::Unstaged), Some(Role::Unstaged));
    assert_eq!(role("    M both", Section::Staged), Some(Role::Staged));
    assert_eq!(role("    new", Section::Untracked), Some(Role::Untracked));
    // no blank lines until a diff is open: then one after it, setting
    // what follows apart, whether a file or a section
    assert!(!trimmed(&app).contains(&String::new()));
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let at = t.iter().position(|l| l == "M README").unwrap();
    assert_eq!(
        t[at + 1..at + 6],
        ["@@ -1 +1 @@", "-hello world", "+hello there", "", "M both"]
    );
    assert_eq!(t.iter().filter(|l| l.is_empty()).count(), 1);
    to_file(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let at = t.iter().rposition(|l| l == "+three").unwrap();
    assert_eq!(t[at + 1..at + 3], ["", "staged (1)"]);
    // it's the diff's own: no file line (marked or acted on as one)
    let after: Vec<_> = app
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Detail { line, .. } if line.parts.iter().all(|p| p.1.trim().is_empty()) => {
                Some(line.diff.as_ref().map(|d| (d.side, d.hunk, d.at)))
            }
            _ => None,
        })
        .collect();
    let side = argus_core::diff::Side::Unstaged;
    assert_eq!(after, [Some((side, None, None)), Some((side, None, None))]);
    // a blank line is nothing to stop at
    let blank =
        |line: &argus_tui::details::DetailLine| line.parts.iter().all(|p| p.1.trim().is_empty());
    assert!(app.rows.iter().all(|r| match r {
        Row::Detail { line, .. } => !blank(line) || line.is_body(),
        Row::Repo(_) => true,
    }));
    // the last diff of all has nothing after it to set apart
    to_staged(&mut app, "both");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert_eq!(t.last().map(String::as_str), Some("+two"), "{t:?}");
}

#[test]
fn forge_names_counts_and_pull_requests_are_links() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    let answer = |app: &mut App, parent: Option<argus_core::forge::ParentInfo>| {
        app.on_msg(Msg::Forge {
            path: f.clone(),
            branches: branches.clone(),
            info: Ok(Some(argus_core::forge::RepoInfo {
                open_issues: 2,
                open_pulls: 1,
                pulls: vec![a_pull("feat", 12)],
                parent,
            })),
        });
    };
    // each linked part of the line starting with `start`: its text and URL
    let links = |app: &App, start: &str| -> Vec<(String, String)> {
        app.rows
            .iter()
            .find_map(|r| match r {
                Row::Detail { line, .. } => {
                    let text: String = line.parts.iter().map(|p| p.1.as_str()).collect();
                    text.trim_start().starts_with(start).then(|| {
                        line.links
                            .iter()
                            .map(|(i, url)| (line.parts[*i].1.trim().to_string(), url.clone()))
                            .collect()
                    })
                }
                _ => None,
            })
            .unwrap()
    };
    let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
    answer(&mut app, None);
    assert_eq!(
        links(&app, "forge"),
        [
            pair("2 open issues", "https://github.com/acme/feature/issues"),
            pair(
                "1 open pull request",
                "https://github.com/acme/feature/pulls"
            ),
        ]
    );
    assert_eq!(
        links(&app, "* feat"),
        [pair(
            "#12 open, checks failing",
            "https://github.com/acme/feature/pull/12"
        )]
    );
    // a fork: its parent's name and counts, then its own
    answer(
        &mut app,
        Some(argus_core::forge::ParentInfo {
            name: "up/feature".into(),
            open_issues: 9,
            open_pulls: 0,
        }),
    );
    assert_eq!(
        forge_line(&app).unwrap(),
        "forge     up/feature: 9 open issues, 0 open pull requests; this fork: 2 open issues, 1 open pull request"
    );
    assert_eq!(
        links(&app, "forge"),
        [
            pair("up/feature", "https://github.com/up/feature"),
            pair("9 open issues", "https://github.com/up/feature/issues"),
            pair(
                "0 open pull requests",
                "https://github.com/up/feature/pulls"
            ),
            pair("2 open issues", "https://github.com/acme/feature/issues"),
            pair(
                "1 open pull request",
                "https://github.com/acme/feature/pulls"
            ),
        ]
    );
    // inverse: other lines link nothing
    assert!(links(&app, "path").is_empty());
    assert!(links(&app, "old").is_empty());
}

#[test]
fn a_forks_own_counts_show_only_when_it_has_any() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    let mut line = |issues: usize, pulls: usize| {
        app.on_msg(Msg::Forge {
            path: f.clone(),
            branches: branches.clone(),
            info: Ok(Some(argus_core::forge::RepoInfo {
                open_issues: issues,
                open_pulls: pulls,
                pulls: vec![],
                parent: Some(argus_core::forge::ParentInfo {
                    name: "up/feature".into(),
                    open_issues: 9,
                    open_pulls: 4,
                }),
            })),
        });
        forge_line(&app).unwrap()
    };
    let theirs = "forge     up/feature: 9 open issues, 4 open pull requests";
    assert_eq!(line(0, 0), theirs);
    // either kind is enough, and both
    assert_eq!(
        line(2, 0),
        format!("{theirs}; this fork: 2 open issues, 0 open pull requests")
    );
    assert_eq!(
        line(0, 1),
        format!("{theirs}; this fork: 0 open issues, 1 open pull request")
    );
    assert_eq!(
        line(1, 1),
        format!("{theirs}; this fork: 1 open issue, 1 open pull request")
    );
}

#[test]
fn stashes_past_the_tenth_are_counted_and_fold_with_the_rest() {
    let (_sb, mut app) = setup_with("", |sb| {
        let d = sb.path("dev/dirty");
        for n in 0..11 {
            write(&d, "README", &format!("stash {n}\n"));
            git(&d, &["stash", "push", "-q", "-m", &format!("s{n}")]);
        }
    });
    cursor_on_detail(&mut app, "dirty", "stashes");
    let t = trimmed(&app);
    assert_eq!(t.iter().filter(|l| l.starts_with("stash@{")).count(), 10);
    assert!(t.contains(&"… 1 more".to_string()), "{t:?}");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert!(t.iter().any(|l| l.starts_with("stashes (11")));
    assert!(
        !t.iter()
            .any(|l| l.starts_with("stash@{") || l.contains('…')),
        "{t:?}"
    );
}

#[test]
fn links_are_drawn_over_the_text_they_link() {
    let (sb, mut app, fx) = forge_setup("");
    let f = sb.path("dev/feature");
    let [Effect::Forge { branches, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    app.on_msg(Msg::Forge {
        path: f.clone(),
        branches: branches.clone(),
        info: Ok(Some(argus_core::forge::RepoInfo {
            open_issues: 2,
            open_pulls: 1,
            pulls: vec![a_pull("feat", 12)],
            parent: None,
        })),
    });
    let (theme, _) = Theme::build("mono", &Default::default());
    let draw = |app: &mut App, w: u16, h: u16| {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let view = ui::View {
            theme: &theme,
            icons: argus_core::icons::UNICODE,
            tick: 0,
        };
        let mut links = vec![];
        t.draw(|f| links = ui::draw(f, app, &view)).unwrap();
        let buf = t.backend().buffer().clone();
        (links, buf)
    };
    // what a style comes to on screen (a cell spells out "no colour")
    let drawn = |s: ratatui::style::Style| {
        use ratatui::style::Color::Reset;
        (s.fg.unwrap_or(Reset), s.bg.unwrap_or(Reset), s.add_modifier)
    };
    let (links, buf) = draw(&mut app, 120, 40);
    let got: Vec<(&str, &str)> = links
        .iter()
        .map(|l| (l.text.as_str(), l.url.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("2 open issues", "https://github.com/acme/feature/issues"),
            (
                "1 open pull request",
                "https://github.com/acme/feature/pulls"
            ),
            (
                "#12 open, checks failing",
                "https://github.com/acme/feature/pull/12"
            ),
        ]
    );
    // each sits exactly on the cells that show its text, in their style
    for l in &links {
        let shown: String = (0..l.text.chars().count() as u16)
            .map(|dx| buf[(l.x + dx, l.y)].symbol().to_string())
            .collect();
        assert_eq!(shown, l.text);
        assert_eq!(drawn(buf[(l.x, l.y)].style()), drawn(l.style), "{}", l.text);
    }
    // what's written over them: there, as a link, and closed again
    let mut out = vec![];
    ui::write_links(&mut out, &links[2..]).unwrap();
    let out = String::from_utf8(out).unwrap();
    let l = &links[2];
    assert!(
        out.starts_with(&format!("\x1b[{};{}H", l.y + 1, l.x + 1)),
        "{out:?}"
    );
    assert!(
        out.contains("\x1b]8;;https://github.com/acme/feature/pull/12\x1b\\#12 open, checks failing\x1b]8;;\x1b\\"),
        "{out:?}"
    );
    assert!(out.ends_with("\x1b[0m"), "{out:?}");
    let mut none = vec![];
    ui::write_links(&mut none, &[]).unwrap();
    assert!(none.is_empty());
    // a link whose address or text could end the sequence early isn't
    // written at all
    for (url, text) in [
        ("https://x/\x1b\\\x1b]0;t\x07", "#12"),
        ("file:///etc/passwd", "#12"),
        ("https://x/1", "#12\x1b[2J"),
    ] {
        let mut bad = links[2].clone();
        (bad.url, bad.text) = (url.into(), text.into());
        let mut out = vec![];
        ui::write_links(&mut out, &[bad]).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains("]8;;") && !out.contains("]0;"), "{out:?}");
    }
    // under the cursor, the link takes the selection's style with the row
    cursor_on_detail(&mut app, "feature", "  * feat");
    let (links, buf) = draw(&mut app, 120, 40);
    let l = links.iter().find(|l| l.text.starts_with("#12")).unwrap();
    assert_eq!(drawn(buf[(l.x, l.y)].style()), drawn(l.style));
    assert!(
        l.style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)
            || l.style.bg.is_some(),
        "the selection's: {:?}",
        l.style
    );
    // scrolled, a link is where its row now is on screen
    let (links, buf) = draw(&mut app, 120, 8);
    assert!(
        app.scroll > 0,
        "the list is scrolled to keep the cursor in view"
    );
    let l = links.iter().find(|l| l.text.starts_with("#12")).unwrap();
    let shown: String = (0..l.text.chars().count() as u16)
        .map(|dx| buf[(l.x + dx, l.y)].symbol().to_string())
        .collect();
    assert_eq!(shown, l.text);
    // a link the width cuts short isn't one; nor any under a menu
    let (links, _) = draw(&mut app, 50, 40);
    assert!(
        !links.iter().any(|l| l.text.starts_with("#12")),
        "{links:?}"
    );
    app.on_key(key('t'));
    assert!(draw(&mut app, 120, 40).0.is_empty());
}

#[test]
fn the_cursor_moves_on_as_magit_does_when_its_line_goes() {
    use argus_tui::details::Section;
    let (sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        // another repo's changes, open too, are no part of it
        write(&sb.path("dev/clean"), "README", "changed\n");
        write(&sb.path("dev/clean"), "zz", "new\n");
    });
    app.act(Action::ExpandAll);
    let d = sb.path("dev/dirty");
    let in_dirty = |app: &App| app.current().unwrap().path.ends_with("dirty");
    // the cursor on dirty's own line of a section, or of a file in it
    let dirty_line = |app: &mut App, section: Section, file: Option<&str>| {
        app.cursor = (0..app.rows.len())
            .find(|&n| {
                matches!(&app.rows[n], Row::Detail { repo, line }
                if app.ws.entries[*repo].path.ends_with("dirty")
                    && line.section == Some(section)
                    && line.diff.is_none()
                    && line.file.as_deref() == file)
            })
            .unwrap();
    };
    let here = |app: &App| match &app.rows[app.cursor] {
        Row::Detail { line, .. } => {
            let text: String = line.parts.iter().map(|p| p.1.as_str()).collect();
            (text.trim().to_string(), line.section)
        }
        Row::Repo(i) => (app.ws.entries[*i].display_name(), None),
    };
    let after = |app: &mut App, args: &[&str]| {
        git(&d, args);
        app.ws.refresh(&d);
        app.rebuild();
    };
    let at = |text: &str, section| (text.to_string(), Some(section));
    // a file staged: on to the next file of its section, not after it
    dirty_line(&mut app, Section::Unstaged, Some("README"));
    after(&mut app, &["add", "README"]);
    assert_eq!(here(&app), at("M both", Section::Unstaged));
    // the last of its section, with none after: the one before
    after(&mut app, &["reset", "-q", "README"]);
    dirty_line(&mut app, Section::Unstaged, Some("both"));
    after(&mut app, &["add", "both"]);
    assert_eq!(here(&app), at("M README", Section::Unstaged));
    assert!(in_dirty(&app), "its own repo's, not another's");
    // the first section gone, two after it: the next, not the last
    dirty_line(&mut app, Section::Untracked, None);
    after(&mut app, &["add", "bin", "new"]);
    assert_eq!(here(&app), at("unstaged (1)", Section::Unstaged));
    assert!(in_dirty(&app));
    after(&mut app, &["reset", "-q", "bin", "new"]);
    // the section's last file: the section is gone, on to the next one
    dirty_line(&mut app, Section::Unstaged, Some("README"));
    after(&mut app, &["add", "README"]);
    assert_eq!(here(&app), at("staged (2)", Section::Staged));
    assert!(in_dirty(&app));
    // a section gone from its own line, none after it: the one before
    after(&mut app, &["reset", "-q"]);
    dirty_line(&mut app, Section::Unstaged, None);
    after(&mut app, &["checkout", "--", "."]);
    assert_eq!(here(&app), at("untracked (2)", Section::Untracked));
    assert!(in_dirty(&app));
    // in a diff: its hunk gone with its file, on to the next file
    after(&mut app, &["add", "-N", "bin", "new"]);
    dirty_line(&mut app, Section::Unstaged, Some("bin"));
    after(&mut app, &["add", "bin"]);
    assert_eq!(here(&app), at("A new", Section::Unstaged));
    // no section left at all: the line before them
    after(&mut app, &["add", "new"]);
    dirty_line(&mut app, Section::Staged, None);
    after(&mut app, &["commit", "-q", "-m", "all"]);
    assert_eq!(here(&app), ("branches (1)".into(), None));
    // a hunk folded away under the cursor, another file after its own in
    // the section: the hunk's own line
    write(&d, "README", "one more\n");
    write(&d, "both", "and this\n");
    app.ws.refresh(&d);
    app.rebuild();
    dirty_line(&mut app, Section::Unstaged, Some("README"));
    app.on_key(code(KeyCode::Tab));
    app.cursor += 2;
    assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. } if line.diff.is_some()));
    fold_hunk_under_cursor(&mut app);
    assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. }
        if line.file.as_deref() == Some("README")
            && line.diff.as_ref().is_some_and(|d| d.hunk.is_some() && d.line.is_none())));
    assert!(in_dirty(&app));
}

/// A repo's status read again and handed in, as the runtime does.
fn status_arrives(app: &mut App, repo: &std::path::Path) {
    let st = argus_core::status::status(repo, Default::default()).unwrap();
    app.on_msg(Msg::Status(repo.to_path_buf(), Box::new(Ok(st))));
}

/// `f` of dirty with three changes far apart: three hunks.
fn three_hunks(sb: &Sandbox) {
    let d = sb.path("dev/dirty");
    let lines: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    write(&d, "f", &lines);
    commit(&d, "f");
    let changed = lines
        .replace("line 3\n", "line three\n")
        .replace("line 20\n", "line twenty\n")
        .replace("line 37\n", "line 37\nadded\n");
    write(&d, "f", &changed);
}

#[test]
fn the_cursor_stays_on_any_line_when_a_status_comes_back() {
    let (sb, mut app) = setup_with("", three_hunks);
    let d = sb.path("dev/dirty");
    let text = |app: &App| trimmed(app)[app.cursor_line()].clone();
    for start in [
        "remote",
        "activity",
        "issue",
        "auto-fix",
        "branches",
        "unstaged (1)",
    ] {
        cursor_on_detail(&mut app, "dirty", start);
        let before = (app.cursor, text(&app));
        status_arrives(&mut app, &d);
        assert_eq!((app.cursor, text(&app)), before, "{start}");
    }
    // a line of a commit's message, two levels down
    cursor_on_detail(&mut app, "dirty", "issue     1 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    app.cursor += 1;
    app.on_key(code(KeyCode::Tab));
    let msg = trimmed(&app)
        .iter()
        .position(|t| t == "f")
        .unwrap_or_else(|| panic!("{:?}", trimmed(&app)));
    app.cursor = app.row_of_detail(msg);
    status_arrives(&mut app, &d);
    assert_eq!((app.cursor_line(), text(&app).as_str()), (msg, "f"));
    // inverse: the line gone (the remote taken away), the nearest left
    cursor_on_detail(&mut app, "dirty", "remote");
    git(&d, &["remote", "remove", "origin"]);
    status_arrives(&mut app, &d);
    assert!(text(&app).starts_with("commit "), "{}", text(&app));
    assert!(app.current().unwrap().path.ends_with("dirty"));
}

#[test]
fn a_folded_hunk_stays_folded_when_one_before_it_is_staged() {
    let (sb, mut app) = setup_with("", three_hunks);
    let d = sb.path("dev/dirty");
    cursor_on_detail(&mut app, "dirty", "path");
    to_file(&mut app, "f");
    app.on_key(code(KeyCode::Tab));
    let hunks = |app: &App| -> Vec<(String, bool)> {
        // each hunk's header, and whether its lines show
        let t = trimmed(app);
        let at = |i: &usize| t[*i].starts_with("@@");
        let next = |i: usize| {
            t.get(i + 1)
                .is_some_and(|l| !l.starts_with("@@") && !l.is_empty())
        };
        (0..t.len())
            .filter(at)
            .map(|i| (t[i].clone(), next(i)))
            .collect()
    };
    let shows = |app: &App| hunks(app).into_iter().map(|h| h.1).collect::<Vec<_>>();
    assert_eq!(shows(&app), [true, true, true]);
    // the second folded
    let second = trimmed(&app)
        .iter()
        .position(|t| t == "+line twenty")
        .unwrap();
    app.cursor = app.row_of_detail(second);
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shows(&app), [true, false, true]);
    let folded = hunks(&app)[1].0.clone();
    assert_eq!(trimmed(&app)[app.cursor_line()], folded, "on its header");
    // the first staged (its lines, from outside): the folded one is first
    // now, and still the folded one; the cursor is still on it
    let patch = "--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n line 1\n line 2\n-line 3\n+line three\n line 4\n line 5\n line 6\n";
    std::fs::write(d.join("p"), patch).unwrap();
    git(&d, &["apply", "--cached", "p"]);
    std::fs::remove_file(d.join("p")).unwrap();
    status_arrives(&mut app, &d);
    let now = hunks(&app);
    assert_eq!(shows(&app), [false, true], "{now:?}");
    assert!(trimmed(&app)[app.cursor_line()].starts_with("@@"));
    assert_eq!(
        app.cursor_line(),
        trimmed(&app).iter().position(|t| *t == now[0].0).unwrap()
    );
    // inverse: unfolded again, nothing is left folded
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shows(&app), [true, true]);
}

#[test]
fn a_hunk_changed_under_the_cursor_keeps_it_and_its_line() {
    let (sb, mut app) = setup_with("", three_hunks);
    let d = sb.path("dev/dirty");
    cursor_on_detail(&mut app, "dirty", "path");
    to_file(&mut app, "f");
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    let second = t.iter().position(|t| t == "+line twenty").unwrap();
    app.cursor = app.row_of_detail(second);
    // the hunk becomes another (more changed in it): the line where the
    // cursor was in it, not the next hunk or the file
    let text = std::fs::read_to_string(d.join("f")).unwrap();
    write(&d, "f", &text.replace("line 21\n", "line twenty-one\n"));
    status_arrives(&mut app, &d);
    assert_eq!(app.cursor_line(), second, "{:?}", trimmed(&app));
    assert_eq!(trimmed(&app)[second], "-line 21", "the same place in it");
    // inverse: the hunk gone, the next one's header
    let text = std::fs::read_to_string(d.join("f")).unwrap();
    let back = text
        .replace("line twenty\n", "line 20\n")
        .replace("line twenty-one\n", "line 21\n");
    write(&d, "f", &back);
    status_arrives(&mut app, &d);
    let t = trimmed(&app);
    assert!(
        t[app.cursor_line()].starts_with("@@"),
        "{}",
        t[app.cursor_line()]
    );
    assert_eq!(t[app.cursor_line() + 4], "+added", "{t:?}");
    // the last hunk gone, something after the diff (so a blank line ends
    // it): the hunk before, not the blank line
    write(&d, "staged", "x\n");
    git(&d, &["add", "staged"]);
    status_arrives(&mut app, &d);
    let t = trimmed(&app);
    assert_eq!(t[app.cursor_line() + 4], "+added", "{t:?}");
    assert!(t.contains(&String::new()), "{t:?}");
    let text = std::fs::read_to_string(d.join("f")).unwrap();
    write(&d, "f", &text.replace("added\n", ""));
    status_arrives(&mut app, &d);
    let t = trimmed(&app);
    assert_eq!(t[app.cursor_line()], "@@ -1,6 +1,6 @@", "{t:?}");
}

#[test]
fn what_was_unfolded_under_a_line_is_forgotten_when_the_line_goes() {
    let (sb, mut app) = setup_with("", |_| {});
    let d = sb.path("dev/dirty");
    cursor_on_detail(&mut app, "dirty", "path");
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let open = |app: &App| trimmed(app).iter().any(|t| t.starts_with("@@"));
    assert!(open(&app));
    // inverse: a status with the file still changed leaves its diff open
    write(&d, "other", "x\n");
    status_arrives(&mut app, &d);
    assert!(open(&app));
    // committed, then changed again: folded, as any file starts
    git(&d, &["commit", "-qam", "it"]);
    status_arrives(&mut app, &d);
    write(&d, "README", "changed again\n");
    status_arrives(&mut app, &d);
    assert!(trimmed(&app).iter().any(|t| t == "M README"));
    assert!(!open(&app), "{:?}", trimmed(&app));
    assert!(app.toggled.is_empty(), "{:?}", app.toggled); // the same under the uncommitted issue, and in a nested repo
    let n = d.join("nested");
    std::fs::create_dir_all(&n).unwrap();
    git(&n, &["init", "-q"]);
    write(&n, "a", "1\n");
    commit(&n, "a");
    write(&n, "a", "2\n");
    status_arrives(&mut app, &d);
    cursor_on_detail(&mut app, "dirty", "issue     3 uncommitted path(s)");
    app.on_key(code(KeyCode::Tab));
    app.cursor = (0..app.rows.len())
        .find(|&r| matches!(&app.rows[r], Row::Detail { line, .. } if line.file.as_deref() == Some("README")))
        .unwrap();
    assert_eq!(trimmed(&app)[app.cursor_line()], "M README");
    app.on_key(code(KeyCode::Tab));
    assert!(open(&app));
    cursor_on_detail(&mut app, "dirty", "    nested");
    app.on_key(code(KeyCode::Tab));
    let inner = (0..app.rows.len())
        .find(|&r| matches!(&app.rows[r], Row::Detail { line, .. } if line.file.as_deref() == Some("a")))
        .unwrap();
    app.cursor = inner;
    app.on_key(code(KeyCode::Tab));
    assert_eq!(
        trimmed(&app).iter().filter(|t| t.starts_with("@@")).count(),
        2
    );
    // both files committed, then changed again: neither diff comes back
    git(&d, &["commit", "-qam", "it"]);
    git(&n, &["commit", "-qam", "it"]);
    let nested = n.canonicalize().unwrap();
    status_arrives(&mut app, &nested);
    status_arrives(&mut app, &d);
    write(&d, "README", "and again\n");
    write(&n, "a", "3\n");
    status_arrives(&mut app, &nested);
    status_arrives(&mut app, &d);
    assert!(!open(&app), "{:?}", trimmed(&app));
}

#[test]
fn tab_on_a_line_with_nothing_under_it_folds_what_it_is_under() {
    let (sb, mut app) = setup_with("", |_| {});
    let d = sb.path("dev/dirty");
    // a branch with nothing to unfold: the branches
    cursor_on_detail(&mut app, "dirty", "  * main");
    app.on_key(code(KeyCode::Tab));
    assert!(app.expanded.contains(&d));
    let t = trimmed(&app);
    assert!(!t.iter().any(|t| t.starts_with("* main")), "{t:?}");
    assert_eq!(t[app.cursor_line()], "branches (1)");
    app.on_key(code(KeyCode::Tab));
    assert!(trimmed(&app).iter().any(|t| t.starts_with("* main")));
    // inverse: a line under nothing folds the repo
    cursor_on_detail(&mut app, "dirty", "path");
    app.on_key(code(KeyCode::Tab));
    assert!(!app.expanded.contains(&d));
    assert!(matches!(app.rows[app.cursor], Row::Repo(_)));
}

/// The last file of a section staged: the cursor goes to the line now
/// where its section was, not after the file into another section.
#[test]
fn staging_a_sections_last_file_moves_on_to_the_next_line_not_after_the_file() {
    let (sb, mut app) = setup_with("", |sb| {
        write(&sb.path("dev/clean"), "zz", "new\n");
        write(&sb.path("dev/dirty"), "zz", "new\n");
    });
    app.act(Action::ExpandAll);
    let text = |app: &App| trimmed(app)[app.cursor_line()].clone();
    let stage = |app: &mut App, name: &str| {
        let repo = sb.path(&format!("dev/{name}"));
        app.cursor = (0..app.rows.len())
            .find(|&n| {
                matches!(&app.rows[n], Row::Detail { repo: r, line }
                    if app.ws.entries[*r].path == repo && line.file.as_deref() == Some("zz"))
            })
            .unwrap();
        git(&repo, &["add", "zz"]);
        status_arrives(app, &repo);
        assert!(app.current().unwrap().path.ends_with(name));
    };
    // nothing else changed: the staged section, new, is where untracked was
    stage(&mut app, "clean");
    assert_eq!(text(&app), "staged (1)");
    // unstaged changes too: on to them
    stage(&mut app, "dirty");
    assert_eq!(text(&app), "unstaged (1)");
}

/// The "uncommitted paths" issue unfolds into the sections of changes,
/// as they are further down: the same files, acting the same, folding
/// by themselves.
#[test]
fn the_uncommitted_issue_unfolds_into_the_sections() {
    let (_sb, mut app) = setup_with("", diff_fixture);
    cursor_on_detail(&mut app, "dirty", "issue     5 uncommitted path(s)");
    let count = |app: &App, text: &str| trimmed(app).iter().filter(|t| *t == text).count();
    for line in ["untracked (2)", "unstaged (2)", "staged (1)", "M README"] {
        assert_eq!(count(&app, line), 1, "{line}: {:?}", trimmed(&app));
    }
    app.on_key(code(KeyCode::Tab));
    for line in ["untracked (2)", "unstaged (2)", "staged (1)", "M README"] {
        assert_eq!(count(&app, line), 2, "{line}: {:?}", trimmed(&app));
    }
    // under the issue, further in than it, files further in than sections
    let under = below(&app, "issue     5 uncommitted path(s)", 4);
    assert_eq!(under[..3], ["untracked (2)", "bin", "new"], "{under:?}");
    let texts = detail_texts(&app);
    let col = |text: &str| {
        let l = texts.iter().find(|t| t.trim() == text).unwrap();
        l.len() - l.trim_start().len()
    };
    assert!(col("untracked (2)") > col("issue     5 uncommitted path(s)"));
    // a file there unfolds to its diff, further in than the file; the one
    // further down stays folded
    let first = trimmed(&app).iter().position(|t| t == "M README").unwrap();
    app.cursor = app.row_of_detail(first);
    app.on_key(code(KeyCode::Tab));
    let t = detail_texts(&app);
    assert_eq!(t.iter().filter(|t| t.trim().starts_with("@@")).count(), 1);
    assert!(t[first + 1].trim().starts_with("@@"), "{t:?}");
    let indent = |l: &String| l.len() - l.trim_start().len();
    assert!(indent(&t[first + 1]) > indent(&t[first]), "{t:?}");
    // drawn as any diff line is, its changed words picked out or not
    let screen = render(&mut app, 120, 60);
    for l in ["-hello world", "+hello there"] {
        assert_eq!(
            screen.lines().filter(|s| s.trim() == l).count(),
            1,
            "{screen}"
        );
    }
    // and stages like the other
    app.cursor = app.row_of_detail(first);
    match app.act(Action::StageFiles).as_slice() {
        [Effect::Jobs(j)] => assert_eq!(j[0].op.label(), "stage README"),
        other => panic!("{other:?}"),
    }
    // Tab on the issue folds them again; the sections below are untouched
    cursor_on_detail(&mut app, "dirty", "issue     5 uncommitted path(s)");
    app.on_key(code(KeyCode::Tab));
    assert_eq!(count(&app, "unstaged (2)"), 1);
    assert!(app.current().unwrap().path.ends_with("dirty"));
}

/// The stash menu offers what there is something to do with.
#[test]
fn the_stash_menu_offers_only_what_applies() {
    let (sb, mut app) = setup("");
    let d = sb.path("dev/dirty");
    let offered = |app: &mut App| -> Vec<(char, String)> {
        app.on_key(key('z'));
        let items = match &app.mode {
            Mode::Menu { menu, .. } => menu
                .items
                .iter()
                .map(|i| (i.key, i.label.clone()))
                .collect(),
            _ => panic!("no menu"),
        };
        app.on_key(code(KeyCode::Esc));
        items
    };
    let keys = |app: &mut App| -> String { offered(app).iter().map(|i| i.0).collect() };
    // a star on the first two switches and on what goes by them
    cursor_on_detail(&mut app, "dirty", "path");
    git(&d, &["add", "README"]);
    status_arrives(&mut app, &d);
    let labels: Vec<String> = offered(&mut app).into_iter().map(|i| i.1).collect();
    assert_eq!(
        labels,
        [
            "include untracked*",
            "keep index*",
            "message",
            "stash all*",
            "stash staged"
        ]
    );
    git(&d, &["reset", "-q"]);
    status_arrives(&mut app, &d);
    // unstaged changes only: no staged, marked, pop or drop
    cursor_on_detail(&mut app, "dirty", "path");
    assert_eq!(keys(&mut app), "ukMz");
    // something staged
    git(&d, &["add", "README"]);
    status_arrives(&mut app, &d);
    assert_eq!(keys(&mut app), "ukMzg");
    // a file marked
    git(&d, &["reset", "-q"]);
    status_arrives(&mut app, &d);
    to_file(&mut app, "README");
    app.act(Action::ToggleMark);
    let items = offered(&mut app);
    assert!(items.contains(&('m', "stash marked*".into())), "{items:?}");
    app.marked_files.clear();
    // a stash: pop and drop, asking which unless the cursor is on one
    git(&d, &["stash", "push", "-q", "-m", "mine"]);
    write(&d, "README", "more\n");
    status_arrives(&mut app, &d);
    cursor_on_detail(&mut app, "dirty", "path");
    let items = offered(&mut app);
    assert_eq!(keys(&mut app), "ukMzpd", "{:?}", trimmed(&app));
    // (one stash: nothing to ask before popping it)
    assert!(items.contains(&('p', "pop".into())), "{items:?}");
    cursor_on_detail(&mut app, "dirty", "    stash@{0}");
    let items = offered(&mut app);
    assert_eq!(keys(&mut app), "ukMzpd");
    assert!(items.contains(&('p', "pop".into())), "{items:?}");
    assert!(items.contains(&('d', "drop…".into())), "{items:?}");
}

/// A file's diff open in two places (under the uncommitted issue and in
/// its section): a region is of the copy the cursor is in.
#[test]
fn a_region_stays_in_the_copy_of_the_diff_it_was_started_in() {
    let (_sb, mut app) = setup_with("", three_hunks);
    cursor_on_detail(&mut app, "dirty", "issue     1 uncommitted path(s)");
    app.on_key(code(KeyCode::Tab));
    let files: Vec<usize> = (0..app.rows.len())
        .filter(|&n| {
            matches!(&app.rows[n], Row::Detail { line, .. }
            if line.file.as_deref() == Some("f") && line.diff.is_none())
        })
        .collect();
    assert_eq!(files.len(), 2, "under the issue, and in its section");
    // (the lower first, so the rows above it don't move)
    for n in files.iter().rev() {
        app.cursor = *n;
        app.on_key(code(KeyCode::Tab));
    }
    let copies: Vec<usize> = (0..app.rows.len())
        .filter(|&n| {
            matches!(&app.rows[n], Row::Detail { line, .. }
            if line.diff.as_ref().and_then(|d| d.line.as_ref()).is_some_and(|l| l.text == "line 3"))
        })
        .collect();
    assert_eq!(copies.len(), 2);
    for start in copies {
        // from "-line 3" to "+line three", the next line
        app.cursor = start;
        app.on_key(ctrl(' '));
        app.on_key(code(KeyCode::Down));
        let lines = app.region_lines();
        assert_eq!(lines.len(), 2, "{start}: {lines:?}");
        app.on_key(ctrl(' '));
        app.marked_lines.clear();
    }
    // A region is of the diff, from where it starts to where the cursor
    // is in it, wherever the cursor goes: past the end of its copy (into
    // the other copy, onto another repo) is the diff's end, before it the
    // diff's start. From "+line twenty" (the fourth changed line of five):
    let twenty: Vec<usize> = (0..app.rows.len())
        .filter(|&n| matches!(&app.rows[n], Row::Detail { line, .. }
            if line.diff.as_ref().and_then(|d| d.line.as_ref()).is_some_and(|l| l.text == "line twenty")))
        .collect();
    assert_eq!(twenty.len(), 2);
    let last = app.rows.len() - 1;
    let ats = |app: &App| -> Vec<(usize, usize)> {
        app.region_lines().iter().map(|l| (l.3, l.4)).collect()
    };
    for (n, start) in twenty.iter().enumerate() {
        app.cursor = *start;
        app.on_key(ctrl(' '));
        let own = ats(&app);
        assert_eq!(own.len(), 1, "{own:?}");
        // every row below the copy: the two lines to the diff's end
        let after: Vec<usize> = match n {
            0 => vec![twenty[1] - 2, twenty[1], twenty[1] + 3, last],
            _ => vec![last],
        };
        for row in after {
            app.cursor = row;
            let lines = ats(&app);
            assert_eq!(lines.len(), 2, "copy {n}, row {row}: {lines:?}");
            assert_eq!(lines[0], own[0]);
        }
        // every row above it: the four lines from the diff's start
        let before: Vec<usize> = match n {
            0 => vec![0, 1],
            _ => vec![0, twenty[0] - 3, twenty[0], twenty[0] + 2],
        };
        for row in before {
            app.cursor = row;
            let lines = ats(&app);
            assert_eq!(lines.len(), 4, "copy {n}, row {row}: {lines:?}");
            assert_eq!(lines[3], own[0]);
        }
        // on the hunk's header in its own copy: nothing of the hunk
        // before the start but what's between
        app.cursor = start - 5;
        assert!(matches!(&app.rows[app.cursor], Row::Detail { line, .. }
            if line.diff.as_ref().is_some_and(|d| d.hunk == Some(1) && d.line.is_none())));
        assert_eq!(
            ats(&app).len(),
            2,
            "the hunk's two changed lines up to the start"
        );
        // on the blank line that ends its copy's diff (when anything
        // follows it): the diff's end; on its file's own line: its start
        let blank = (*start..app.rows.len()).find(|&r| {
            matches!(&app.rows[r], Row::Detail { line, .. } if line.id.last() == Some(&Seg::Blank))
        });
        if n == 0 {
            app.cursor = blank.expect("sections follow the issue's copy");
            assert_eq!(ats(&app).len(), 2, "to the end");
        }
        let own_file = (0..*start)
            .rev()
            .find(|&r| {
                matches!(&app.rows[r], Row::Detail { line, .. }
                if line.file.as_deref() == Some("f") && line.diff.is_none())
            })
            .unwrap();
        app.cursor = own_file;
        assert_eq!(ats(&app).len(), 4, "from the start");
        // its diff folded: the same, the lines being the diff's, shown or not
        let file = (0..*start)
            .rev()
            .find(|&r| {
                matches!(&app.rows[r], Row::Detail { line, .. }
                if line.file.as_deref() == Some("f") && line.diff.is_none())
            })
            .unwrap();
        app.cursor = file;
        app.on_key(code(KeyCode::Tab));
        app.cursor = app.rows.len() - 1;
        assert_eq!(ats(&app).len(), 2);
        app.cursor = file;
        app.on_key(code(KeyCode::Tab));
        // what its file is under folded, the file's line gone with it:
        // no further than where it started, wherever the cursor is
        let under = match &app.rows[file] {
            Row::Detail { line, .. } => line.id[..line.id.len() - 1].to_vec(),
            r => panic!("{r:?}"),
        };
        let parent = (0..file)
            .rev()
            .find(|&r| matches!(&app.rows[r], Row::Detail { line, .. } if line.id == under))
            .unwrap();
        app.cursor = parent;
        app.on_key(code(KeyCode::Tab));
        for row in [0, parent, app.rows.len() - 1] {
            app.cursor = row;
            assert_eq!(ats(&app), own, "{row}");
        }
        app.cursor = parent;
        app.on_key(code(KeyCode::Tab));
        app.on_key(ctrl(' '));
        app.marked_lines.clear();
    }
}

/// Two repos nested in one work tree, one's path ending as the other's
/// does: each unfolds into itself.
#[test]
fn nested_repos_with_like_names_unfold_into_themselves() {
    let (sb, mut app) = setup_with("", |sb| {
        let c = sb.path("dev/clean");
        for (rel, file) in [("b/sub", "inner"), ("sub", "outer")] {
            let n = c.join(rel);
            std::fs::create_dir_all(&n).unwrap();
            git(&n, &["init", "-q"]);
            write(&n, file, "1\n");
            commit(&n, file);
            write(&n, file, "2\n");
        }
    });
    cursor_on_detail(&mut app, "clean", "    b/");
    app.on_key(code(KeyCode::Tab));
    cursor_on_detail(&mut app, "clean", "      b/sub");
    app.on_key(code(KeyCode::Tab));
    assert!(below(&app, "      b/sub", 6).iter().any(|l| l == "M inner"));
    cursor_on_detail(&mut app, "clean", "    sub");
    app.on_key(code(KeyCode::Tab));
    let under = below(&app, "    sub", 4);
    assert!(under.iter().any(|l| l == "M outer"), "{under:?}");
    let own = sb.path("dev/clean/sub").canonicalize().unwrap();
    let at = app.cursor + 1;
    assert!(matches!(&app.rows[at], Row::Detail { repo, .. } if app.ws.entries[*repo].path == own));
}

/// A rescan reads everything again: what was unfolded shows what's there
/// now, not what was read before.
#[test]
fn a_rescan_reads_what_is_unfolded_again() {
    let (sb, mut app) = setup_with("", |_| {});
    let d = sb.path("dev/dirty");
    cursor_on_detail(&mut app, "dirty", "issue     1 uncommitted path(s)");
    app.on_key(code(KeyCode::Tab));
    let count = |app: &App, text: &str| trimmed(app).iter().filter(|t| *t == text).count();
    assert_eq!(count(&app, "M README"), 2);
    write(&d, "zz", "new\n");
    app.ws.refresh(&d);
    let entries = app.ws.entries.clone();
    app.on_msg(Msg::Scanned {
        entries,
        truncated: vec![],
    });
    assert_eq!(count(&app, "zz"), 2, "{:?}", trimmed(&app));
}

/// A stash unfolded, then dropped; an issue unfolded, then resolved: both
/// forgotten. One still there stays unfolded.
#[test]
fn what_was_unfolded_under_a_stash_or_an_issue_goes_with_it() {
    let (sb, mut app) = setup_with("", |_| {});
    let d = sb.path("dev/dirty");
    git(&d, &["stash", "push", "-q", "-m", "one"]);
    write(&d, "README", "again\n");
    status_arrives(&mut app, &d);
    cursor_on_detail(&mut app, "dirty", "    stash@{0}");
    app.on_key(code(KeyCode::Tab));
    cursor_on_detail(&mut app, "dirty", "issue     1 uncommitted path(s)");
    app.on_key(code(KeyCode::Tab));
    let kinds = |app: &App| -> Vec<&'static str> {
        let ids = app.toggled.values().flatten();
        ids.map(|id| match id.first() {
            Some(Seg::Stashes) => "stash",
            Some(Seg::Issue(..)) => "issue",
            _ => "other",
        })
        .collect()
    };
    assert_eq!(kinds(&app), ["issue", "stash"]);
    // inverse: a status with both still there keeps both
    status_arrives(&mut app, &d);
    assert_eq!(kinds(&app), ["issue", "stash"]);
    git(&d, &["stash", "drop", "-q"]);
    status_arrives(&mut app, &d);
    assert_eq!(kinds(&app), ["issue"]);
    git(&d, &["checkout", "-q", "--", "README"]);
    status_arrives(&mut app, &d);
    assert!(app.toggled.is_empty(), "{:?}", app.toggled);
}

/// Fold the hunk whose line the cursor is on, as if from elsewhere: the
/// cursor is left to find its way.
fn fold_hunk_under_cursor(app: &mut App) {
    let root = app.current().unwrap().path.clone();
    let Row::Detail { line, .. } = &app.rows[app.cursor] else {
        panic!()
    };
    let hunk = line.id[..line.id.len() - 1].to_vec();
    assert!(matches!(hunk.last(), Some(Seg::Hunk(..))), "{hunk:?}");
    app.toggled.entry(root).or_default().insert(hunk);
    app.rebuild();
}

#[test]
fn tab_folds_what_the_line_belongs_to_not_the_whole_repo() {
    let (sb, mut app) = setup_with("", |sb| {
        diff_fixture(sb);
        let d = sb.path("dev/dirty");
        git(&d, &["branch", "other"]);
        git(&d, &["stash", "push", "-q", "-m", "kept", "--", "README"]);
        write(&d, "README", "hello there\n");
        // a commit to unfold: two files
        write(&d, "c1", "one\n");
        write(&d, "c2", "two\n");
        git(&d, &["add", "c1", "c2"]);
        git(&d, &["commit", "-q", "-m", "two files", "--", "c1", "c2"]);
    });
    let d = sb.path("dev/dirty");
    to_file(&mut app, "README");
    let open = |app: &App| app.expanded.contains(&d);
    let on = |app: &mut App, start: &str| {
        cursor_on_detail(app, "dirty", start);
    };
    let has = |app: &App, text: &str| trimmed(app).iter().any(|t| t.starts_with(text));
    // a heading folds its own lines, and unfolds them; the repo stays open
    for (heading, member) in [
        ("branches", "other"),
        ("stashes", "stash@{0}"),
        ("unstaged", "M README"),
    ] {
        on(&mut app, heading);
        assert!(has(&app, member), "{heading}: open by default");
        app.on_key(code(KeyCode::Tab));
        assert!(open(&app), "{heading}");
        assert!(!has(&app, member), "{heading} folded: {:?}", trimmed(&app));
        assert!(has(&app, heading), "its heading stays");
        assert!(trimmed(&app)[app.cursor_line()].starts_with(heading));
        app.on_key(code(KeyCode::Tab));
        assert!(has(&app, member), "{heading} unfolded");
    }
    // inverse: a line with nothing of its own to fold still folds the repo
    on(&mut app, "path");
    app.on_key(code(KeyCode::Tab));
    assert!(!open(&app));
    app.on_key(code(KeyCode::Tab));
    // the blank line after a diff folds that diff, leaving its file's line
    to_file(&mut app, "README");
    app.on_key(code(KeyCode::Tab));
    let blank = app
        .rows
        .iter()
        .position(|r| matches!(r, Row::Detail { line, .. } if line.parts.iter().all(|p| p.1.trim().is_empty())))
        .expect("a blank line after the diff");
    app.cursor = blank;
    app.on_key(code(KeyCode::Tab));
    assert!(open(&app));
    assert!(!has(&app, "+hello there"), "{:?}", trimmed(&app));
    assert_eq!(trimmed(&app)[app.cursor_line()], "M README");
    assert!(!trimmed(&app).contains(&String::new()));
    // a commit's files: their heading folds them; a file's diff ends with
    // a blank line when another file follows, and Tab on the diff or the
    // blank folds it back to the file
    on(&mut app, "issue     2 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    app.on_key(key('n'));
    app.on_key(code(KeyCode::Tab));
    on(&mut app, "        files (2)");
    app.on_key(code(KeyCode::Tab));
    assert!(open(&app) && !has(&app, "A c1"), "{:?}", trimmed(&app));
    assert!(has(&app, "author "), "the commit itself stays unfolded");
    assert!(has(&app, "files (2)"), "its heading stays");
    // (nothing unfolded so far is a diff: no blank line has been added)
    assert!(
        !trimmed(&app).contains(&String::new()),
        "{:?}",
        trimmed(&app)
    );
    app.on_key(code(KeyCode::Tab));
    assert!(has(&app, "A c1") && has(&app, "A c2"));
    assert!(
        !trimmed(&app).contains(&String::new()),
        "files, no diff yet"
    );
    let c1 = trimmed(&app).iter().position(|t| t == "A c1").unwrap();
    app.cursor = app.row_of_detail(c1);
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert_eq!(t[c1 + 1..c1 + 5], ["@@ -0,0 +1 @@", "+one", "", "A c2"]);
    // on a line of the diff: its hunk folds, as a changed file's does
    app.cursor = app.row_of_detail(c1 + 2);
    app.on_key(code(KeyCode::Tab));
    assert_eq!(trimmed(&app)[c1 + 1..c1 + 4], ["@@ -0,0 +1 @@", "", "A c2"]);
    assert_eq!(app.cursor_line(), c1 + 1, "on the hunk's line");
    app.on_key(code(KeyCode::Tab));
    assert_eq!(trimmed(&app)[c1 + 1..c1 + 3], ["@@ -0,0 +1 @@", "+one"]);
    // on the blank line after it: the diff folds
    app.cursor = app.row_of_detail(c1 + 3);
    app.on_key(code(KeyCode::Tab));
    assert!(open(&app) && !has(&app, "+one"), "{:?}", trimmed(&app));
    assert_eq!(trimmed(&app)[app.cursor_line()], "A c1");
    assert!(has(&app, "A c2"), "its commit stays");
    // the last file's diff has nothing after it in the commit to set apart
    let c2 = trimmed(&app).iter().position(|t| t == "A c2").unwrap();
    app.cursor = app.row_of_detail(c2);
    app.on_key(code(KeyCode::Tab));
    let t = trimmed(&app);
    assert_eq!(t[c2 + 1..c2 + 3], ["@@ -0,0 +1 @@", "+two"]);
    // a line of a commit's message folds the commit
    let msg = trimmed(&app).iter().position(|t| t == "two files").unwrap();
    app.cursor = app.row_of_detail(msg);
    app.on_key(code(KeyCode::Tab));
    assert!(open(&app) && !has(&app, "files (2)"), "{:?}", trimmed(&app));
}

/// Places among the detail rows, for tests that work from their text.
trait DetailRows {
    /// The cursor's place among the detail lines (as `trimmed` lists them).
    fn cursor_line(&self) -> usize;
    /// The row of the `n`th detail line.
    fn row_of_detail(&self, n: usize) -> usize;
}

impl DetailRows for App {
    fn cursor_line(&self) -> usize {
        self.rows[..self.cursor]
            .iter()
            .filter(|r| matches!(r, Row::Detail { .. }))
            .count()
    }

    fn row_of_detail(&self, n: usize) -> usize {
        (0..self.rows.len())
            .filter(|&i| matches!(self.rows[i], Row::Detail { .. }))
            .nth(n)
            .unwrap()
    }
}

#[test]
fn the_same_commits_under_an_issue_and_under_their_branch_fold_separately() {
    let (sb, mut app) = setup_with("", |sb| {
        commit(&sb.path("dev/feature"), "unpushed work");
    });
    let f = sb.path("dev/feature");
    let id = git(&f, &["rev-parse", "--short=7", "HEAD"]);
    let commit_line = format!("{} unpushed work", id.trim());
    let shown =
        |app: &App, start: &str| trimmed(app).iter().filter(|t| t.starts_with(start)).count();
    // unfolding the branch's commits leaves the issue's folded, and the
    // cursor on the branch
    cursor_on_detail(&mut app, "feature", "  * feat");
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shown(&app, &commit_line), 1, "{:?}", trimmed(&app));
    assert!(trimmed(&app)[app.cursor_line()].starts_with("* feat"));
    assert!(trimmed(&app)[app.cursor_line() + 1].starts_with(&commit_line));
    // and the other way round: the issue's, with the branch's as they were
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shown(&app, &commit_line), 2);
    assert!(trimmed(&app)[app.cursor_line()].starts_with("issue     1 unpushed"));
    // a commit unfolded under the branch isn't under the issue
    let under_branch = |app: &App| {
        let t = trimmed(app);
        let branch = t.iter().position(|l| l.starts_with("* feat")).unwrap();
        branch + 1
    };
    app.cursor = app.row_of_detail(under_branch(&app));
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shown(&app, "author "), 1, "{:?}", trimmed(&app));
    assert_eq!(
        app.cursor_line(),
        under_branch(&app),
        "the cursor stays put"
    );
    let t = trimmed(&app);
    assert!(t[under_branch(&app) + 1].starts_with("commit "), "{t:?}");
    // from a line of its message, Tab folds it: there, the cursor on the
    // branch's copy of the commit, not the issue's
    let msg = t.iter().rposition(|l| l == "unpushed work").unwrap();
    assert!(msg > under_branch(&app));
    app.cursor = app.row_of_detail(msg);
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shown(&app, "author "), 0);
    assert_eq!(app.cursor_line(), under_branch(&app));
    // folding the issue's commits leaves the branch's
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    assert_eq!(shown(&app, &commit_line), 1);
    assert!(trimmed(&app)[under_branch(&app)].starts_with(&commit_line));
    // a status coming back keeps the cursor on the branch's copy
    app.cursor = app.row_of_detail(under_branch(&app));
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit");
    app.on_key(code(KeyCode::Tab));
    app.cursor = app.row_of_detail(under_branch(&app));
    let st = argus_core::status::status(&f, Default::default()).unwrap();
    app.on_msg(Msg::Status(f.clone(), Box::new(Ok(st))));
    assert_eq!(app.cursor_line(), under_branch(&app));
}

#[test]
fn every_line_says_where_it_is() {
    // each line has its place among its repo's details, which what it
    // unfolds into is under
    let id_of = |app: &App, start: &str| -> Option<Vec<Seg>> {
        app.rows.iter().find_map(|r| match r {
            Row::Detail { line, .. } => {
                let text: String = line.parts.iter().map(|p| p.1.as_str()).collect();
                text.trim_start()
                    .starts_with(start)
                    .then(|| line.id.clone())
            }
            _ => None,
        })
    };
    let prep = |sb: &Sandbox| {
        let f = sb.path("dev/feature");
        commit(&f, "unpushed work");
        write(&f, "README", "stashed\n");
        git(&f, &["stash", "push", "-q", "-m", "mine"]);
    };
    let (_sb, mut app) = setup_with("[tags.ignore.issues]\nstashes = []\n", prep);
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit");
    let at = |start: &str| id_of(&app, start).unwrap_or_else(|| panic!("{start}"));
    let issue = |name: &'static str, shown: &'static str| vec![Seg::Issue(name, shown)];
    let branch = |name: &str| vec![Seg::Branches, Seg::Branch(name.into())];
    assert_eq!(at("issue     1 unpushed"), issue("unpushed", ""));
    assert_eq!(at("issue     1 stash"), issue("stashes", ""));
    assert_eq!(at("* feat"), branch("feat"));
    assert_eq!(at("main"), branch("main"));
    assert!(
        matches!(&at("stash@{0}")[..], [Seg::Stashes, Seg::Stash(id)] if id.len() >= 40),
        "by its commit"
    );
    assert_eq!(at("path"), [Seg::Info("path")]);
    assert_eq!(at("branches"), [Seg::Branches]);
    // an ignored issue's line, and a snoozed one's: their own
    let (_sb, mut app) = setup_with(
        "[tags.ignore.issues]\nstashes = ['**']\nnon_default_branch = ['**']\n",
        prep,
    );
    cursor_on_detail(&mut app, "feature", "issue     1 unpushed commit");
    let line = "issue     not on default branch (main)  (ignored)";
    assert_eq!(
        id_of(&app, line).unwrap(),
        issue("non_default_branch", "ignored")
    );
    let key = app.current().unwrap().key.clone();
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    app.ws
        .state
        .snooze_issue(&key, argus_core::attention::Issue::Unpushed(0), until);
    app.ws.resolve_flags();
    app.rebuild();
    assert_eq!(
        id_of(&app, "issue     1 unpushed commit(s)  (snoozed").unwrap(),
        issue("unpushed", "snoozed")
    );
    // what a line unfolds into is under it
    cursor_on_detail(&mut app, "feature", "  * feat");
    app.on_key(code(KeyCode::Tab));
    let below = match &app.rows[app.cursor + 1] {
        Row::Detail { line, .. } => line.id.clone(),
        r => panic!("{r:?}"),
    };
    assert!(
        matches!(&below[..], [Seg::Branches, Seg::Branch(b), Seg::Commit(_)] if b == "feat"),
        "{below:?}"
    );
    // and no two lines of a repo are in the same place
    for name in ["feature", "dirty", "clean"] {
        cursor_on_detail(&mut app, name, "path");
        let ids: Vec<&Vec<Seg>> = app
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Detail { line, repo } if app.ws.entries[*repo].path.ends_with(name) => {
                    Some(&line.id)
                }
                _ => None,
            })
            .collect();
        let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(distinct.len(), ids.len(), "{name}: {ids:?}");
        assert!(ids.iter().all(|id| !id.is_empty()));
    }
}

#[test]
fn a_question_from_the_background_waits_for_the_user_and_its_answer_for_a_busy_repo() {
    use argus_core::identity::RepoId;
    use argus_core::pull::{Orphan, OrphanState, PullPlan};
    // dirty is pulled, so the pull that finds it renamed is running in it
    let (sb, mut app) = setup("[tags.pull]\nrepos = ['acme/dirty']\n");
    let d = sb.path("dev/dirty");
    let renamed = || Msg::PullPlanned {
        tracks: argus_core::State::default(),
        plan: PullPlan {
            orphans: vec![Orphan {
                path: d.clone(),
                id: RepoId::new("github.com", "acme", "dirty"),
                forge: "github".into(),
                state: OrphanState::Renamed(RepoId::new("github.com", "other", "fresh")),
            }],
            ..Default::default()
        },
        errors: vec![],
    };
    // while a filter is being typed, the answer arrives: the typing goes on
    app.on_key(key('/'));
    app.on_key(key('d'));
    assert!(matches!(app.mode, Mode::Input { .. }));
    let fx = app.on_msg(renamed());
    assert!(matches!(app.mode, Mode::Input { .. }), "not replaced");
    assert!(app.running.contains_key(&d), "its pull has started: {fx:?}");
    app.on_key(key('i'));
    assert_eq!(app.filter, "di");
    // and the question is asked once that's finished with
    app.on_key(code(KeyCode::Enter));
    assert!(
        matches!(&app.mode, Mode::Confirm { question, .. } if question.starts_with("acme/dirty is now other/fresh")),
    );
    // yes, with its pull still running: not dropped as busy, but started
    // when the pull is done
    let fx = app.on_key(key('y'));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Jobs(_))), "{fx:?}");
    assert!(
        app.message
            .as_ref()
            .unwrap()
            .0
            .contains("1 to follow once its repo's job is done"),
        "{:?}",
        app.message
    );
    let other = sb.path("dev/clean");
    let finished = |path: &std::path::PathBuf| Msg::JobFinished {
        path: path.clone(),
        label: "pull".into(),
        op: Op::Fetch,
        outcome: argus_core::ops::OpOutcome {
            success: true,
            code: Some(0),
            output: String::new(),
            duration: Default::default(),
        },
    };
    // (another repo's job ending isn't it)
    let fx = app.on_msg(finished(&other));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Jobs(_))), "{fx:?}");
    let fx = app.on_msg(finished(&d));
    let set_url = fx.iter().find_map(|e| match e {
        Effect::Jobs(j) => Some(j[0].op.clone()),
        _ => None,
    });
    assert_eq!(
        set_url,
        Some(Op::SetRemoteUrl {
            remote: "origin".into(),
            url: "https://github.com/other/fresh.git".into()
        })
    );
    // once, not again at the next job's end
    app.running.clear();
    let fx = app.on_msg(finished(&d));
    assert!(!fx.iter().any(|e| matches!(e, Effect::Jobs(_))), "{fx:?}");
    // two questions wait their turn
    app.on_key(key('/'));
    app.on_msg(renamed());
    app.on_msg(renamed());
    app.on_key(code(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    app.on_key(key('n'));
    assert!(matches!(app.mode, Mode::Confirm { .. }), "the second");
    app.on_key(key('n'));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn fix_auto_reads_again_when_a_status_still_wants_the_fix_just_applied() {
    let (sb, mut app) = setup_with("", two_step);
    let f = sb.path("dev/feature");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    // read before the push finishes, delivered after it
    let stale = argus_core::status::status(&f, Default::default()).unwrap();
    let fx = app.act(Action::FixAuto(Scope::Selected));
    let [Effect::Fix { paths, kinds }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    let applied = argus_core::fix::apply_fix(&paths[0], kinds, &app.ws.config.stash).unwrap();
    app.on_msg(Msg::FixDone {
        path: f.clone(),
        applied: Ok(applied),
    });
    let fx = app.on_msg(Msg::Status(f.clone(), Box::new(Ok(stale.clone()))));
    // not taken as the end: read again, the chain still going
    assert!(
        matches!(fx.as_slice(), [Effect::Refresh(p)] if p == &vec![f.clone()]),
        "{fx:?}"
    );
    assert!(!app.message.as_ref().unwrap().0.ends_with("done"));
    // the true status goes on to the next fix
    let fresh = argus_core::status::status(&f, Default::default()).unwrap();
    let fx = app.on_msg(Msg::Status(f.clone(), Box::new(Ok(fresh))));
    assert!(fx.iter().any(|e| matches!(e, Effect::Fix { .. })), "{fx:?}");
    // inverse: wanted again a second time, it isn't making progress: done
    let (sb, mut app) = setup_with("", two_step);
    let f = sb.path("dev/feature");
    while current(&app) != "acme/feature" {
        app.act(Action::Down);
    }
    let before = argus_core::status::status(&f, Default::default()).unwrap();
    let fx = app.act(Action::FixAuto(Scope::Selected));
    let [Effect::Fix { kinds, .. }] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    let fix = argus_core::fix::choose(&before, kinds, &app.ws.config.stash).unwrap();
    app.on_msg(Msg::FixDone {
        path: f.clone(),
        applied: Ok(Some(argus_core::fix::Applied {
            fix,
            outcome: argus_core::ops::OpOutcome {
                success: true,
                code: Some(0),
                output: String::new(),
                duration: Default::default(),
            },
        })),
    });
    let again = || Msg::Status(f.clone(), Box::new(Ok(before.clone())));
    assert!(matches!(
        app.on_msg(again()).as_slice(),
        [Effect::Refresh(_)]
    ));
    assert!(app.on_msg(again()).is_empty());
    assert!(app.message.as_ref().unwrap().0.ends_with(": done"));
    assert!(!app.running.contains_key(&f));
}

#[test]
fn two_fix_actions_running_together_finish_as_one() {
    let (sb, mut app) = setup_with("", |sb| {
        two_step(sb);
        // clean and dirty on a pushed branch each: switch-default applies
        for n in ["clean", "dirty"] {
            let r = sb.path(&format!("dev/{n}"));
            git(&r, &["checkout", "-q", "--", "."]);
            git(&r, &["switch", "-q", "-c", "topic"]);
            git(&r, &["push", "-q", "-u", "origin", "topic"]);
        }
    });
    let msg = |app: &App| app.message.as_ref().unwrap().0.clone();
    let on = |app: &mut App, name: &str| {
        app.act(Action::Top);
        while current(app) != name {
            app.act(Action::Down);
        }
    };
    on(&mut app, "acme/feature");
    assert!(!app.act(Action::FixAuto(Scope::Selected)).is_empty());
    on(&mut app, "acme/clean");
    assert!(!app.act(Action::AutoFix(Scope::Selected)).is_empty());
    let done = |app: &mut App, n: &str| {
        app.on_msg(Msg::FixDone {
            path: sb.path(&format!("dev/{n}")),
            applied: Ok(None),
        });
    };
    // the first to finish isn't the end, whichever action it was
    done(&mut app, "feature");
    assert!(!msg(&app).ends_with("done"), "{}", msg(&app));
    done(&mut app, "clean");
    assert_eq!(msg(&app), "fixes: done");
}
