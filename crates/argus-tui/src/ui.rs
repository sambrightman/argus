//! Rendering. Reads [`App`], never mutates anything but scroll bookkeeping.

use std::collections::{BTreeSet, HashMap};

use argus_core::ViewFilter;
use argus_core::badges::{self, Role};
use argus_core::config::Refine;
use argus_core::diff::LineKind;
use argus_core::discover::RepoKind;
use argus_core::icons::Icons;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Mode, Prompt, Row};
use crate::menu::MenuId;
use crate::refine;
use crate::theme::{Theme, Ui};

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
        Mode::Menu { .. } => app.menu_shown = draw_menu(f, app, v, main),
        Mode::Help { scroll } => draw_help(f, v, main, *scroll),
        Mode::Pick { .. } => draw_pick(f, app, v, main),
        _ => {}
    }
}

/// A list picked from by typing (its query on the status line), as many of
/// the matches as fit, best first.
fn draw_pick(f: &mut Frame, app: &App, v: &View, area: Rect) {
    let Mode::Pick {
        title,
        labels,
        shown,
        cursor,
        ..
    } = &app.mode
    else {
        return;
    };
    let t = v.theme;
    let room = (area.height as usize).saturating_sub(3).max(1);
    let start = cursor.saturating_sub(room.saturating_sub(1));
    let mut lines: Vec<Line> = shown
        .iter()
        .enumerate()
        .skip(start)
        .take(room)
        .map(|(n, &i)| {
            let l = Line::from(Span::raw(format!(" {}", labels[i])));
            if n == *cursor {
                l.patch_style(t.ui(Ui::Selection))
            } else {
                l
            }
        })
        .collect();
    if shown.is_empty() {
        lines.push(Line::from(Span::styled(" no match", t.ui(Ui::Dim))));
    }
    let footer = format!(
        " {} of {} · type to narrow · ↑↓ C-n C-p · Enter · Esc/C-g cancel",
        shown.len(),
        labels.len()
    );
    let w = labels
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()))
        .chain([UnicodeWidthStr::width(footer.as_str()), title.len()])
        .max()
        .unwrap_or(20) as u16
        + 4;
    lines.push(Line::from(Span::styled(footer, t.ui(Ui::Dim))));
    let r = popup(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(t.ui(Ui::Border))
                .title(Span::styled(format!(" {title} "), t.ui(Ui::Title))),
        ),
        r,
    );
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
            let mut s = format!(" argus  {n} repos · {attn} need attention · a: attention only");
            let hidden = app
                .ws
                .entries
                .iter()
                .filter(|e| e.flags.hidden() && !e.nested)
                .count();
            if !app.view.show_hidden && hidden > 0 {
                s.push_str(&format!(" · H: show {hidden} hidden"));
            }
            s
        };
        if app.view.show_hidden {
            s.push_str(" · showing hidden · H: hide them");
        }
        if !app.filter.is_empty() {
            s.push_str(&format!(" · filter: {}", app.filter));
        }
        if !app.marked.is_empty() {
            s.push_str(&format!(" · {} marked", app.marked.len()));
        }
        if app.scanning {
            s.push_str(&format!(" · scanning {}", v.icons.spin(v.tick)));
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
    // keep the cursor's row in view, moving as little as possible
    app.scroll = app
        .scroll
        .min(app.cursor)
        .max((app.cursor + 1).saturating_sub(height));
    app.scroll = app
        .scroll
        .min(app.rows.len().saturating_sub(height.min(app.rows.len())));

    if app.rows.is_empty() {
        let msg = if app.scanning {
            "scanning…".to_string()
        } else if app.view.attention_only && !app.ws.entries.is_empty() {
            let party = match v.icons.celebrate.trim_end() {
                "" => String::new(),
                c => format!(" {c}"),
            };
            format!("nothing needs attention{party}  (a: show all)")
        } else {
            "no repositories (check roots in config; R rescans)".to_string()
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
    // as wide as the repo with the most flags, listed or not (hidden, or
    // not needing attention), so the column stays put as the view changes
    let flags_w = app
        .ws
        .entries
        .iter()
        .filter(|e| !e.nested)
        .map(|e| flags_width(e, &v.icons))
        .max()
        .unwrap_or(0);
    let branch_w = app
        .rows
        .iter()
        .filter_map(|r| if let Row::Repo(i) = r { Some(*i) } else { None })
        .filter_map(|i| badges::branch(&app.ws.entries[i], &v.icons))
        .map(|b| UnicodeWidthStr::width(b.text.as_str()))
        .max()
        .unwrap_or(0)
        .min(30);

    let refined = refined_rows(app, app.scroll..app.scroll + height);
    let region: std::collections::BTreeSet<_> = app.region_lines().into_iter().collect();
    let mut lines = vec![];
    for (row_idx, row) in app.rows.iter().enumerate().skip(app.scroll).take(height) {
        let selected = row_idx == app.cursor;
        let mut line = match row {
            Row::Repo(i) => repo_line(app, v, *i, flags_w, name_w, branch_w),
            Row::Detail { repo, line } => {
                // two in from the name: past the marker, flags and a space
                let indent = "  ".repeat(app.ws.depth(*repo)) + &" ".repeat(flags_w + 4);
                let path = &app.ws.entries[*repo].path;
                let marked = match (line.diff.as_deref(), &line.file) {
                    (None, Some(f)) => app.marked_files.get(path).is_some_and(|m| m.contains(f)),
                    (Some(d), Some(f)) => d.hunk.zip(d.at).is_some_and(|(h, at)| {
                        let key = (path.clone(), f.clone(), d.side, h, at);
                        app.marked_lines.contains(&key) || region.contains(&key)
                    }),
                    _ => false,
                };
                let mut spans = if marked {
                    vec![
                        Span::styled(v.icons.marked.to_string(), t.ui(Ui::Marked)),
                        Span::raw(indent[1..].to_string()),
                    ]
                } else {
                    vec![Span::raw(indent)]
                };
                let style =
                    |role: &Option<Role>| role.map(|r| t.role(r)).unwrap_or(t.ui(Ui::Detail));
                match (refined.get(&row_idx), line.parts.as_slice()) {
                    // a refined diff line: indent, +/-, words, then any note
                    (Some(segs), [indent, (role, text), rest @ ..]) => {
                        spans.push(Span::styled(indent.1.clone(), style(&indent.0)));
                        spans.push(Span::styled(
                            text.chars().take(1).collect::<String>(),
                            style(role),
                        ));
                        for (changed, word) in segs {
                            let st = if *changed {
                                style(role).add_modifier(Modifier::REVERSED)
                            } else {
                                style(role)
                            };
                            spans.push(Span::styled(word.clone(), st));
                        }
                        spans.extend(rest.iter().map(|(r, s)| Span::styled(s.clone(), style(r))));
                    }
                    _ => spans.extend(
                        line.parts
                            .iter()
                            .map(|(role, s)| Span::styled(s.clone(), style(role))),
                    ),
                }
                Line::from(spans)
            }
        };
        if selected {
            line = line.patch_style(t.ui(Ui::Selection));
            // extend the selection bar to the full width
            let pad = (body.width as usize).saturating_sub(line.width());
            line.push_span(Span::styled(" ".repeat(pad), t.ui(Ui::Selection)));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), body);
}

/// Changed-word segments for the diff lines to refine among `visible`
/// rows (`[ui] refine`: every hunk shown, the cursor's, or none), by row.
fn refined_rows(app: &App, visible: std::ops::Range<usize>) -> HashMap<usize, refine::Segments> {
    let hunk_of = |n: usize| match app.rows.get(n) {
        Some(Row::Detail { repo, line }) => {
            let d = line.diff.as_ref()?;
            Some((*repo, line.file.clone()?, d.side, d.hunk?))
        }
        _ => None,
    };
    let targets: BTreeSet<_> = match app.ws.config.ui.refine {
        Refine::Off => return HashMap::new(),
        Refine::Selected => hunk_of(app.cursor).into_iter().collect(),
        Refine::All => visible.filter_map(hunk_of).collect(),
    };
    let mut out = HashMap::new();
    for key in targets {
        let rows: Vec<(usize, LineKind, &str)> = (0..app.rows.len())
            .filter(|&n| hunk_of(n).as_ref() == Some(&key))
            .filter_map(|n| match &app.rows[n] {
                Row::Detail { line, .. } => {
                    let l = line.diff.as_ref()?.line.as_ref()?;
                    Some((n, l.kind, l.text.as_str()))
                }
                _ => None,
            })
            .collect();
        let lines: Vec<(LineKind, &str)> = rows.iter().map(|(_, k, t)| (*k, *t)).collect();
        for ((n, _, _), segs) in rows.iter().zip(refine::refine(&lines)) {
            out.insert(*n, segs);
        }
    }
    out
}

fn repo_line<'a>(
    app: &App,
    v: &View,
    i: usize,
    flags_w: usize,
    name_w: usize,
    branch_w: usize,
) -> Line<'a> {
    let t = v.theme;
    let e = &app.ws.entries[i];
    let mut spans: Vec<Span> = vec![];
    let marked = app.marked.contains(&e.path);
    spans.push(if app.running.contains_key(&e.path) {
        Span::styled(v.icons.spin(v.tick).to_string(), t.ui(Ui::Running))
    } else if marked {
        Span::styled(v.icons.marked.to_string(), t.ui(Ui::Marked))
    } else {
        Span::raw(" ")
    });
    // flags, then a space, in a column as wide as the most of them
    let flags = badges::flags(e, &v.icons);
    for b in &flags {
        spans.push(Span::styled(b.text.clone(), t.role(b.role)));
    }
    spans.push(Span::raw(
        " ".repeat(flags_w + 1 - flags_width(e, &v.icons)),
    ));

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
            Span::styled(format!("{} ", v.icons.ok.trim_end()), t.ui(Ui::Ok))
        } else {
            Span::styled(format!("{} ", v.icons.failed.trim_end()), t.ui(Ui::Failed))
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
                Prompt::AmendMessage(p, _, on_remote) => format!(
                    "amend {}{}: ",
                    app.ws.get(p).map(|e| e.display_name()).unwrap_or_default(),
                    if *on_remote {
                        " (on a remote: pushing it will need force)"
                    } else {
                        ""
                    }
                ),
                Prompt::CommitMessage(p, all) => format!(
                    "commit {} changes ({} repo(s)): ",
                    if *all { "all" } else { "staged" },
                    p.len()
                ),
                Prompt::StashMessage(p) => format!("stash message ({} repo(s)): ", p.len()),
                Prompt::BranchName(p) => format!("new branch at HEAD ({} repo(s)): ", p.len()),
                Prompt::SquashMessage(_, c, instant) => format!(
                    "{}squash into {} (message to add; empty for none): ",
                    if *instant { "instant " } else { "" },
                    &c[..c.len().min(7)]
                ),
                Prompt::SpinOffName(p, stay) => format!(
                    "spin {} the unpushed commits to a new branch ({} repo(s)): ",
                    if *stay { "out" } else { "off" },
                    p.len()
                ),
                Prompt::RemoteUrl(_) => "URL of the remote to add as origin: ".to_string(),
                Prompt::SnoozeDuration(k) => format!("snooze {} repo(s) for: ", k.len()),
                Prompt::SnoozeIssue(_, i) => format!("snooze {} for: ", i.name().replace('_', " ")),
            };
            let x = area.x + (UnicodeWidthStr::width(label.as_str()) + editor.cursor()) as u16;
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            Line::from(vec![
                Span::styled(label, t.ui(Ui::Key)),
                Span::raw(editor.text()),
            ])
        }
        Mode::Pick { editor, .. } => {
            let label = "checkout: ";
            let x = area.x + (UnicodeWidthStr::width(label) + editor.cursor()) as u16;
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            Line::from(vec![
                Span::styled(label, t.ui(Ui::Key)),
                Span::raw(editor.text()),
            ])
        }
        Mode::Confirm { question, .. } => Line::from(vec![
            Span::styled(format!(" {question}"), t.ui(Ui::Failed)),
            Span::styled(" (y/n)", t.ui(Ui::Key)),
        ]),
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

/// Draws the open menu; returns the places of the items shown.
fn draw_menu(f: &mut Frame, app: &App, v: &View, area: Rect) -> Option<std::ops::Range<usize>> {
    let Mode::Menu { menu, cursor } = &app.mode else {
        return None;
    };
    let t = v.theme;
    let targets = app.selected().len();
    let title = match menu.id {
        // names its repo
        crate::menu::MenuId::Ignore
        | crate::menu::MenuId::Gitignore
        | crate::menu::MenuId::PickCommit => {
            format!(" {} ", menu.title)
        }
        _ => format!(
            " {} · {} repo{} ",
            menu.title,
            targets,
            if targets == 1 { "" } else { "s" }
        ),
    };
    let mut lines: Vec<Line> = vec![];
    // as many items as fit, around the cursor, saying how many are hidden
    let fixed = 3; // borders and the footer
    let room = (area.height as usize).saturating_sub(fixed);
    let total = menu.items.len();
    let (start, shown) = if total <= room {
        (0, total)
    } else {
        let shown = room.saturating_sub(2).max(1);
        let start = cursor.saturating_sub(shown / 2).min(total - shown);
        (start, shown)
    };
    if start > 0 {
        lines.push(Line::from(Span::styled(
            format!(" ↑ {start} more"),
            t.ui(Ui::Dim),
        )));
    }
    for (n, it) in menu.items.iter().enumerate().skip(start).take(shown) {
        let label = match it.switch {
            Some(s) => format!(
                "[{}] {}",
                if app.switch_on(s) { "x" } else { " " },
                it.label
            ),
            None => it.label.to_string(),
        };
        let mut l = Line::from(vec![
            Span::styled(format!(" {} ", it.key), t.ui(Ui::Key)),
            Span::raw(format!(" {label}")),
        ]);
        if n == *cursor {
            l = l.patch_style(t.ui(Ui::Selection));
        }
        lines.push(l);
    }
    if start + shown < total {
        lines.push(Line::from(Span::styled(
            format!(" ↓ {} more", total - start - shown),
            t.ui(Ui::Dim),
        )));
    }
    let moves = if menu.id.is_list() {
        "↑↓ n p j k"
    } else {
        "↑↓"
    };
    lines.push(Line::from(Span::styled(
        format!(" q/Esc/C-g cancel · {moves} Enter"),
        t.ui(Ui::Dim),
    )));
    let w = menu
        .items
        .iter()
        .map(|i| i.label.len() as u16 + 12)
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
    Some(start..start + shown)
}

pub fn help_lines() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = [
        ("↑↓ C-p C-n j k", "move"),
        (
            "n p",
            "next / previous section (skips diff and message lines)",
        ),
        ("M-n M-p", "next / previous repo"),
        ("PgUp PgDn C-v M-v C-u C-d", "page"),
        ("Home End M-< M-> < > gg G", "top / bottom"),
        ("Tab Enter", "unfold / fold details"),
        (
            "Space",
            "mark repo, or file in the details (actions apply to marked, else current)",
        ),
        (
            "s u K",
            "stage / unstage / discard files (marked, else current; discard asks)",
        ),
        (
            "C-space",
            "in a diff: a region of lines, for s u K e (again: keep as marks)",
        ),
        (
            "e",
            "in a diff: edit the hunk, line or marked lines, then stage (or unstage)",
        ),
        (
            "i",
            "add untracked files (marked, else current) to a .gitignore…",
        ),
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

/// How wide a repo's flag badges are.
fn flags_width(e: &argus_core::Entry, i: &Icons) -> usize {
    badges::flags(e, i)
        .iter()
        .map(|b| UnicodeWidthStr::width(b.text.as_str()))
        .sum()
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
        (&i.ignored, Role::Hidden, "ignored"),
        (&i.pull, Role::Pull, "pulled by argus pull (and F a)"),
        (&i.snoozed, Role::Hidden, "snoozed"),
        (
            &i.branch,
            Role::DefaultBranch,
            "default branch (others in the branch colour)",
        ),
        (&i.detached, Role::Detached, "detached HEAD (tag or commit)"),
        (
            &i.detached_commits,
            Role::Conflict,
            "commits only on the detached HEAD",
        ),
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
        (&i.no_remote, Role::Warning, "no remote"),
        (
            &i.branches,
            Role::Warning,
            "other branches with unpushed work",
        ),
        (&i.error, Role::Error, "error reading the repo"),
        (&i.worktree, Role::Dim, "linked worktree"),
        (&i.submodule, Role::Dim, "submodule"),
        (i.spin(0), Role::Dim, "job running"),
        (&i.marked, Role::Dim, "marked for a batch action"),
        (&i.ok, Role::Ahead, "job or fix succeeded"),
        (&i.failed, Role::Error, "job or fix failed"),
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
                    .title(Span::styled(
                        " keys (j k n p ↑ ↓ scroll; any other key closes) ",
                        t.ui(Ui::Title),
                    )),
            ),
        r,
    );
}
