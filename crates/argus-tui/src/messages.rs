//! The messages window (`v m`): everything said and done this session,
//! each message holding what it did, folded; or every timed step, slowest
//! first; or the steps' time by kind. Like htop, sorting by time isn't a
//! tree.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use argus_core::trace::Step;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Event, Happened};
use crate::input::{Edit, LineEditor};

/// How the messages are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum By {
    /// As they happened, each holding what it did.
    #[default]
    Tree,
    /// The same tree, each level slowest first (what took no time, a
    /// message or a note, after, as it happened).
    Sorted,
    /// Every timed step, slowest first.
    Slowest,
    /// The steps' time by kind, most first.
    Kinds,
}

impl By {
    fn next(self) -> Self {
        match self {
            By::Tree => By::Sorted,
            By::Sorted => By::Slowest,
            By::Slowest => By::Kinds,
            By::Kinds => By::Tree,
        }
    }
}

/// The window's state: how it's shown, sorted by own time rather than all
/// a step took (by time, by kind), the line the cursor's on (past the
/// last: on the newest, following), the first line shown, the messages
/// folded the other way from how they'd be, the search, and, by kind (no
/// line of the history being shown), the line the cursor was on before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Messages {
    pub by: By,
    pub own: bool,
    pub cursor: usize,
    pub top: usize,
    pub flipped: HashSet<Vec<usize>>,
    pub query: String,
    pub searching: Option<LineEditor>,
    pub before_kinds: Option<Vec<usize>>,
}

impl Default for Messages {
    fn default() -> Self {
        Messages {
            by: By::Tree,
            own: false,
            cursor: usize::MAX,
            top: 0,
            flipped: HashSet::new(),
            query: String::new(),
            searching: None,
            before_kinds: None,
        }
    }
}

/// One line of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLine<'a> {
    pub depth: usize,
    /// Its place: the message's index in the history, then each step's in
    /// the one holding it.
    pub path: Vec<usize>,
    /// When, for a message (not what it holds).
    pub at: Option<jiff::Timestamp>,
    /// Whether it's unfolded, if it holds anything.
    pub fold: Option<bool>,
    pub what: Shown<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown<'a> {
    Said(&'a str),
    Failed(&'a str),
    Job {
        repo: &'a str,
        label: &'a str,
        ok: bool,
    },
    /// A step, and what it's in where that's not shown above it (sorted
    /// by time).
    Step {
        step: &'a Step,
        of: Option<&'a str>,
    },
    /// A line of what was said, or of a job's output, after its first.
    Output(&'a str),
    /// The steps of a kind, and their time.
    Kind {
        kind: &'a str,
        tally: Tally,
    },
}

/// The steps of a kind: how many, their time, the slowest's, and how it
/// divides (see [`argus_core::trace::Split`]): their own, and, of those
/// that took steps, their steps' and those steps' total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tally {
    pub count: usize,
    pub took: Duration,
    pub slowest: Duration,
    pub own: Duration,
    pub sub_steps: Option<(Duration, Duration)>,
}

/// A message, or something it holds.
#[derive(Clone, Copy)]
enum Node<'a> {
    Event(&'a Event),
    Step(&'a Step),
    Line(&'a str),
}

impl<'a> Node<'a> {
    fn held(self) -> Vec<Node<'a>> {
        match self {
            Node::Event(e) => match &e.what {
                Happened::Said(s) | Happened::Failed(s) => {
                    s.lines().skip(1).map(Node::Line).collect()
                }
                Happened::Job { output, .. } => output.lines().map(Node::Line).collect(),
                Happened::Step(s) => s.steps.iter().map(Node::Step).collect(),
            },
            Node::Step(s) => s.steps.iter().map(Node::Step).collect(),
            Node::Line(_) => vec![],
        }
    }

    fn shown(self) -> Shown<'a> {
        match self {
            Node::Event(e) => match &e.what {
                Happened::Said(s) => Shown::Said(s.lines().next().unwrap_or("")),
                Happened::Failed(s) => Shown::Failed(s.lines().next().unwrap_or("")),
                Happened::Job {
                    repo, label, ok, ..
                } => Shown::Job {
                    repo,
                    label,
                    ok: *ok,
                },
                Happened::Step(s) => Shown::Step { step: s, of: None },
            },
            // (in the tree, what it's in is above it)
            Node::Step(s) => Shown::Step { step: s, of: None },
            Node::Line(l) => Shown::Output(l),
        }
    }

    /// Whether its own text has `q` (lowercase) in it.
    fn has(self, q: &str) -> bool {
        let text = match self.shown() {
            Shown::Said(s) | Shown::Failed(s) | Shown::Output(s) => s,
            Shown::Job { repo, label, .. } => {
                return repo.to_lowercase().contains(q) || label.to_lowercase().contains(q);
            }
            Shown::Step { step, of } => return step.named_in(of).to_lowercase().contains(q),
            Shown::Kind { kind, .. } => kind,
        };
        text.to_lowercase().contains(q)
    }

    /// Whether it or anything it holds has `q` in it.
    fn found(self, q: &str) -> bool {
        self.has(q) || self.held().into_iter().any(|n| n.found(q))
    }
}

/// The lines `m` shows of `history`.
pub fn lines<'a>(history: &'a [Event], m: &Messages) -> Vec<MessageLine<'a>> {
    let q = m.query.to_lowercase();
    let mut out = vec![];
    match m.by {
        By::Tree | By::Sorted => {
            let events: Vec<(usize, Node)> = history.iter().map(Node::Event).enumerate().collect();
            for (i, node) in in_order(events, m) {
                if q.is_empty() || node.found(&q) {
                    tree(&mut out, node, vec![i], Some(history[i].at), m, &q, false);
                }
            }
        }
        By::Slowest => {
            let mut all: Vec<MessageLine> = vec![];
            for (i, e) in history.iter().enumerate() {
                if let Happened::Step(s) = &e.what {
                    timed(&mut all, s, None, vec![i], e.at);
                }
            }
            all.retain(|l| {
                q.is_empty()
                    || matches!(l.what, Shown::Step { step, of } if step.named_in(of).to_lowercase().contains(&q))
            });
            // (stable: of those as slow, the earlier first)
            all.sort_by_key(|l| match l.what {
                Shown::Step { step, .. } if m.own => std::cmp::Reverse(own_time(step)),
                Shown::Step { step, .. } => std::cmp::Reverse(step.took),
                _ => std::cmp::Reverse(None),
            });
            out = all;
        }
        By::Kinds => {
            let mut kinds: BTreeMap<&str, Tally> = BTreeMap::new();
            fn count<'a>(s: &'a Step, kinds: &mut BTreeMap<&'a str, Tally>) {
                if let Some(took) = s.took {
                    let k = kinds.entry(&s.kind).or_default();
                    k.count += 1;
                    k.took += took;
                    k.slowest = k.slowest.max(took);
                    match s.split() {
                        Some(split) => {
                            k.own += split.own;
                            let (sub, total) = k.sub_steps.get_or_insert_default();
                            *sub += split.sub_steps;
                            *total += split.total;
                        }
                        None => k.own += took,
                    }
                }
                for s in &s.steps {
                    count(s, kinds);
                }
            }
            for e in history {
                if let Happened::Step(s) = &e.what {
                    count(s, &mut kinds);
                }
            }
            let mut kinds: Vec<_> = kinds
                .into_iter()
                .filter(|(k, _)| q.is_empty() || k.to_lowercase().contains(&q))
                .collect();
            kinds.sort_by_key(|(_, t)| std::cmp::Reverse(if m.own { t.own } else { t.took }));
            out = kinds
                .into_iter()
                .enumerate()
                .map(|(i, (kind, tally))| MessageLine {
                    depth: 0,
                    path: vec![i],
                    at: None,
                    fold: None,
                    what: Shown::Kind { kind, tally },
                })
                .collect();
        }
    }
    out
}

/// `node`'s line and, unfolded, those of what it holds: folded unless
/// flipped, or, searching, unfolded where it holds what's searched for.
/// Searching, what it holds is shown where it leads to a match, or all of
/// it under one (`under`).
fn tree<'a>(
    out: &mut Vec<MessageLine<'a>>,
    node: Node<'a>,
    path: Vec<usize>,
    at: Option<jiff::Timestamp>,
    m: &Messages,
    q: &str,
    under: bool,
) {
    let under = under || q.is_empty() || node.has(q);
    let held = node.held();
    let found_inside = !q.is_empty() && held.iter().any(|n| n.found(q));
    let open = found_inside != m.flipped.contains(&path);
    out.push(MessageLine {
        depth: path.len() - 1,
        path: path.clone(),
        at,
        fold: (!held.is_empty()).then_some(open),
        what: node.shown(),
    });
    if open {
        for (i, n) in in_order(held.into_iter().enumerate().collect(), m) {
            if under || n.found(q) {
                let mut p = path.clone();
                p.push(i);
                tree(out, n, p, None, m, q, under);
            }
        }
    }
}

/// `nodes` (each by its place among them) as the tree shows them: as they
/// came, or sorted, slowest first (by own time, `m.own`); what took no
/// time after, as it came.
fn in_order<'a>(mut nodes: Vec<(usize, Node<'a>)>, m: &Messages) -> Vec<(usize, Node<'a>)> {
    if m.by == By::Sorted {
        let time = |n: &Node| -> Option<Duration> {
            let s: &Step = match *n {
                Node::Event(Event {
                    what: Happened::Step(s),
                    ..
                }) => s,
                Node::Step(s) => s,
                _ => return None,
            };
            if m.own { own_time(s) } else { s.took }
        };
        nodes.sort_by_key(|(_, n)| std::cmp::Reverse(time(n)));
    }
    nodes
}

/// `s` and every step in it that was timed, each a line; `of`, what `s`
/// is in.
fn timed<'a>(
    out: &mut Vec<MessageLine<'a>>,
    s: &'a Step,
    of: Option<&'a str>,
    path: Vec<usize>,
    at: jiff::Timestamp,
) {
    if s.took.is_some() {
        out.push(MessageLine {
            depth: 0,
            path: path.clone(),
            at: Some(at),
            fold: None,
            what: Shown::Step { step: s, of },
        });
    }
    for (i, inner) in s.steps.iter().enumerate() {
        let mut p = path.clone();
        p.push(i);
        timed(out, inner, Some(&s.what), p, at);
    }
}

/// What a key did: whether the window stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyed {
    Stay,
    Close,
}

/// `k` in the window showing `history`, `page` lines a page.
pub fn key(history: &[Event], m: &mut Messages, k: KeyEvent, page: usize) -> Keyed {
    if let Some(editor) = &mut m.searching {
        match editor.handle(k) {
            Edit::Submit => m.searching = None,
            Edit::Cancel => {
                m.searching = None;
                m.query.clear();
            }
            Edit::Changed => {
                let text = editor.text();
                let was = on(history, m);
                m.query = text;
                place(history, m, was, m.by);
            }
            Edit::Moved | Edit::Ignored => {}
        }
        return Keyed::Stay;
    }
    let lines = lines(history, m);
    let last = lines.len().saturating_sub(1);
    let at = m.cursor.min(last);
    // (onto the last: following the newest)
    let go = |to: usize| if to >= last { usize::MAX } else { to };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let line = lines.get(at);
    // (the view by a page and the cursor with it, as Emacs scrolls)
    let up_a_page = |m: &mut Messages| {
        m.top = m.top.saturating_sub(page);
        m.cursor = go(at.saturating_sub(page));
    };
    let down_a_page = |m: &mut Messages| {
        m.top = m.top.saturating_add(page);
        m.cursor = go(at.saturating_add(page));
    };
    let flip = |m: &mut Messages, path: &Vec<usize>| {
        if !m.flipped.remove(path) {
            m.flipped.insert(path.clone());
        }
    };
    match k.code {
        // a page, as C-v and M-v move the list
        KeyCode::Char('v') if ctrl => down_a_page(m),
        KeyCode::Char('v') if alt => up_a_page(m),
        KeyCode::Char('g') if ctrl => return Keyed::Close,
        KeyCode::Char('<') if alt => m.cursor = go(0),
        KeyCode::Char('>') if alt => m.cursor = usize::MAX,
        KeyCode::Down | KeyCode::Char('j' | 'n') => m.cursor = go(at.saturating_add(1)),
        KeyCode::Up | KeyCode::Char('k' | 'p') => m.cursor = go(at.saturating_sub(1)),
        KeyCode::PageDown => down_a_page(m),
        KeyCode::PageUp => up_a_page(m),
        KeyCode::Home => m.cursor = go(0),
        KeyCode::End => m.cursor = usize::MAX,
        KeyCode::Tab | KeyCode::Enter => {
            if let Some(l) = line.filter(|l| l.fold.is_some()) {
                flip(m, &l.path);
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(l) = line.filter(|l| l.fold == Some(false)) {
                flip(m, &l.path);
            }
        }
        // folds it, or goes to what holds it
        KeyCode::Left | KeyCode::Char('h') => match line {
            Some(l) if l.fold == Some(true) => flip(m, &l.path),
            Some(l) => {
                if let Some(up) = lines[..at].iter().rposition(|u| u.depth < l.depth) {
                    m.cursor = go(up);
                }
            }
            None => {}
        },
        KeyCode::Char('s') => {
            let (was, from) = (on(history, m), m.by);
            m.by = m.by.next();
            m.top = 0;
            place(history, m, was, from);
        }
        // sorted by own time, or by all a step took
        KeyCode::Char('o') if m.by != By::Tree => {
            let was = on(history, m);
            m.own = !m.own;
            m.top = 0;
            place(history, m, was, m.by);
        }
        KeyCode::Char('/') => m.searching = Some(LineEditor::new(&m.query)),
        KeyCode::Esc if !m.query.is_empty() => {
            let was = on(history, m);
            m.query.clear();
            place(history, m, was, m.by);
        }
        KeyCode::Esc | KeyCode::Char('q') => return Keyed::Close,
        _ => {}
    }
    Keyed::Stay
}

/// Where the cursor is in the lines `m` shows: the line's place.
fn on(history: &[Event], m: &Messages) -> Option<Vec<usize>> {
    let lines = lines(history, m);
    let at = m.cursor.min(lines.len().checked_sub(1)?);
    Some(lines[at].path.clone())
}

/// The cursor, after the view changed from `from`: on the line it was on
/// (`was`) where that's shown, else on the slowest (by time) or the
/// newest, following (as they happened). By kind, lines are kinds: the
/// line it was on before is kept for after.
fn place(history: &[Event], m: &mut Messages, was: Option<Vec<usize>>, from: By) {
    let was = match (from == By::Kinds, m.by == By::Kinds) {
        (false, true) => {
            m.before_kinds = was;
            None
        }
        (true, true) => None,
        (true, false) => m.before_kinds.take(),
        (false, false) => was,
    };
    let lines = lines(history, m);
    let kept = was.and_then(|p| lines.iter().position(|l| l.path == p));
    let newest = lines.len().saturating_sub(1);
    m.cursor = match (kept, m.by) {
        // (on the newest as they happened: following it)
        (Some(i), By::Tree) if i >= newest => usize::MAX,
        (Some(i), _) => i,
        (None, By::Tree) => usize::MAX,
        (None, _) => 0,
    };
}

/// After the history's oldest `n` messages are dropped, `lines` lines of
/// them shown: the cursor and the view on the same lines, and what was
/// flipped of the rest still flipped.
pub fn dropped(m: &mut Messages, n: usize, lines: usize) {
    if m.cursor != usize::MAX {
        m.cursor = m.cursor.saturating_sub(lines);
    }
    m.top = m.top.saturating_sub(lines);
    let moved = |mut p: Vec<usize>| {
        (p[0] >= n).then(|| {
            p[0] -= n;
            p
        })
    };
    m.flipped = std::mem::take(&mut m.flipped)
        .into_iter()
        .filter_map(moved)
        .collect();
    m.before_kinds = m.before_kinds.take().and_then(moved);
}

/// The time `s` took that none of its steps ran in: all of it, if it took
/// none.
fn own_time(s: &Step) -> Option<Duration> {
    s.split().map(|split| split.own).or(s.took)
}
