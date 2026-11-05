//! Unfolded detail lines for a repo (magit-style sections, flattened).

use argus_core::Entry;
use argus_core::attention::Issue;
use argus_core::badges::Role;
use argus_core::fix::{self, FixKind};
use argus_core::paths::contract_tilde;

/// A detail line: styled parts. `None` uses the plain detail style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLine {
    pub parts: Vec<(Option<Role>, String)>,
}

const MAX_FILES: usize = 25;

fn line(parts: Vec<(Option<Role>, String)>) -> DetailLine {
    DetailLine { parts }
}

fn plain(s: impl Into<String>) -> (Option<Role>, String) {
    (None, s.into())
}

fn styled(r: Role, s: impl Into<String>) -> (Option<Role>, String) {
    (Some(r), s.into())
}

fn ago(t: jiff::Timestamp) -> String {
    let secs = (jiff::Timestamp::now() - t).get_seconds().max(0);
    match secs {
        s if s < 90 => format!("{s}s ago"),
        s if s < 90 * 60 => format!("{}m ago", s / 60),
        s if s < 36 * 3600 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}

/// Detail lines for `e`. Fixes are listed in `priority` order (`[fix] auto`),
/// the one an auto-fix action would apply marked as such.
pub fn details(e: &Entry, priority: &[FixKind]) -> Vec<DetailLine> {
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
        (e.flags.sync, "syncable"),
        (e.flags.excluded, "excluded"),
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
        v.push(line(vec![
            styled(Role::Dim, "issue     "),
            styled(role, text),
        ]));
    }
    let auto = fix::choose(s, priority);
    for f in fix::ranked(s, priority) {
        if f.kind.needs_input() {
            continue;
        }
        let label = if Some(&f) == auto.as_ref() {
            "auto-fix  "
        } else {
            "fix       "
        };
        v.push(line(vec![
            styled(Role::Dim, label),
            styled(Role::Ahead, f.op.label()),
            styled(Role::Dim, format!("  {}", f.reason)),
        ]));
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
                        parts.push(styled(Role::Ahead, format!(" ↑{}", t.ahead)));
                    }
                    if t.behind > 0 {
                        parts.push(styled(Role::Behind, format!(" ↓{}", t.behind)));
                    }
                }
                None => parts.push(styled(Role::Dim, "  (no upstream)")),
            }
            if b.merged && s.default_branch.as_deref() != Some(&b.name) {
                parts.push(styled(Role::Dim, "  merged"));
            }
            v.push(line(parts));
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
            v.push(line(vec![
                plain("    "),
                styled(role, format!("{}{}", f.index, f.worktree)),
                plain(format!(" {}", f.path)),
            ]));
        }
        let more = s.changes.total().saturating_sub(MAX_FILES);
        if more > 0 {
            v.push(line(vec![styled(Role::Dim, format!("    … {more} more"))]));
        }
    }
    if s.stashes > 0 {
        v.push(line(vec![
            styled(Role::Dim, "stashes   "),
            styled(Role::Stash, s.stashes.to_string()),
        ]));
    }
    v
}

fn describe(i: &Issue, default: Option<&str>) -> (Role, String) {
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
        Issue::OffDefault => (
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
    }
}
