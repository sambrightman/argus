//! Unfolded detail lines for a repo (magit-style sections, flattened).

use argus_core::Entry;
use argus_core::attention::Issue;
use argus_core::badges::Role;
use argus_core::config::StashConfig;
use argus_core::diff::Side;
use argus_core::fix::{self, FixKind};
use argus_core::history::Range;
use argus_core::icons::Icons;
use argus_core::ops::Op;
use argus_core::paths::contract_tilde;
use argus_core::status::PathKind;

/// A detail line: styled parts. `None` uses the plain detail style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLine {
    pub parts: Vec<(Option<Role>, String)>,
    /// The changed file this line shows (repo-relative), for marking.
    pub file: Option<String>,
    /// Part of that file's diff (see [`DiffRef`]).
    pub diff: Option<Box<DiffRef>>,
    /// The issue this line shows, for ignoring it.
    pub issue: Option<Issue>,
    /// That issue is ignored: toggling stops ignoring it. (A repo can show
    /// stashes both ways, with the same count.)
    pub ignored: bool,
    /// The stash this line shows (`stash@{n}`), for dropping it.
    pub stash: Option<usize>,
    /// The local branch this line shows, for deleting it.
    pub branch: Option<String>,
    /// What Tab unfolds it into, read when unfolded.
    pub expand: Option<Expand>,
    /// The fix this line offers, for the Fix menu to start on.
    pub fix: Option<FixKind>,
    /// Body text (a commit message line, a diff line), which `n`/`p` skip
    /// as magit's do, stopping only at section lines.
    pub body: bool,
    /// Parts that are links (by their place in `parts`), and where to.
    pub links: Vec<(usize, String)>,
    /// The section of changes this line heads (no `file`) or is in.
    pub section: Option<Section>,
    /// Where the line is in its repo's details: the lines it's under,
    /// outermost first, then itself (see [`Seg`]). What's folded and
    /// where the cursor is are remembered by it, so the same commit shown
    /// in two places folds in each by itself.
    pub id: Vec<Seg>,
}

/// One step of a line's place among its repo's details (see
/// [`DetailLine::id`]): `[Section(Unstaged), File("a.rs"), Hunk(..),
/// Line(3)]` is a line of a hunk of a file's unstaged changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Seg {
    /// A labelled line: `path`, `remote`…; a commit's `commit`, `author`.
    Info(&'static str),
    /// A line of text, by its place: an error's, a commit message's.
    Text(usize),
    /// An issue's line, by its name and how it's shown: counted (none),
    /// `ignored` or `snoozed`.
    Issue(&'static str, &'static str),
    /// A fix's line, by its label (`auto-fix`, `then`, `fix`; `…` for one
    /// that asks first) and its kind's name.
    Fix(&'static str, &'static str),
    Branches,
    Branch(String),
    Section(Section),
    /// A changed file; one a commit changed; one in an untracked folder.
    File(String),
    /// A repo nested in the work tree, by its path there.
    Repo(String),
    /// A hunk, by what it adds and removes (its line numbers change as
    /// others are staged), and which of the file's hunks doing the same.
    Hunk(u64, usize),
    /// A hunk's line, by its place in it.
    Line(usize),
    /// Something said in place of content: "binary file", "no commits".
    Note,
    /// "… more".
    More,
    /// The blank line after a diff.
    Blank,
    Stashes,
    /// A stash, by its commit (its number if that's unknown).
    Stash(String),
    Commit(String),
    /// The heading of a commit's files.
    Files,
}

impl Seg {
    /// Whether what's under a line like this is shown until it's folded
    /// (the rest unfold when asked).
    pub fn open_by_default(&self) -> bool {
        matches!(
            self,
            Seg::Branches | Seg::Stashes | Seg::Section(_) | Seg::Files | Seg::Hunk(..)
        )
    }

    /// The hunks' steps, in order.
    pub fn hunks(hunks: &[argus_core::diff::Hunk]) -> Vec<Seg> {
        use std::hash::{Hash, Hasher};
        let mut seen: Vec<u64> = vec![];
        for h in hunks {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            let changed =
                |l: &&argus_core::diff::Line| l.kind != argus_core::diff::LineKind::Context;
            for l in h.lines.iter().filter(changed) {
                (l.kind == argus_core::diff::LineKind::Add, &l.text).hash(&mut hasher);
            }
            seen.push(hasher.finish());
        }
        let nth = |i: usize| seen[..i].iter().filter(|k| **k == seen[i]).count();
        (0..seen.len())
            .map(|i| Seg::Hunk(seen[i], nth(i)))
            .collect()
    }
}

/// The sections of a repo's changes, in the order shown (magit's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Untracked,
    Unstaged,
    Staged,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Untracked, Section::Unstaged, Section::Staged];

    pub fn title(self) -> &'static str {
        match self {
            Section::Untracked => "untracked",
            Section::Unstaged => "unstaged",
            Section::Staged => "staged",
        }
    }

    /// The side its files' diffs are of (an untracked file's is all
    /// added, against nothing).
    pub fn side(self) -> Side {
        match self {
            Section::Staged => Side::Staged,
            _ => Side::Unstaged,
        }
    }

    /// The file's letter in this section and its colour, if it's in it:
    /// the work tree's change under unstaged, the index's under staged; a
    /// conflicted file is unstaged.
    pub fn code(self, f: &argus_core::status::FileChange) -> Option<(Role, char)> {
        match (self, f.index, f.worktree) {
            (Section::Untracked, ..) if f.untracked() => Some((Role::Untracked, '?')),
            (_, '?', _) => None,
            // (staged for deletion and still there: untracked, not changed)
            (Section::Unstaged, ..) if f.untracked() => None,
            (Section::Unstaged, 'U', _) => Some((Role::Conflict, 'U')),
            (_, 'U', _) => None,
            (Section::Unstaged, _, w) if w != ' ' => Some((Role::Unstaged, w)),
            (Section::Staged, i, _) if i != ' ' => Some((Role::Staged, i)),
            _ => None,
        }
    }

    /// The paths of `s` in this section, in order.
    pub fn files(self, s: &argus_core::status::RepoStatus) -> Vec<String> {
        let in_it = |f: &&argus_core::status::FileChange| self.code(f).is_some();
        let files = s.changes.files.iter().filter(in_it);
        files.map(|f| f.path.clone()).collect()
    }

    /// Whether this is the section's own line, which folds it.
    pub fn heads(line: &DetailLine) -> Option<Section> {
        match line.id.last() {
            Some(Seg::Section(s)) => Some(*s),
            _ => None,
        }
    }
}

impl DetailLine {
    /// Body text: see [`Self::body`]; a working tree diff's lines too.
    pub fn is_body(&self) -> bool {
        self.body || self.diff.as_ref().is_some_and(|d| d.line.is_some())
    }

    /// Whether lines can be under this one: Tab on it, or on any of them,
    /// folds and unfolds them. A group's heading, a line that unfolds into
    /// something read when asked for, a changed file (its diff).
    pub fn folds(&self) -> bool {
        self.id.last().is_some_and(Seg::open_by_default)
            || self.expand.is_some()
            || (self.file.is_some() && self.section.is_some() && self.diff.is_none())
    }

    /// The line, unfolding into `expand`.
    pub fn unfolding(mut self, expand: Option<Expand>) -> Self {
        self.expand = expand;
        self
    }

    /// The line at `id`, under whatever this one is under.
    pub fn at(mut self, seg: Seg) -> Self {
        self.id.push(seg);
        self
    }

    /// The line under the line at `parent`.
    pub fn under(mut self, parent: &[Seg]) -> Self {
        self.id.splice(0..0, parent.iter().cloned());
        self
    }
}

/// What a details line unfolds into.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Expand {
    /// A list of commits.
    Commits(Range),
    /// The other local branches with commits on no remote, each unfolding
    /// into them.
    UnpushedBranches,
    /// One commit (or stash): its message and changed files.
    Commit(String),
    /// A file a commit changed (`old -> new` for a rename): its diff
    /// against the commit's first parent.
    CommitFile(String, String),
    /// The files of an untracked directory shown collapsed.
    Dir(String),
    /// A repo nested in the work tree: its own details, acting on it.
    Repo(String),
}

/// What an untracked path's line unfolds into, if anything.
pub fn untracked_expand(path: &str, kind: PathKind) -> Option<Expand> {
    if path.ends_with('/') {
        Some(Expand::Dir(path.to_string()))
    } else if kind == PathKind::Repo {
        Some(Expand::Repo(path.to_string()))
    } else {
        None
    }
}

/// An untracked path's step: a repo's says so, to be found again as one.
pub fn file_seg(path: &str, expand: Option<&Expand>) -> Seg {
    match expand {
        Some(Expand::Repo(rel)) => Seg::Repo(rel.clone()),
        _ => Seg::File(path.to_string()),
    }
}

/// What an issue's line unfolds into: the commits behind it.
fn issue_expand(issue: &Issue, s: &argus_core::status::RepoStatus) -> Option<Expand> {
    let on_no_remote = |b: &str| {
        Expand::Commits(Range::NotOnRemote {
            tip: format!("refs/heads/{b}"),
        })
    };
    Some(match issue {
        Issue::Unpushed(_) | Issue::UpstreamGone | Issue::NoUpstream => {
            on_no_remote(s.head.branch()?)
        }
        Issue::UnpushedBranches(_) => Expand::UnpushedBranches,
        Issue::DetachedCommits(_) => Expand::Commits(Range::Orphans),
        Issue::Behind(_) => Expand::Commits(Range::Missing {
            tip: format!("refs/remotes/{}", s.upstream.as_ref()?.name),
            from: "HEAD".into(),
        }),
        _ => return None,
    })
}

/// A diff row under a file's line: a side's heading, a hunk header, or one
/// of its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRef {
    pub side: Side,
    /// The hunk's index on that side (headers and lines).
    pub hunk: Option<usize>,
    /// The line (lines only).
    pub line: Option<argus_core::diff::Line>,
    /// The line's index in its hunk (lines only).
    pub at: Option<usize>,
}

const MAX_FILES: usize = 25;
const MAX_STASHES: usize = 10;

fn line(parts: Vec<(Option<Role>, String)>) -> DetailLine {
    DetailLine {
        parts,
        file: None,
        diff: None,
        issue: None,
        ignored: false,
        stash: None,
        branch: None,
        expand: None,
        fix: None,
        body: false,
        links: vec![],
        section: None,
        id: vec![],
    }
}

/// A plain line with these parts, to fill in.
pub fn line_of(parts: Vec<(Option<Role>, String)>) -> DetailLine {
    line(parts)
}

/// A blank line, nothing to stop at: after a diff, before what follows.
pub fn blank() -> DetailLine {
    DetailLine {
        body: true,
        ..line(vec![])
    }
    .at(Seg::Blank)
}

/// A diff line as shown: its `+`, `-` or space and text, in its colour,
/// with a note when the file ends without a newline there.
pub fn diff_line(l: &argus_core::diff::Line) -> Vec<(Option<Role>, String)> {
    use argus_core::diff::LineKind;
    let (role, mark) = match l.kind {
        LineKind::Add => (Some(Role::DiffAdd), '+'),
        LineKind::Remove => (Some(Role::DiffRemove), '-'),
        LineKind::Context => (None, ' '),
    };
    let mut parts = vec![(role, format!("{mark}{}", l.text))];
    if l.no_newline {
        parts.push(styled(Role::Dim, "  (no newline at end of file)"));
    }
    parts
}

fn plain(s: impl Into<String>) -> (Option<Role>, String) {
    (None, s.into())
}

fn styled(r: Role, s: impl Into<String>) -> (Option<Role>, String) {
    (Some(r), s.into())
}

pub(crate) fn ago(t: jiff::Timestamp) -> String {
    ago_since(t, jiff::Timestamp::now())
}

/// How long before `now` `t` was, in the largest unit that reads well.
fn ago_since(t: jiff::Timestamp, now: jiff::Timestamp) -> String {
    let secs = (now - t).get_seconds().max(0);
    match secs {
        s if s < 90 => format!("{s}s ago"),
        s if s < 90 * 60 => format!("{}m ago", s / 60),
        s if s < 36 * 3600 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// The fix lines of a repo whose folder is gone: pruning a stale
/// worktree's record (`stale`), else dropping it or rescanning.
pub fn gone_lines(stale: bool) -> Vec<DetailLine> {
    let (op, keys) = if stale {
        ("prune its record", "x w")
    } else {
        ("drop from the list, or rescan", "x g, x G")
    };
    vec![
        DetailLine {
            fix: stale.then_some(FixKind::PruneWorktrees),
            ..line(vec![
                styled(Role::Dim, "fix       "),
                styled(Role::Ahead, op),
                styled(Role::Dim, format!("  its folder is gone  ({keys})")),
            ])
        }
        .at(Seg::Fix("fix", "gone")),
    ]
}

/// How long from `now` until `t`, in [`ago_since`]'s units, to the
/// nearest (a snooze for 3d has `3d left`, not 2d and change).
fn left_until(t: jiff::Timestamp, now: jiff::Timestamp) -> String {
    let secs = (t - now).get_seconds().max(0);
    let near = |unit: i64| (secs + unit / 2) / unit;
    match secs {
        s if s < 90 => format!("{s}s left"),
        s if s < 90 * 60 => format!("{}m left", near(60)),
        s if s < 36 * 3600 => format!("{}h left", near(3600)),
        _ => format!("{}d left", near(86_400)),
    }
}

/// Who the lines are for, which decides how they say what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hints {
    /// The TUI: the keys that run a fix.
    Keys,
    /// `argus status --details`: the command that runs a fix, and, as
    /// nothing looks further back for a tag afterwards, where the search
    /// for one stopped (the config's `describe_limit`).
    Commands { describe_limit: usize },
}

impl Hints {
    /// How to run a fix of `kind` that isn't the auto-fix; `None` when
    /// nothing need be said (the TUI's Fix menu lists them).
    fn fix(self, kind: FixKind) -> Option<String> {
        let Hints::Commands { .. } = self else {
            return kind.manual().then(|| manual_keys(kind).to_string());
        };
        Some(match kind {
            FixKind::Commit | FixKind::Gitignore | FixKind::Branch | FixKind::AddRemote => {
                format!("in the TUI: {}", manual_keys(kind))
            }
            _ => format!("argus fix -k {}", kind.name()),
        })
    }
}

/// What's known from the repo's forge, to show.
#[derive(Debug, Clone, Copy)]
pub enum ForgeView<'a> {
    /// Asked, not answered yet.
    Loading,
    Info(&'a argus_core::forge::RepoInfo),
    Failed(&'a str),
}

/// Detail lines for `e`, with `icons` for ahead/behind. Fixes: the auto-fix (by `priority`, i.e. `[fix]
/// auto`), what later auto-fixes would apply, then the other fixes.
/// `state` has when its issue snoozes end.
pub fn details(
    e: &Entry,
    priority: &[FixKind],
    stash: &StashConfig,
    icons: &Icons,
    state: &argus_core::State,
    hints: Hints,
    forge: Option<ForgeView>,
) -> Vec<DetailLine> {
    // a labelled line (the label padded to the column the values start in)
    let info = |label: &'static str, rest: Vec<(Option<Role>, String)>| {
        let mut parts = vec![styled(Role::Dim, format!("{label:<10}"))];
        parts.extend(rest);
        line(parts).at(Seg::Info(label))
    };
    let mut v = vec![info("path", vec![plain(contract_tilde(&e.path))])];
    if let Some(err) = e.error() {
        for (n, l) in err.lines().take(6).enumerate() {
            v.push(line(vec![styled(Role::Error, l.to_string())]).at(Seg::Text(n)));
        }
        return v;
    }
    let Some(s) = e.status() else { return v };
    if let Some(u) = &s.remote_url {
        v.push(info(
            "remote",
            vec![plain(format!("{} {u}", s.remote.as_deref().unwrap_or("")))],
        ));
    }
    if let Some(id) = &s.head_commit {
        v.push(info("commit", vec![plain(id.clone())]));
    }
    if !s.tags.is_empty() {
        v.push(info(
            "tags",
            vec![styled(Role::Detached, s.tags.join(", "))],
        ));
    }
    if let Some(d) = &s.describe {
        v.push(info("describe", vec![styled(Role::Detached, d.clone())]));
    } else if s.describe_gave_up {
        v.push(info(
            "describe",
            vec![styled(
                Role::Dim,
                match hints {
                    Hints::Keys => "searching further back for a tag…".to_string(),
                    Hints::Commands { describe_limit: 1 } => "no tag in the last commit".into(),
                    Hints::Commands { describe_limit: n } => {
                        format!("no tag in the last {n} commits")
                    }
                },
            )],
        ));
    }
    let mut times = vec![];
    if let Some(t) = s.last_commit {
        times.push(format!("commit {}", ago(t)));
    }
    if let Some(t) = s.last_fetch {
        times.push(format!("fetched {}", ago(t)));
    } else if s.remote.is_some() {
        times.push("never fetched".into());
    }
    if !times.is_empty() {
        v.push(info("activity", vec![plain(times.join(", "))]));
    }
    let flags = [
        (e.flags.pull, "pulled"),
        (e.flags.ignored, "ignored"),
        (e.flags.snoozed, "snoozed"),
        (e.flags.new, "new"),
    ];
    let mut fl: Vec<String> = e.flags.marks.clone();
    fl.extend(flags.iter().filter(|f| f.0).map(|f| f.1.to_string()));
    if let Some(t) = &e.flags.tracked {
        fl.push(format!("tracked by {t}"));
    }
    if !fl.is_empty() {
        v.push(info("flags", vec![plain(fl.join(", "))]));
    }
    match forge {
        None => {}
        Some(ForgeView::Loading) => v.push(info(
            "forge",
            vec![styled(Role::Dim, "looking up pull requests and issues…")],
        )),
        Some(ForgeView::Failed(why)) => v.push(info("forge", vec![styled(Role::Warning, why)])),
        Some(ForgeView::Info(i)) => {
            // the repo's page, its issues and its pull requests: each a
            // link, for the repo it's a fork of and then for itself
            let mut l = info("forge", vec![]);
            let host = e.id.as_ref().map(|id| id.host.as_str()).unwrap_or_default();
            let counts = |l: &mut DetailLine, repo: &str, issues: usize, pulls: usize| {
                let base = format!("https://{host}/{repo}");
                for (n, one, many, page, after) in [
                    (issues, "issue", "issues", "issues", ", "),
                    (pulls, "pull request", "pull requests", "pulls", ""),
                ] {
                    l.links.push((l.parts.len(), format!("{base}/{page}")));
                    let noun = argus_core::plural(n, one, many);
                    l.parts.push(plain(format!("{n} open {noun}")));
                    if !after.is_empty() {
                        l.parts.push(plain(after));
                    }
                }
            };
            let own = e.id.as_ref().map(|id| id.slug()).unwrap_or_default();
            match &i.parent {
                None => counts(&mut l, &own, i.open_issues, i.open_pulls),
                Some(p) => {
                    l.links
                        .push((l.parts.len(), format!("https://{host}/{}", p.name)));
                    l.parts.push(plain(p.name.clone()));
                    l.parts.push(plain(": "));
                    counts(&mut l, &p.name, p.open_issues, p.open_pulls);
                    if i.open_issues + i.open_pulls > 0 {
                        l.parts.push(plain("; this fork: "));
                        counts(&mut l, &own, i.open_issues, i.open_pulls);
                    }
                }
            }
            v.push(l);
        }
    }
    for issue in &e.issues {
        let (role, text) = describe(issue, s.default_branch.as_deref());
        v.push(
            DetailLine {
                issue: Some(*issue),
                expand: issue_expand(issue, s),
                ..line(vec![styled(Role::Dim, "issue     "), styled(role, text)])
            }
            .at(Seg::Issue(issue.name(), "")),
        );
    }
    for issue in &e.ignored_issues {
        let (_, text) = describe(issue, s.default_branch.as_deref());
        v.push(
            DetailLine {
                issue: Some(*issue),
                expand: issue_expand(issue, s),
                ignored: true,
                ..line(vec![
                    styled(Role::Dim, "issue     "),
                    styled(Role::Dim, format!("{text}  (ignored)")),
                ])
            }
            .at(Seg::Issue(issue.name(), "ignored")),
        );
    }
    let now = jiff::Timestamp::now();
    for issue in &e.snoozed_issues {
        let (_, text) = describe(issue, s.default_branch.as_deref());
        let left = state
            .snoozed_issues
            .get(&e.key)
            .and_then(|m| m.get(issue.name()))
            .map(|t| format!(", {}", left_until(*t, now)))
            .unwrap_or_default();
        v.push(
            DetailLine {
                issue: Some(*issue),
                expand: issue_expand(issue, s),
                ..line(vec![
                    styled(Role::Dim, "issue     "),
                    styled(Role::Dim, format!("{text}  (snoozed{left})")),
                ])
            }
            .at(Seg::Issue(issue.name(), "snoozed")),
        );
    }
    let p = fix::preview(s, &e.fix_kinds(priority), stash);
    let labelled = p
        .chain
        .iter()
        .enumerate()
        .map(|(i, f)| (if i == 0 { "auto-fix  " } else { "then      " }, f));
    for (label, f) in labelled.chain(p.alternatives.iter().map(|f| ("fix       ", f))) {
        let mut parts = vec![
            styled(Role::Dim, label),
            styled(Role::Ahead, f.op.label()),
            styled(Role::Dim, format!("  {}", f.reason)),
        ];
        if let Some(why) = &f.blocked {
            parts.push(styled(Role::Warning, format!("  not automatic: {why}")));
        }
        if let Some(how) = hints.fix(f.kind).filter(|_| label == "fix       ") {
            parts.push(styled(Role::Dim, format!("  ({how})")));
        }
        v.push(
            DetailLine {
                fix: Some(f.kind),
                ..line(parts)
            }
            .at(Seg::Fix(label, f.kind.name())),
        );
    }
    // those that ask for something first, with the keys that run them
    let manual: Vec<fix::Fix> = fix::plan(s, stash)
        .into_iter()
        .filter(|f| f.kind.manual())
        .collect();
    for kind in FixKind::ALL {
        let of_kind: Vec<&fix::Fix> = manual.iter().filter(|f| f.kind == kind).collect();
        let Some(f) = of_kind.first() else { continue };
        // one fix per branch to delete: all of them on one line
        let branches: Vec<String> = of_kind
            .iter()
            .filter_map(|f| match &f.op {
                Op::DeleteBranch { branch, .. } => Some(branch.clone()),
                _ => None,
            })
            .collect();
        let reason = match branches.len() {
            n if n > 1 => format!("{n} merged branches: {}", fix::list(&branches, 3)),
            _ => f.reason.clone(),
        };
        let mut parts = vec![
            styled(Role::Dim, "fix       "),
            styled(Role::Ahead, format!("{}…", kind.name())),
            styled(Role::Dim, format!("  {reason}")),
        ];
        if let Some(why) = &f.blocked {
            parts.push(styled(Role::Warning, format!("  {why}")));
        }
        parts.extend(
            hints
                .fix(kind)
                .map(|how| styled(Role::Dim, format!("  ({how})"))),
        );
        v.push(
            DetailLine {
                fix: Some(kind),
                ..line(parts)
            }
            .at(Seg::Fix("…", kind.name())),
        );
    }
    if !s.branches.is_empty() {
        v.push(line(vec![styled(Role::Dim, "branches")]).at(Seg::Branches));
        for b in &s.branches {
            let mut parts = vec![
                plain(if b.is_head { "  * " } else { "    " }),
                styled(
                    if s.default_branch.as_deref() == Some(&b.name) {
                        Role::DefaultBranch
                    } else {
                        Role::Branch
                    },
                    b.name.clone(),
                ),
            ];
            match &b.upstream {
                Some(t) if t.gone => {
                    parts.push(styled(Role::Warning, format!("  {} gone", t.name)))
                }
                Some(t) => {
                    parts.push(styled(Role::Dim, format!("  {}", t.name)));
                    if t.ahead > 0 {
                        parts.push(styled(Role::Ahead, format!(" {}{}", icons.ahead, t.ahead)));
                    }
                    if t.behind > 0 {
                        parts.push(styled(
                            Role::Behind,
                            format!(" {}{}", icons.behind, t.behind),
                        ));
                    }
                }
                None => parts.push(styled(Role::Dim, "  (no upstream)")),
            }
            if b.merged && s.default_branch.as_deref() != Some(&b.name) {
                parts.push(styled(Role::Dim, "  merged"));
            } else if b.absorbed {
                parts.push(styled(Role::Dim, "  merged (squashed or rebased)"));
            }
            let unpushed = b.unpushed.filter(|&n| n > 0);
            // unless ↑ already says it
            let ahead = b.upstream.as_ref().filter(|t| !t.gone).map(|t| t.ahead);
            if let Some(n) = unpushed.filter(|&n| ahead != Some(n)) {
                parts.push(styled(Role::Dim, format!("  {n} on no remote")));
            }
            let pull = match forge {
                Some(ForgeView::Info(i)) => i.pull(&b.name),
                _ => None,
            };
            let mut link = None;
            if let Some(p) = pull {
                use argus_core::forge::{Checks, PullState, Review};
                let role = match (p.state, p.review, p.checks) {
                    (PullState::Open, Some(Review::ChangesRequested), _)
                    | (PullState::Open, _, Some(Checks::Failing)) => Role::Warning,
                    (PullState::Open, _, _) => Role::Ahead,
                    _ => Role::Dim,
                };
                parts.push(plain("  "));
                // (a pull request without a usable address isn't a link)
                link = (!p.url.is_empty()).then(|| (parts.len(), p.url.clone()));
                parts.push(styled(role, p.summary()));
                parts.push(styled(Role::Dim, format!("  {}", p.title)));
            }
            v.push(
                DetailLine {
                    links: link.into_iter().collect(),
                    branch: Some(b.name.clone()),
                    expand: unpushed.map(|_| {
                        Expand::Commits(Range::NotOnRemote {
                            tip: format!("refs/heads/{}", b.name),
                        })
                    }),
                    ..line(parts)
                }
                .at(Seg::Branches)
                .at(Seg::Branch(b.name.clone())),
            );
        }
    }
    // changes by section, as magit has them: each headed by its own line
    // (which folds it), its files below, a file in every section it has
    // changes in
    for section in Section::ALL {
        let files: Vec<_> = s
            .changes
            .files
            .iter()
            .filter_map(|f| Some((f, section.code(f)?)))
            .collect();
        if files.is_empty() {
            continue;
        }
        v.push(
            DetailLine {
                section: Some(section),
                ..line(vec![styled(
                    Role::Dim,
                    format!("{} ({})", section.title(), files.len()),
                )])
            }
            .at(Seg::Section(section)),
        );
        for (f, (role, code)) in files.iter().take(MAX_FILES) {
            // (an untracked file's place says what it is)
            let mut parts = vec![plain("    ")];
            match section {
                Section::Untracked => parts.push(styled(*role, f.path.clone())),
                _ => {
                    parts.push(styled(*role, code.to_string()));
                    parts.push(plain(format!(" {}", f.path)));
                }
            }
            let expand = (section == Section::Untracked)
                .then(|| untracked_expand(&f.path, f.kind))
                .flatten();
            v.push(
                DetailLine {
                    file: Some(f.path.clone()),
                    section: Some(section),
                    ..line(parts)
                }
                .under(&[Seg::Section(section), file_seg(&f.path, expand.as_ref())])
                .unfolding(expand),
            );
        }
        let more = files.len().saturating_sub(MAX_FILES);
        if more > 0 {
            v.push(
                DetailLine {
                    section: Some(section),
                    body: true,
                    ..line(vec![styled(Role::Dim, format!("    … {more} more"))])
                }
                .at(Seg::Section(section))
                .at(Seg::More),
            );
        }
    }
    if !s.stashes.is_empty() {
        let ignored = e.stash_ignored.iter().filter(|&&i| i).count();
        let mut parts = vec![
            styled(Role::Dim, "stashes   "),
            styled(Role::Stash, s.stashes.len().to_string()),
        ];
        if ignored > 0 {
            parts.push(styled(Role::Dim, format!(" ({ignored} ignored)")));
        }
        v.push(line(parts).at(Seg::Stashes));
        for (n, message) in s.stashes.iter().enumerate().take(MAX_STASHES) {
            let label = format!("          stash@{{{n}}} {message}");
            let id = s.stash_ids.get(n);
            v.push(
                DetailLine {
                    stash: Some(n),
                    expand: id.map(|id| Expand::Commit(id.clone())),
                    ..line(vec![if e.stash_ignored.get(n) == Some(&true) {
                        styled(Role::Dim, format!("{label}  (ignored)"))
                    } else {
                        plain(label)
                    }])
                }
                .at(Seg::Stashes)
                .at(Seg::Stash(id.cloned().unwrap_or_else(|| n.to_string()))),
            );
        }
        let more = s.stashes.len().saturating_sub(MAX_STASHES);
        if more > 0 {
            v.push(
                line(vec![styled(Role::Dim, format!("          … {more} more"))])
                    .at(Seg::Stashes)
                    .at(Seg::More),
            );
        }
    }
    v
}

pub fn describe(i: &Issue, default: Option<&str>) -> (Role, String) {
    match i {
        Issue::Conflicts(n) => (Role::Conflict, format!("{n} conflicted path(s)")),
        Issue::InProgress => (
            Role::Conflict,
            "operation in progress: finish or abort it".into(),
        ),
        Issue::Dirty(n) => (Role::Unstaged, format!("{n} uncommitted path(s)")),
        Issue::Unpushed(n) => (Role::Ahead, format!("{n} unpushed commit(s)")),
        Issue::UpstreamGone => (
            Role::Warning,
            "upstream branch deleted on remote, with unmerged commits".into(),
        ),
        Issue::NoUpstream => (
            Role::Warning,
            "branch has no upstream and isn't merged".into(),
        ),
        Issue::UnpushedBranches(n) => (
            Role::Warning,
            format!("{n} other local branch(es) with unpushed work"),
        ),
        Issue::Detached => (Role::Detached, "detached HEAD".into()),
        Issue::NonDefaultBranch => (
            Role::Branch,
            format!(
                "not on default branch{}",
                default.map(|d| format!(" ({d})")).unwrap_or_default()
            ),
        ),
        Issue::Stashes(n) => (Role::Stash, format!("{n} stash(es)")),
        Issue::Behind(n) => (Role::Behind, format!("{n} commit(s) behind upstream")),
        Issue::DetachedCommits(n) => (
            Role::Conflict,
            format!("{n} commit(s) on detached HEAD in no branch: create one before switching"),
        ),
        Issue::NoRemote => (Role::Dim, "no remote".into()),
        Issue::FinishedWorktree => (
            Role::Branch,
            format!(
                "finished worktree: its branch is merged{} and its upstream gone",
                default.map(|d| format!(" into {d}")).unwrap_or_default()
            ),
        ),
        Issue::StaleWorktrees(n) => (
            Role::Warning,
            format!(
                "{n} worktree record(s) whose folder is gone: their branches count as checked out"
            ),
        ),
    }
}

/// The keys that run a fix of `kind`, which asks for something first.
fn manual_keys(kind: FixKind) -> &'static str {
    match kind {
        FixKind::Abort => "x k",
        FixKind::Commit => "c A",
        FixKind::Gitignore => "i",
        FixKind::Branch => "b c",
        FixKind::AddRemote => "P r",
        FixKind::DeleteBranches => "b x",
        FixKind::RemoveWorktree => "x W",
        _ => "x x",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Action;
    use crate::menu::MenuId;

    #[test]
    fn a_file_staged_for_deletion_and_still_there_is_staged_and_untracked() {
        use argus_core::status::FileChange;
        let f = |index, worktree| FileChange {
            path: "f".into(),
            index,
            worktree,
            kind: PathKind::File,
        };
        let codes = |f: &FileChange| Section::ALL.map(|s| s.code(f).map(|c| c.1));
        assert_eq!(codes(&f('D', '?')), [Some('?'), None, Some('D')]);
        // inverse: changed in both, it's unstaged and staged
        assert_eq!(codes(&f('M', 'M')), [None, Some('M'), Some('M')]);
        assert_eq!(codes(&f('?', '?')), [Some('?'), None, None]);
    }

    #[test]
    fn ago_switches_unit_at_one_and_a_half_of_each() {
        let now = jiff::Timestamp::from_second(1_000_000_000).unwrap();
        let ago = |secs: i64| ago_since(now - jiff::SignedDuration::from_secs(secs), now);
        for (secs, want) in [
            (0, "0s ago"),
            (89, "89s ago"),
            (90, "1m ago"),
            (90 * 60 - 1, "89m ago"),
            (90 * 60, "1h ago"),
            (36 * 3600 - 1, "35h ago"),
            (36 * 3600, "1d ago"),
            (10 * 86_400, "10d ago"),
            (-5, "0s ago"),
        ] {
            assert_eq!(ago(secs), want, "{secs}");
        }
    }

    #[test]
    fn left_until_rounds_to_the_nearest_unit() {
        let now = jiff::Timestamp::from_second(1_000_000_000).unwrap();
        let left = |secs: i64| left_until(now + jiff::SignedDuration::from_secs(secs), now);
        for (secs, want) in [
            (0, "0s left"),
            (89, "89s left"),
            (90, "2m left"),
            (90 * 60 - 1, "90m left"),
            (90 * 60, "2h left"),
            (36 * 3600 - 1, "36h left"),
            (36 * 3600, "2d left"),
            (3 * 86_400 - 1, "3d left"),
            (-5, "0s left"),
        ] {
            assert_eq!(left(secs), want, "{secs}");
        }
    }

    #[test]
    fn manual_fix_keys_lead_to_their_actions() {
        for kind in FixKind::ALL.into_iter().filter(|k| k.manual()) {
            let keys: Vec<char> = manual_keys(kind)
                .split(' ')
                .flat_map(|k| k.chars())
                .collect();
            let action = match keys.as_slice() {
                ['i'] => Action::GitignoreMenu,
                // the Fix menu's own item for it, built when opened
                ['x', k] => {
                    assert_eq!(crate::menu::fix_key(kind), *k, "{kind:?}");
                    continue;
                }
                [m, k] => {
                    let menu = MenuId::from_key(*m).unwrap().menu();
                    menu.items.iter().find(|i| i.key == *k).unwrap().action
                }
                other => panic!("{kind:?}: {other:?}"),
            };
            let expect = match kind {
                FixKind::Abort => Action::AbortOperation,
                FixKind::Commit => Action::Commit { all: true },
                FixKind::Gitignore => Action::GitignoreMenu,
                // a branch at HEAD names a detached HEAD's commits
                FixKind::Branch => Action::NewBranch,
                FixKind::AddRemote => Action::AddRemote,
                FixKind::DeleteBranches => Action::DeleteMergedBranches,
                k => panic!("{k:?} isn't manual"),
            };
            assert_eq!(action, expect, "{kind:?}");
        }
    }
}
