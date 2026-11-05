//! Rendering. Reads [`App`], never mutates anything but scroll bookkeeping.

use argus_core::ViewFilter;
use argus_core::badges::{self, Icons, Role};
use argus_core::discover::RepoKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode, Prompt, Row};
use crate::menu::MenuId;
use crate::theme::{Theme, Ui};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct View<'a> {
    pub theme: &'a Theme,
    pub icons: Icons,
    pub tick: usize,
}

pub fn draw(f: &mut Frame, app: &mut App, v: &View) {
    let area = f.area();
    let log_h = if app.show_log {
        (area.height / 3).max(6)
    } else {
        0
    };
    let [main, log, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(log_h),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_list(f, app, v, main);
    if app.show_log {
        draw_log(f, app, v, log);
    }
    draw_status(f, app, v, status);
    match &app.mode {
        Mode::Menu { .. } => draw_menu(f, app, v, main),
        Mode::Help { scroll } => draw_help(f, v, main, *scroll),
        _ => {}
    }
}

fn draw_list(f: &mut Frame, app: &mut App, v: &View, area: Rect) {
    let t = v.theme;
    let header_text = {
        let n = app
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Repo(_)))
            .count();
        let attn = app
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Repo(i) if app.ws.entries[*i].needs_attention()))
            .count();
        let mut s = if app.view.attention_only {
            let total = app
                .ws
                .ordered(ViewFilter {
                    attention_only: false,
                    ..app.view
                })
                .len();
            format!(" argus  {attn} of {total} repos need attention · a: show all")
        } else {
            format!(" argus  {n} repos · {attn} need attention")
        };
        if app.view.show_hidden {
            s.push_str(" · showing hidden");
        }
        if !app.filter.is_empty() {
            s.push_str(&format!(" · filter: {}", app.filter));
        }
        if !app.marked.is_empty() {
            s.push_str(&format!(" · {} marked", app.marked.len()));
        }
        if app.scanning {
            s.push_str(&format!(" · scanning {}", SPINNER[v.tick % SPINNER.len()]));
        }
        s
    };
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(header_text, t.ui(Ui::Header)))),
        header,
    );

    let height = body.height as usize;
    app.page = height.saturating_sub(2).max(1);
    if app.cursor < app.scroll {
        app.scroll = app.cursor;
    } else if app.cursor >= app.scroll + height {
        app.scroll = app.cursor + 1 - height;
    }
    app.scroll = app
        .scroll
        .min(app.rows.len().saturating_sub(height.min(app.rows.len())));

    if app.rows.is_empty() {
        let msg = if app.scanning {
            "scanning…"
        } else if app.view.attention_only && !app.ws.entries.is_empty() {
            "nothing needs attention 🎉  (a: show all)"
        } else {
            "no repositories (check roots in config; R rescans)"
        };
        f.render_widget(Paragraph::new(Span::styled(msg, t.ui(Ui::Dim))), body);
        return;
    }

    // Column widths from visible repo rows.
    let name_w = app
        .rows
        .iter()
        .filter_map(|r| if let Row::Repo(i) = r { Some(*i) } else { None })
        .map(|i| {
            UnicodeWidthStr::width(app.ws.entries[i].display_name().as_str())
                + 2 * app.ws.depth(i)
                + 2
        })
        .max()
        .unwrap_or(10)
        .min(body.width as usize / 2);
    let branch_w = app
        .rows
        .iter()
        .filter_map(|r| if let Row::Repo(i) = r { Some(*i) } else { None })
        .filter_map(|i| badges::branch(&app.ws.entries[i], &v.icons))
        .map(|b| UnicodeWidthStr::width(b.text.as_str()))
        .max()
        .unwrap_or(0)
        .min(30);

    let mut lines = vec![];
    for (row_idx, row) in app.rows.iter().enumerate().skip(app.scroll).take(height) {
        let selected = row_idx == app.cursor;
        let mut line = match row {
            Row::Repo(i) => repo_line(app, v, *i, name_w, branch_w),
            Row::Detail { repo, line } => {
                let indent = "  ".repeat(app.ws.depth(*repo)) + "      ";
                let mut spans = vec![Span::raw(indent)];
                spans.extend(line.parts.iter().map(|(role, s)| {
                    Span::styled(
                        s.clone(),
                        role.map(|r| t.role(r)).unwrap_or(t.ui(Ui::Detail)),
                    )
                }));
                Line::from(spans)
            }
        };
        if selected {
            line = line.patch_style(t.ui(Ui::Selection));
            // extend the selection bar to the full width
            let w = line.width();
            if w < body.width as usize {
                line.push_span(Span::styled(
                    " ".repeat(body.width as usize - w),
                    t.ui(Ui::Selection),
                ));
            }
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), body);
}

fn repo_line<'a>(app: &App, v: &View, i: usize, name_w: usize, branch_w: usize) -> Line<'a> {
    let t = v.theme;
    let e = &app.ws.entries[i];
    let mut spans: Vec<Span> = vec![];
    let marked = app.marked.contains(&e.path);
    spans.push(if app.running.contains_key(&e.path) {
        Span::styled(SPINNER[v.tick % SPINNER.len()], t.ui(Ui::Running))
    } else if marked {
        Span::styled("▌", t.ui(Ui::Marked))
    } else {
        Span::raw(" ")
    });
    // flags: fixed 2-cell column
    let flags = badges::flags(e, &v.icons);
    let mut fw = 0;
    for b in flags.iter().take(2) {
        fw += UnicodeWidthStr::width(b.text.as_str());
        spans.push(Span::styled(b.text.clone(), t.role(b.role)));
    }
    spans.push(Span::raw(" ".repeat(3usize.saturating_sub(fw))));

    let depth = app.ws.depth(i);
    let tree = match e.kind {
        RepoKind::Worktree => format!("{} ", v.icons.worktree.trim_end()),
        RepoKind::Submodule => format!("{} ", v.icons.submodule.trim_end()),
        _ => String::new(),
    };
    let name = format!("{}{tree}{}", "  ".repeat(depth), e.display_name());
    let name = truncate(&name, name_w);
    let mut name_style = t.ui(Ui::Name);
    if e.flags.hidden() {
        name_style = t.role(argus_core::badges::Role::Hidden);
    }
    if marked {
        name_style = name_style.patch(t.ui(Ui::Marked));
    }
    let pad = name_w.saturating_sub(UnicodeWidthStr::width(name.as_str()));
    spans.push(Span::styled(name, name_style));
    spans.push(Span::raw(" ".repeat(pad + 1)));

    if let Some(b) = badges::branch(e, &v.icons) {
        let text = truncate(&b.text, branch_w);
        let pad = branch_w.saturating_sub(UnicodeWidthStr::width(text.as_str()));
        spans.push(Span::styled(text, t.role(b.role)));
        spans.push(Span::raw(" ".repeat(pad + 1)));
    } else {
        spans.push(Span::raw(" ".repeat(branch_w + 1)));
    }
    for (n, b) in badges::state(e, &v.icons).into_iter().enumerate() {
        if n > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(b.text, t.role(b.role)));
    }
    if let Some(label) = app.running.get(&e.path) {
        spans.push(Span::styled(format!("  {label}…"), t.ui(Ui::Running)));
    }
    Line::from(spans)
}

pub fn truncate(s: &str, w: usize) -> String {
    if UnicodeWidthStr::width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut cur = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cur + cw + 1 > w {
            break;
        }
        out.push(c);
        cur += cw;
    }
    out.push('…');
    out
}

fn draw_log(f: &mut Frame, app: &App, v: &View, area: Rect) {
    let t = v.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(t.ui(Ui::Border))
        .title(Span::styled(" process log ", t.ui(Ui::Title)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = vec![];
    for e in &app.log {
        let mark = if e.ok {
            Span::styled("✓ ", t.ui(Ui::Ok))
        } else {
            Span::styled("✗ ", t.ui(Ui::Failed))
        };
        let time = e.at.strftime("%H:%M:%S").to_string();
        lines.push(Line::from(vec![
            Span::styled(format!("{time} "), t.ui(Ui::Dim)),
            mark,
            Span::styled(
                format!("{} ", e.repo),
                t.ui(Ui::Name).add_modifier(Modifier::BOLD),
            ),
            Span::raw(e.label.clone()),
        ]));
        if !e.ok || e.output.lines().count() <= 3 {
            for l in e.output.lines().take(12) {
                lines.push(Line::from(Span::styled(format!("    {l}"), t.ui(Ui::Dim))));
            }
        }
    }
    let skip = lines.len().saturating_sub(inner.height as usize);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        inner,
    );
}

fn draw_status(f: &mut Frame, app: &App, v: &View, area: Rect) {
    let t = v.theme;
    let line = match &app.mode {
        Mode::Input { prompt, editor } => {
            let label = match prompt {
                Prompt::Filter => "filter: ".to_string(),
                Prompt::CommitMessage(p) => format!("commit message ({} repo(s)): ", p.len()),
                Prompt::SnoozeDuration(k) => format!("snooze {} repo(s) for: ", k.len()),
            };
            let x = area.x + (UnicodeWidthStr::width(label.as_str()) + editor.cursor()) as u16;
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            Line::from(vec![
                Span::styled(label, t.ui(Ui::Key)),
                Span::raw(editor.text()),
            ])
        }
        _ => match &app.message {
            Some((m, true)) => Line::from(Span::styled(format!(" {m}"), t.ui(Ui::Failed))),
            Some((m, false)) => Line::from(Span::raw(format!(" {m}"))),
            None => {
                let mut spans = vec![];
                for (k, label) in [
                    ("?", "help"),
                    ("Tab", "details"),
                    ("F", "pull"),
                    ("f", "fetch"),
                    ("P", "push"),
                    ("x", "fix"),
                    ("m", "mark"),
                    ("o", "open"),
                    ("/", "filter"),
                    ("q", "quit"),
                ] {
                    spans.push(Span::styled(format!(" {k}"), t.ui(Ui::Key)));
                    spans.push(Span::styled(format!(" {label}"), t.ui(Ui::Dim)));
                }
                Line::from(spans)
            }
        },
    };
    f.render_widget(Paragraph::new(line).style(t.ui(Ui::StatusBar)), area);
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    // bottom of the list area, like transient
    Rect {
        x: area.x,
        y: area.bottom().saturating_sub(h),
        width: w,
        height: h,
    }
}

fn draw_menu(f: &mut Frame, app: &App, v: &View, area: Rect) {
    let Mode::Menu { menu, cursor } = &app.mode else {
        return;
    };
    let t = v.theme;
    let targets = app.selected().len();
    let title = format!(
        " {} · {} repo{} ",
        menu.title,
        targets,
        if targets == 1 { "" } else { "s" }
    );
    let mut lines = vec![];
    for (n, it) in menu.items.iter().enumerate() {
        let mut l = Line::from(vec![
            Span::styled(format!(" {} ", it.key), t.ui(Ui::Key)),
            Span::raw(format!(" {}", it.label)),
        ]);
        if n == *cursor {
            l = l.patch_style(t.ui(Ui::Selection));
        }
        lines.push(l);
    }
    lines.push(Line::from(Span::styled(
        " q/Esc/C-g cancel · ↑↓ Enter",
        t.ui(Ui::Dim),
    )));
    let w = menu
        .items
        .iter()
        .map(|i| i.label.len() as u16 + 8)
        .max()
        .unwrap_or(20)
        .max(title.len() as u16 + 4);
    let r = popup(area, w.max(36), lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(t.ui(Ui::Border))
                .title(Span::styled(title, t.ui(Ui::Title))),
        ),
        r,
    );
}

pub fn help_lines() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = [
        ("↑↓ C-p C-n j k", "move"),
        ("n p  M-n M-p", "next / previous repo"),
        ("PgUp PgDn C-v M-v C-u C-d", "page"),
        ("Home End M-< M-> < > gg G", "top / bottom"),
        ("Tab Enter", "unfold / fold details"),
        ("Space", "mark repo (actions apply to marked, else current)"),
        ("U Esc C-g", "unmark all (Esc also clears filter)"),
        ("/ C-s", "filter (readline keys)"),
        ("a", "toggle attention only"),
        ("H", "toggle hidden repos"),
        ("g C-l", "refresh statuses"),
        ("R", "rescan roots"),
        ("!", "shell in repo"),
        ("$ L", "process log"),
        ("q C-c", "quit"),
    ]
    .iter()
    .map(|(k, d)| (k.to_string(), d.to_string()))
    .collect();
    for id in MenuId::ALL {
        let m = id.menu();
        let items = m
            .items
            .iter()
            .map(|i| format!("{} {}", i.key, i.label))
            .collect::<Vec<_>>()
            .join(" · ");
        v.push((format!("{} …", id.key()), format!("{}: {items}", m.title)));
    }
    v
}

/// Every icon in `i` with its meaning and colour role, for the help legend.
pub fn legend(i: &Icons) -> Vec<(&str, Role, &'static str)> {
    vec![
        (&i.favourite, Role::Favourite, "favourite"),
        (
            &i.new,
            Role::New,
            "new (cloned or found, not yet acknowledged)",
        ),
        (&i.excluded, Role::Hidden, "excluded"),
        (&i.snoozed, Role::Hidden, "snoozed"),
        (
            &i.branch,
            Role::DefaultBranch,
            "default branch (others in the branch colour)",
        ),
        (&i.detached, Role::Detached, "detached HEAD (tag or commit)"),
        (&i.ahead, Role::Ahead, "commits to push"),
        (
            &i.behind,
            Role::Behind,
            "commits to pull (as of the last fetch)",
        ),
        (&i.staged, Role::Staged, "staged changes"),
        (&i.unstaged, Role::Unstaged, "unstaged changes"),
        (&i.untracked, Role::Untracked, "untracked files"),
        (&i.conflict, Role::Conflict, "conflicts"),
        (&i.in_progress, Role::Conflict, "merge/rebase/… in progress"),
        (&i.stash, Role::Stash, "stashes"),
        (&i.gone, Role::Warning, "upstream branch deleted"),
        (&i.no_upstream, Role::Warning, "branch has no upstream"),
        (
            &i.branches,
            Role::Warning,
            "other branches with unpushed work",
        ),
        (&i.error, Role::Error, "error reading the repo"),
        (&i.worktree, Role::Dim, "linked worktree"),
        (&i.submodule, Role::Dim, "submodule"),
    ]
}

fn draw_help(f: &mut Frame, v: &View, area: Rect, scroll: u16) {
    let t = v.theme;
    let mut lines: Vec<Line> = help_lines()
        .into_iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:>26}  "), t.ui(Ui::Key)),
                Span::raw(d),
            ])
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!("{:>26}  ", "legend"),
        t.ui(Ui::Title),
    )));
    for (icon, role, meaning) in legend(&v.icons) {
        let icon = if icon.is_empty() { "main" } else { icon };
        lines.push(Line::from(vec![
            Span::raw(format!("{:>w$}", "", w = 26 - icon.chars().count().min(26))),
            Span::styled(icon.to_string(), t.role(role)),
            Span::raw(format!("  {meaning}")),
        ]));
    }
    let r = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(t.ui(Ui::Border))
                    .title(Span::styled(" keys (any key closes) ", t.ui(Ui::Title))),
            ),
        r,
    );
}
