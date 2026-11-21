//! Rendering. Reads [`App`], never mutates anything but scroll bookkeeping.

use std::collections::{BTreeSet, HashMap};

use argus_core::badges::{self, Role};
use argus_core::config::Refine;
use argus_core::diff::LineKind;
use argus_core::discover::RepoKind;
use argus_core::icons::Icons;
use argus_core::{ViewFilter, plural};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Marked, Mode, Prompt, Row};
use crate::details::Expand;
use crate::menu::MenuId;
use crate::refine;
use crate::theme::{Theme, Ui};

pub struct View<'a> {
    pub theme: &'a Theme,
    pub icons: Icons,
    pub tick: usize,
}

/// Text on screen that's a link: where, what it says and in what style,
/// and where it goes. The terminal draws these over the frame
/// ([`write_links`]), which itself knows nothing of links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkAt {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub url: String,
    pub style: ratatui::style::Style,
}

/// Write `links` over the frame just drawn: the same text in the same
/// place and style, as terminal hyperlinks (OSC 8). The frame's cells
/// don't change, so what's drawn next still diffs against the truth.
pub fn write_links(w: &mut impl std::io::Write, links: &[LinkAt]) -> std::io::Result<()> {
    use crossterm::{cursor::MoveTo, queue, style::Print};
    use ratatui::backend::IntoCrossterm;
    for l in links {
        // only a plain web address, around text as the frame shows it:
        // neither can end the sequence early
        let Some(url) = argus_core::text::web_url(&l.url) else {
            continue;
        };
        if l.text.contains(|c: char| c.is_control()) {
            continue;
        }
        let style: crossterm::style::ContentStyle = l.style.into_crossterm();
        let text = format!("\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\", l.text);
        queue!(w, MoveTo(l.x, l.y), Print(style.apply(text)))?;
    }
    if !links.is_empty() {
        queue!(w, crossterm::style::ResetColor)?;
        queue!(
            w,
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reset)
        )?;
    }
    w.flush()
}

/// Draws the frame; returns the links in it.
pub fn draw(f: &mut Frame, app: &mut App, v: &View) -> Vec<LinkAt> {
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
    let links = draw_list(f, app, v, main);
    if app.show_log {
        draw_log(f, app, v, log);
    }
    draw_status(f, app, v, status);
    // (nothing is linked under something drawn over the list)
    match &app.mode {
        Mode::Menu { .. } => app.menu_shown = draw_menu(f, app, v, main),
        Mode::Help { scroll } => draw_help(f, v, &app.ws.config.tag_icons(), main, *scroll),
        Mode::History { .. } => draw_history(f, app, v, main),
        Mode::Pick { .. } => draw_pick(f, app, v, main),
        _ => return links,
    }
    vec![]
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

fn draw_list(f: &mut Frame, app: &mut App, v: &View, area: Rect) -> Vec<LinkAt> {
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
            format!(
                " argus  {attn} of {total} {} {} attention · a: show all",
                plural(total, "repo", "repos"),
                plural(attn, "needs", "need"),
            )
        } else {
            let mut s = format!(
                " argus  {n} {} · {attn} {} attention · a: attention only",
                plural(n, "repo", "repos"),
                plural(attn, "needs", "need"),
            );
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
            let n = app.marked.len();
            s.push_str(&format!(" · {n} {} marked", plural(n, "repo", "repos")));
        }
        // (which a folded repo's own row can only hint at)
        let (files, lines) = app.marked_in(None);
        let counted = [(files, "file", "files"), (lines, "line", "lines")];
        let counted = counted.iter().filter(|(n, ..)| *n > 0);
        let counted: Vec<String> = counted
            .map(|(n, one, many)| format!("{n} {}", plural(*n, one, many)))
            .collect();
        if !counted.is_empty() {
            s.push_str(&format!(" · marked in details: {}", counted.join(", ")));
            // (the keys take them all, wherever the cursor is)
            let repos = app.repos_with_marks().len();
            if repos > 1 {
                s.push_str(&format!(" in {repos} repos"));
            }
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
        return vec![];
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
    let tags = app.ws.config.tag_icons();
    let flags_w = app
        .ws
        .entries
        .iter()
        .filter(|e| !e.nested)
        .map(|e| flags_width(e, &v.icons, &tags))
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
    let mut lines = vec![];
    let mut links = vec![];
    for (row_idx, row) in app.rows.iter().enumerate().skip(app.scroll).take(height) {
        let selected = row_idx == app.cursor;
        let mut line = match row {
            Row::Repo(i) => repo_line(app, v, *i, flags_w, name_w, branch_w),
            Row::Detail { repo, line } => {
                // two in from the name: past the marker, flags and a space
                let indent = "  ".repeat(app.ws.depth(*repo)) + &" ".repeat(flags_w + 4);
                // what the row is of the changes, and how much of that is
                // marked
                let marked = match app.target(row_idx) {
                    Some(target) => app.marked(&target),
                    // (the uncommitted issue unfolds into the changes: all
                    // of them, as the repo's own row has it)
                    None if line.expand == Some(Expand::Changes) => {
                        app.repo_marked(&app.ws.entries[*repo].path)
                    }
                    None => Marked::No,
                };
                let icon = match marked {
                    Marked::Wholly => Some(&v.icons.marked),
                    Marked::Partly => Some(&v.icons.partly_marked),
                    Marked::No => None,
                };
                let mut spans = match icon {
                    Some(icon) => vec![
                        Span::styled(icon.to_string(), t.ui(Ui::Marked)),
                        Span::raw(indent[1..].to_string()),
                    ],
                    None => vec![Span::raw(indent)],
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
                    _ => {
                        for (n, (role, s)) in line.parts.iter().enumerate() {
                            // a link, where all of it fits
                            let x: usize = spans.iter().map(Span::width).sum();
                            let fits = x + UnicodeWidthStr::width(s.as_str()) <= body.width.into();
                            let url = line.links.iter().find(|l| l.0 == n).filter(|_| fits);
                            links.extend(url.map(|(_, url)| LinkAt {
                                x: body.x + x as u16,
                                y: body.y + (row_idx - app.scroll) as u16,
                                text: s.clone(),
                                url: url.clone(),
                                style: match selected {
                                    true => style(role).patch(t.ui(Ui::Selection)),
                                    false => style(role),
                                },
                            }));
                            spans.push(Span::styled(s.clone(), style(role)));
                        }
                    }
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
    links
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
    // first what's marked in its details (seen here even when they're
    // folded): all its changes, or some
    spans.push(if app.running.contains_key(&e.path) {
        Span::styled(v.icons.spin(v.tick).to_string(), t.ui(Ui::Running))
    } else {
        match app.repo_marked(&e.path) {
            Marked::Wholly => Span::styled(v.icons.marked.to_string(), t.ui(Ui::Marked)),
            Marked::Partly => Span::styled(v.icons.partly_marked.to_string(), t.ui(Ui::Marked)),
            Marked::No => Span::raw(" "),
        }
    });
    // flags, in a column as wide as the most of them; then the repo's own
    // mark, before its name
    let tags = app.ws.config.tag_icons();
    let flags = badges::flags(e, &v.icons, &tags);
    for b in &flags {
        spans.push(Span::styled(b.text.clone(), t.role(b.role)));
    }
    spans.push(Span::raw(
        " ".repeat(flags_w - flags_width(e, &v.icons, &tags)),
    ));
    spans.push(match marked {
        true => Span::styled(v.icons.marked_repo.to_string(), t.ui(Ui::MarkedRepo)),
        false => Span::raw(" "),
    });

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
        name_style = name_style.patch(t.ui(Ui::MarkedRepo));
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
                Prompt::StashMessage(_) => "stash message (empty for the usual): ".to_string(),
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
                    ("t", "tag"),
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
        | crate::menu::MenuId::Across
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
    let notes = menu.notes.iter().map(|n| n.len() as u16 + 4);
    let w = menu
        .items
        .iter()
        .map(|i| i.label.len() as u16 + 12)
        .chain(notes)
        .max()
        .unwrap_or(20)
        .max(title.len() as u16 + 4)
        .max(36);
    let parts = menu
        .items
        .windows(2)
        .filter(|p| p[0].section != p[1].section)
        .count();
    // as many items as fit, around the cursor, saying how many are hidden
    let fixed = 3 + menu.notes.len() + parts; // borders, footer, notes, dividers
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
        if n > start && menu.items[n - 1].section != it.section {
            let line = v.icons.divider.repeat(w.saturating_sub(4) as usize);
            lines.push(Line::from(Span::styled(format!(" {line}"), t.ui(Ui::Dim))));
        }
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
    for note in &menu.notes {
        lines.push(Line::from(Span::styled(format!(" {note}"), t.ui(Ui::Dim))));
    }
    let moves = if menu.id.is_list() {
        "↑↓ n p j k"
    } else {
        "↑↓ C-n C-p"
    };
    lines.push(Line::from(Span::styled(
        format!(" q/Esc/C-g cancel · {moves} Enter"),
        t.ui(Ui::Dim),
    )));
    let r = popup(area, w, lines.len() as u16 + 2);
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
            "mark a repo; in the details a file, a hunk or a line (actions take the marked, else current)",
        ),
        ("M U", "mark / unmark all the repos listed"),
        (
            "C-g Esc",
            "end a region and unmark what's marked in details (Esc: a filter first)",
        ),
        (
            "s u K",
            "stage / unstage / discard: what's marked, else the file, hunk, section or repo the cursor is on (discard asks)",
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
        ("/ C-s", "filter (readline keys)"),
        ("a", "toggle attention only"),
        ("H", "toggle hidden repos"),
        ("g C-l", "refresh statuses"),
        ("R", "rescan roots"),
        ("!", "shell in repo"),
        ("? h", "help"),
        ("q C-c", "quit"),
    ]
    .iter()
    .map(|(k, d)| (k.to_string(), d.to_string()))
    .collect();
    for id in MenuId::ALL {
        let m = id.menu();
        let mut items: Vec<String> = m
            .items
            .iter()
            .map(|i| format!("{} {}", i.key, i.label))
            .collect();
        // (its parts divided by a dash)
        let mut parts: Vec<usize> = (1..m.items.len())
            .filter(|&n| m.items[n - 1].section != m.items[n].section)
            .collect();
        match id {
            // built from what applies: every item it can have
            MenuId::Fix => {
                items = [
                    "x fix once",
                    "a fix all once",
                    "X fix auto",
                    "A fix all auto",
                ]
                .map(String::from)
                .to_vec();
                items.extend(
                    argus_core::fix::FixKind::ALL
                        .iter()
                        .map(|&k| format!("{} {}", crate::menu::fix_key(k), k.name())),
                );
            }
            // (the tags are config's: favourite's `f`, and yours)
            MenuId::Tag => {
                items.insert(0, "each tag by its key".into());
                parts.iter_mut().for_each(|p| *p += 1);
            }
            _ => {}
        }
        let mut text = String::new();
        for (n, item) in items.iter().enumerate() {
            if n > 0 {
                text.push_str(if parts.contains(&n) { " — " } else { " · " });
            }
            text.push_str(item);
        }
        let items = text;
        v.push((format!("{} …", id.key()), format!("{}: {items}", m.title)));
    }
    v
}

/// How wide a repo's flag badges are.
fn flags_width(e: &argus_core::Entry, i: &Icons, tags: &[(String, String)]) -> usize {
    badges::flags(e, i, tags)
        .iter()
        .map(|b| UnicodeWidthStr::width(b.text.as_str()))
        .sum()
}

/// Every icon in `i`, and each tag's (`tags`: name and icon, in config
/// order), with its meaning and colour role, for the help legend.
pub fn legend<'a>(i: &'a Icons, tags: &'a [(String, String)]) -> Vec<(&'a str, Role, String)> {
    let mut v: Vec<(&str, Role, String)> = tags
        .iter()
        .enumerate()
        .map(|(n, (name, icon))| (icon.as_str(), Role::Tag(n), format!("tag: {name}")))
        .collect();
    let pull = tags.iter().position(|(n, _)| n == argus_core::config::PULL);
    let pull = Role::Tag(pull.unwrap_or(usize::MAX));
    let rest: Vec<(&str, Role, &str)> = vec![
        (
            &i.new,
            Role::New,
            "new (cloned or found, not yet acknowledged)",
        ),
        (&i.ignored, Role::Hidden, "ignored"),
        (&i.pull, pull, "pulled by argus pull (and F a)"),
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
        (&i.off_default, Role::Branch, "not on the default branch"),
        (
            &i.finished_worktree,
            Role::Branch,
            "finished worktree: merged, upstream gone",
        ),
        (
            &i.stale_worktrees,
            Role::Warning,
            "worktree records whose folder is gone",
        ),
        (&i.error, Role::Error, "error reading the repo"),
        (&i.worktree, Role::Dim, "linked worktree"),
        (&i.submodule, Role::Dim, "submodule"),
        (i.spin(0), Role::Dim, "job running"),
        (&i.marked, Role::Dim, "marked, in a repo's details"),
        (
            &i.partly_marked,
            Role::Dim,
            "partly marked: some lines of a file or hunk, some of a repo's changes",
        ),
        (
            &i.marked_repo,
            Role::Dim,
            "a repo marked for a batch action",
        ),
        (
            &i.divider,
            Role::Dim,
            "repeated: the line between a menu's parts",
        ),
        (&i.ok, Role::Ahead, "job or fix succeeded"),
        (&i.failed, Role::Error, "job or fix failed"),
    ];
    v.extend(
        rest.into_iter()
            .map(|(icon, r, m)| (icon, r, m.to_string())),
    );
    v
}

/// The message history over the list, newest at the bottom, scrolled
/// back as far as `Mode::History` says (at most to the oldest).
fn draw_history(f: &mut Frame, app: &mut App, v: &View, area: Rect) {
    use crate::app::Happened;
    let t = v.theme;
    let failed = || Span::styled(format!("{} ", v.icons.failed.trim_end()), t.ui(Ui::Failed));
    let mut lines: Vec<Line> = vec![];
    for e in &app.history {
        let time = Span::styled(format!("{} ", e.at.strftime("%H:%M:%S")), t.ui(Ui::Dim));
        let (first, rest): (Vec<Span>, Vec<&str>) = match &e.what {
            Happened::Said(s) => {
                let mut l = s.lines();
                (
                    vec![Span::raw(l.next().unwrap_or("").to_string())],
                    l.collect(),
                )
            }
            Happened::Failed(s) => {
                let mut l = s.lines();
                let first = Span::styled(l.next().unwrap_or("").to_string(), t.ui(Ui::Failed));
                (vec![failed(), first], l.collect())
            }
            Happened::Job {
                repo,
                label,
                ok,
                output,
            } => {
                let mark = if *ok {
                    Span::styled(format!("{} ", v.icons.ok.trim_end()), t.ui(Ui::Ok))
                } else {
                    failed()
                };
                let name = Span::styled(
                    format!("{repo} "),
                    t.ui(Ui::Name).add_modifier(Modifier::BOLD),
                );
                (
                    vec![mark, name, Span::raw(label.clone())],
                    output.lines().collect(),
                )
            }
            Happened::Step(step) => (vec![Span::styled(step.to_string(), t.ui(Ui::Dim))], vec![]),
        };
        lines.push(Line::from([vec![time], first].concat()));
        for l in rest {
            lines.push(Line::from(Span::styled(
                format!("         {l}"),
                t.ui(Ui::Dim),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(" nothing yet", t.ui(Ui::Dim))));
    }
    let r = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    let height = r.height.saturating_sub(2) as usize;
    let most = lines.len().saturating_sub(height);
    let back = match &mut app.mode {
        Mode::History { back } => {
            *back = (*back).min(most);
            *back
        }
        _ => 0,
    };
    let start = most - back;
    let shown: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(shown).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(t.ui(Ui::Border))
                .title(Span::styled(
                    " messages (j k n p ↑ ↓ C-v M-v PgUp PgDn scroll; any other key closes) ",
                    t.ui(Ui::Title),
                )),
        ),
        r,
    );
}

fn draw_help(f: &mut Frame, v: &View, tags: &[(String, String)], area: Rect, scroll: u16) {
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
    for (icon, role, meaning) in legend(&v.icons, tags) {
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
