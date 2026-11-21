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
    /// Every timed step, slowest first.
    Slowest,
    /// The steps' time by kind, most first.
    Kinds,
}

impl By {
    fn next(self) -> Self {
        match self {
            By::Tree => By::Slowest,
            By::Slowest => By::Kinds,
            By::Kinds => By::Tree,
        }
    }
}

/// The window's state: how it's shown, the line the cursor's on (past the
/// last: on the newest, following), the first line shown, the messages
/// folded the other way from how they'd be, and the search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Messages {
    pub by: By,
    pub cursor: usize,
    pub top: usize,
    pub flipped: HashSet<Vec<usize>>,
    pub query: String,
    pub searching: Option<LineEditor>,
}

impl Default for Messages {
    fn default() -> Self {
        Messages {
            by: By::Tree,
            cursor: usize::MAX,
            top: 0,
            flipped: HashSet::new(),
            query: String::new(),
            searching: None,
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
    Step(&'a Step),
    /// A line of what was said, or of a job's output, after its first.
    Output(&'a str),
    /// The steps of a kind: how many, their time, the slowest's.
    Kind {
        kind: &'a str,
        count: usize,
        total: Duration,
        slowest: Duration,
    },
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
                Happened::Step(s) => Shown::Step(s),
            },
            Node::Step(s) => Shown::Step(s),
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
            Shown::Step(s) => &s.what,
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
        By::Tree => {
            for (i, e) in history.iter().enumerate() {
                let node = Node::Event(e);
                if q.is_empty() || node.found(&q) {
                    tree(&mut out, node, vec![i], Some(e.at), m, &q, false);
                }
            }
        }
        By::Slowest => {
            let mut all: Vec<MessageLine> = vec![];
            for (i, e) in history.iter().enumerate() {
                if let Happened::Step(s) = &e.what {
                    timed(&mut all, s, vec![i], e.at);
                }
            }
            all.retain(|l| {
                q.is_empty()
                    || matches!(l.what, Shown::Step(s) if s.what.to_lowercase().contains(&q))
            });
            // (stable: of those as slow, the earlier first)
            all.sort_by_key(|l| match l.what {
                Shown::Step(s) => std::cmp::Reverse(s.took),
                _ => std::cmp::Reverse(None),
            });
            out = all;
        }
        By::Kinds => {
            let mut kinds: BTreeMap<&str, (usize, Duration, Duration)> = BTreeMap::new();
            fn count<'a>(s: &'a Step, kinds: &mut BTreeMap<&'a str, (usize, Duration, Duration)>) {
                if let Some(took) = s.took {
                    let k = kinds.entry(&s.kind).or_default();
                    k.0 += 1;
                    k.1 += took;
                    k.2 = k.2.max(took);
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
            kinds.sort_by_key(|(_, (_, total, _))| std::cmp::Reverse(*total));
            out = kinds
                .into_iter()
                .enumerate()
                .map(|(i, (kind, (count, total, slowest)))| MessageLine {
                    depth: 0,
                    path: vec![i],
                    at: None,
                    fold: None,
                    what: Shown::Kind {
                        kind,
                        count,
                        total,
                        slowest,
                    },
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
        for (i, n) in held.into_iter().enumerate() {
            if under || n.found(q) {
                let mut p = path.clone();
                p.push(i);
                tree(out, n, p, None, m, q, under);
            }
        }
    }
}

/// `s` and every step in it that was timed, each a line.
fn timed<'a>(out: &mut Vec<MessageLine<'a>>, s: &'a Step, path: Vec<usize>, at: jiff::Timestamp) {
    if s.took.is_some() {
        out.push(MessageLine {
            depth: 0,
            path: path.clone(),
            at: Some(at),
            fold: None,
            what: Shown::Step(s),
        });
    }
    for (i, inner) in s.steps.iter().enumerate() {
        let mut p = path.clone();
        p.push(i);
        timed(out, inner, p, at);
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
                m.query = editor.text();
                m.cursor = usize::MAX;
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
            Some(l) if l.depth > 0 => {
                if let Some(up) = lines[..at].iter().rposition(|u| u.depth < l.depth) {
                    m.cursor = go(up);
                }
            }
            _ => {}
        },
        KeyCode::Char('s') => {
            m.by = m.by.next();
            m.cursor = usize::MAX;
            m.top = 0;
        }
        KeyCode::Char('/') => m.searching = Some(LineEditor::new(&m.query)),
        KeyCode::Esc if !m.query.is_empty() => {
            m.query.clear();
            m.cursor = usize::MAX;
        }
        KeyCode::Esc | KeyCode::Char('q') => return Keyed::Close,
        _ => {}
    }
    Keyed::Stay
}

/// After the history's oldest `n` messages are dropped, `lines` lines of
/// them shown: the cursor and the view on the same lines, and what was
/// flipped of the rest still flipped.
pub fn dropped(m: &mut Messages, n: usize, lines: usize) {
    if m.cursor != usize::MAX {
        m.cursor = m.cursor.saturating_sub(lines);
    }
    m.top = m.top.saturating_sub(lines);
    m.flipped = std::mem::take(&mut m.flipped)
        .into_iter()
        .filter(|p| p[0] >= n)
        .map(|mut p| {
            p[0] -= n;
            p
        })
        .collect();
}

/// What `s`'s steps took summed, when more than it took (they ran side by
/// side).
pub fn summed(s: &Step) -> Option<Duration> {
    let sum: Duration = s.steps.iter().filter_map(|s| s.took).sum();
    s.took.filter(|&t| sum > t).map(|_| sum)
}
