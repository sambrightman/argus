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
    /// What Tab unfolds it into, read when unfolded.
    pub expand: Option<Expand>,
    /// The fix this line offers, for the Fix menu to start on.
    pub fix: Option<FixKind>,
    /// Body text (a commit message line, a diff line), which `n`/`p` skip
    /// as magit's do, stopping only at section lines.
    pub body: bool,
}

impl DetailLine {
    /// Body text: see [`Self::body`]; a working tree diff's lines too.
    pub fn is_body(&self) -> bool {
        self.body || self.diff.as_ref().is_some_and(|d| d.line.is_some())
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
        expand: None,
        fix: None,
        body: false,
    }
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

/// Detail lines for `e`, with `icons` for ahead/behind. Fixes: the auto-fix (by `priority`, i.e. `[fix]
/// auto`), what later auto-fixes would apply, then the other fixes.
/// `state` has when its issue snoozes end.
pub fn details(
    e: &Entry,
    priority: &[FixKind],
    stash: &StashConfig,
    icons: &Icons,
    state: &argus_core::State,
) -> Vec<DetailLine> {
    let mut v = vec![line(vec![
        styled(Role::Dim, "path      "),
        plain(contract_tilde(&e.path)),
    ])];
    if let Some(err) = e.error() {
        for l in err.lines().take(6) {
            v.push(line(vec![styled(Role::Error, l.to_string())]));
        }
        return v;
    }
    let Some(s) = e.status() else { return v };
    if let Some(u) = &s.remote_url {
        v.push(line(vec![
            styled(Role::Dim, "remote    "),
            plain(format!("{} {u}", s.remote.as_deref().unwrap_or(""))),
        ]));
    }
    if let Some(id) = &s.head_commit {
        v.push(line(vec![
            styled(Role::Dim, "commit    "),
            plain(id.clone()),
        ]));
    }
    if !s.tags.is_empty() {
        v.push(line(vec![
            styled(Role::Dim, "tags      "),
            styled(Role::Detached, s.tags.join(", ")),
        ]));
    }
    if let Some(d) = &s.describe {
        v.push(line(vec![
            styled(Role::Dim, "describe  "),
            styled(Role::Detached, d.clone()),
        ]));
    } else if s.describe_gave_up {
        v.push(line(vec![
            styled(Role::Dim, "describe  "),
            styled(Role::Dim, "searching further back for a tag…"),
        ]));
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
        v.push(line(vec![
            styled(Role::Dim, "activity  "),
            plain(times.join(", ")),
        ]));
    }
    let flags = [
        (e.flags.favourite, "favourite"),
        (e.flags.pull, "pulled"),
        (e.flags.ignored, "ignored"),
        (e.flags.snoozed, "snoozed"),
        (e.flags.new, "new"),
    ];
    let mut fl: Vec<String> = flags
        .iter()
        .filter(|f| f.0)
        .map(|f| f.1.to_string())
        .collect();
    if let Some(t) = &e.flags.tracked {
        fl.push(format!("tracked by {t}"));
    }
    if !fl.is_empty() {
        v.push(line(vec![
            styled(Role::Dim, "flags     "),
            plain(fl.join(", ")),
        ]));
    }
    for issue in &e.issues {
        let (role, text) = describe(issue, s.default_branch.as_deref());
        v.push(DetailLine {
            issue: Some(*issue),
            expand: issue_expand(issue, s),
            ..line(vec![styled(Role::Dim, "issue     "), styled(role, text)])
        });
    }
    for issue in &e.ignored_issues {
        let (_, text) = describe(issue, s.default_branch.as_deref());
        v.push(DetailLine {
            issue: Some(*issue),
            expand: issue_expand(issue, s),
            ignored: true,
            ..line(vec![
                styled(Role::Dim, "issue     "),
                styled(Role::Dim, format!("{text}  (ignored)")),
            ])
        });
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
        v.push(DetailLine {
            issue: Some(*issue),
            expand: issue_expand(issue, s),
            ..line(vec![
                styled(Role::Dim, "issue     "),
                styled(Role::Dim, format!("{text}  (snoozed{left})")),
            ])
        });
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
        v.push(DetailLine {
            fix: Some(f.kind),
            ..line(parts)
        });
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
                Op::DeleteBranch { branch } => Some(branch.clone()),
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
        parts.push(styled(Role::Dim, format!("  ({})", manual_keys(kind))));
        v.push(DetailLine {
            fix: Some(kind),
            ..line(parts)
        });
    }
    if !s.branches.is_empty() {
        v.push(line(vec![styled(Role::Dim, "branches")]));
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
            }
            let unpushed = b.unpushed.filter(|&n| n > 0);
            // unless ↑ already says it
            let ahead = b.upstream.as_ref().filter(|t| !t.gone).map(|t| t.ahead);
            if let Some(n) = unpushed.filter(|&n| ahead != Some(n)) {
                parts.push(styled(Role::Dim, format!("  {n} on no remote")));
            }
            v.push(DetailLine {
                expand: unpushed.map(|_| {
                    Expand::Commits(Range::NotOnRemote {
                        tip: format!("refs/heads/{}", b.name),
                    })
                }),
                ..line(parts)
            });
        }
    }
    if !s.changes.files.is_empty() {
        v.push(line(vec![styled(
            Role::Dim,
            format!("changes ({})", s.changes.total()),
        )]));
        for f in s.changes.files.iter().take(MAX_FILES) {
            let role = match (f.index, f.worktree) {
                ('U', _) => Role::Conflict,
                ('?', _) => Role::Untracked,
                (x, _) if x != ' ' => Role::Staged,
                _ => Role::Unstaged,
            };
            v.push(DetailLine {
                file: Some(f.path.clone()),
                expand: (f.index == '?')
                    .then(|| untracked_expand(&f.path, f.kind))
                    .flatten(),
                ..line(vec![
                    plain("    "),
                    styled(role, format!("{}{}", f.index, f.worktree)),
                    plain(format!(" {}", f.path)),
                ])
            });
        }
        let more = s.changes.total().saturating_sub(MAX_FILES);
        if more > 0 {
            v.push(line(vec![styled(Role::Dim, format!("    … {more} more"))]));
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
        v.push(line(parts));
        for (n, message) in s.stashes.iter().enumerate().take(MAX_STASHES) {
            let label = format!("          stash@{{{n}}} {message}");
            v.push(DetailLine {
                stash: Some(n),
                expand: s.stash_ids.get(n).map(|id| Expand::Commit(id.clone())),
                ..line(vec![if e.stash_ignored.get(n) == Some(&true) {
                    styled(Role::Dim, format!("{label}  (ignored)"))
                } else {
                    plain(label)
                }])
            });
        }
        let more = s.stashes.len().saturating_sub(MAX_STASHES);
        if more > 0 {
            v.push(line(vec![styled(
                Role::Dim,
                format!("          … {more} more"),
            )]));
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
        FixKind::Abort => "x A",
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
