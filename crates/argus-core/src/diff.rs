//! One file's changes as hunks, read with gix (no `git diff` process): the
//! index against the work tree (unstaged), or HEAD against the index
//! (staged). Content is compared as git stores it, so filters such as
//! `core.autocrlf` apply and binary files are detected as git does.

use std::path::Path;

use gix::bstr::BStr;
use gix::diff::blob::{
    self, ResourceKind, UnifiedDiff,
    pipeline::{Mode, WorktreeRoots},
    platform::prepare_diff::Operation,
    unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader},
};
use gix::object::tree::EntryKind;
use serde::Serialize;

/// Which changes of a file to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Side {
    /// Index → work tree (untracked files: everything added).
    Unstaged,
    /// HEAD → index.
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub kind: LineKind,
    /// Without the line ending.
    pub text: String,
    /// The file ends here without a newline (git's "\ No newline at end of
    /// file").
    pub no_newline: bool,
    /// The line ends with a carriage return (before its newline, if it
    /// has one): kept apart from the text, which is for showing.
    pub cr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hunk {
    /// 1-based start and length in the old and new versions, as in a
    /// unified diff's `@@ -a,b +c,d @@`.
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// `@@ -a,b +c,d @@`, written as git writes it: `,1` left out, and an
    /// empty side numbered from the line before (0 for an empty file).
    pub fn header(&self) -> String {
        header(self.old_start, self.old_len, self.new_start, self.new_len)
    }
}

fn header(old_start: u32, old_len: u32, new_start: u32, new_len: u32) -> String {
    let range = |start: u32, len: u32| match len {
        0 => format!("{},0", start.saturating_sub(1)),
        1 => format!("{start}"),
        n => format!("{start},{n}"),
    };
    format!(
        "@@ -{} +{} @@",
        range(old_start, old_len),
        range(new_start, new_len)
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileDiff {
    /// No hunks: no changes on that side.
    Text(Vec<Hunk>),
    /// The hunks of a side converted to text for comparing (the file's
    /// `diff.<driver>.textconv` program): for reading, not for applying.
    Converted(Vec<Hunk>),
    /// The hunks of a file with bytes that aren't UTF-8, shown as near as
    /// text can: for reading, not for applying (a patch of them would
    /// write the stand-in characters).
    Inexact(Vec<Hunk>),
    Binary,
    /// An external diff program is configured for the file (`diff=…`).
    External,
}

/// A patch of `path`'s `hunk` for `git apply`: all its changes, or with
/// `only` just the lines at those indices, the others left as they are
/// where the patch applies. `reverse` for one applied backwards (`-R`:
/// unstaging, discarding), where that's the hunk's new side.
pub fn patch(path: &str, hunk: &Hunk, only: Option<&[usize]>, reverse: bool) -> String {
    patch_hunks(path, &[(hunk, only)], reverse)
}

/// A line of a patch being written: its sign, and the line.
type Written<'a> = (char, &'a Line, bool);

/// [`patch`] of several of `path`'s hunks at once, in the file's order,
/// each with its lines.
///
/// What's left out changes what git must be told. Each hunk says where it
/// goes on the side the patch is applied to, as the diff has it, and on
/// the other side where that puts it given the hunks before it in this
/// patch (git goes by that to place a hunk whose surroundings are found
/// in several places). A line left out beside one taken stays where it
/// is, the taken one's replacement in the taken one's place. And only a
/// side's last line can lack a newline: one that would end up followed
/// by another is given its newline.
pub fn patch_hunks(path: &str, hunks: &[(&Hunk, Option<&[usize]>)], reverse: bool) -> String {
    let (a, b) = (quoted("a/", path), quoted("b/", path));
    let mut out = format!("diff --git {a} {b}\n--- {a}\n+++ {b}\n");
    // how far this patch's earlier hunks move the lines after them
    let mut moved: i64 = 0;
    for (hunk, only) in hunks {
        let lines = written(hunk, *only, reverse);
        let count = |sign: char| lines.iter().filter(|l| l.0 == sign).count() as i64;
        let (kept, removed, added) = (count(' '), count('-'), count('+'));
        let at = |start: u32, by: i64| (i64::from(start) + by).max(0) as u32;
        let (old_start, new_start) = match reverse {
            false => (hunk.old_start, at(hunk.old_start, moved)),
            true => (at(hunk.new_start, -moved), hunk.new_start),
        };
        let (old_len, new_len) = ((kept + removed) as u32, (kept + added) as u32);
        out.push_str(&header(old_start, old_len, new_start, new_len));
        out.push('\n');
        moved += added - removed;
        for (sign, l, no_newline) in lines {
            out.push(sign);
            out.push_str(&l.text);
            if l.cr {
                out.push('\r');
            }
            out.push('\n');
            if no_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }
    out
}

/// `prefix` and `path` as a patch names a file: in double quotes with
/// C escapes when the path has an ASCII control character, a quote or
/// a backslash in it (as git writes it, and must have it to read it back).
fn quoted(prefix: &str, path: &str) -> String {
    let odd = |c: char| c.is_ascii_control() || c == '"' || c == '\\';
    if !path.chars().any(odd) {
        return format!("{prefix}{path}");
    }
    let mut out = format!("\"{prefix}");
    for c in path.chars() {
        match c {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii_control() => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The lines of `hunk` as a patch of the chosen ones has them.
fn written<'a>(hunk: &'a Hunk, only: Option<&[usize]>, reverse: bool) -> Vec<Written<'a>> {
    let chosen = |i: usize| only.is_none_or(|o| o.contains(&i));
    // the side the patch ends up with, and the one it's applied to
    let (result, target) = if reverse { ('-', '+') } else { ('+', '-') };
    let mut lines: Vec<Written> = vec![];
    let mut i = 0;
    while i < hunk.lines.len() {
        if hunk.lines[i].kind == LineKind::Context {
            lines.push((' ', &hunk.lines[i], hunk.lines[i].no_newline));
            i += 1;
            continue;
        }
        // a run of changes: its removed lines, then its added ones
        let run = hunk.lines[i..]
            .iter()
            .take_while(|l| l.kind != LineKind::Context)
            .count();
        let of = |kind: LineKind| {
            let run = (i..i + run).filter(move |&n| hunk.lines[n].kind == kind);
            run.map(|n| (chosen(n), &hunk.lines[n]))
        };
        let (removed, added): (Vec<_>, Vec<_>) =
            (of(LineKind::Remove).collect(), of(LineKind::Add).collect());
        // The side applied to is there as it is: its chosen lines to go,
        // the others staying. The other side's chosen lines come in where
        // the chosen ones go: forwards after the last removed line taken,
        // backwards before the first added line taken.
        let (theirs, ours, ours_sign) = match reverse {
            false => (&removed, &added, '+'),
            true => (&added, &removed, '-'),
        };
        let coming = ours.iter().filter(|(taken, _)| *taken);
        let coming: Vec<Written> = coming.map(|(_, l)| (ours_sign, *l, l.no_newline)).collect();
        let place = match reverse {
            false => theirs.iter().rposition(|(taken, _)| *taken).map(|n| n + 1),
            true => theirs.iter().position(|(taken, _)| *taken),
        };
        let place = place.unwrap_or(if reverse { 0 } else { theirs.len() });
        let staying =
            |(taken, l): &(bool, &'a Line)| (if *taken { target } else { ' ' }, *l, l.no_newline);
        let block = theirs[..place].iter().map(staying);
        let block = block
            .chain(coming)
            .chain(theirs[place..].iter().map(staying));
        lines.extend(block);
        i += run;
    }
    // only the last line of the side being made can lack a newline
    let mut last = lines.iter().rposition(|l| l.0 == ' ' || l.0 == result);
    let mut n = 0;
    while n < lines.len() {
        let (sign, l, no_newline) = lines[n];
        if no_newline && Some(n) != last {
            if sign == result {
                lines[n].2 = false;
            } else if sign == ' ' {
                // there without a newline, here with one
                lines[n] = (target, l, true);
                lines.insert(n + 1, (result, l, false));
                last = last.map(|at| at + 1);
                n += 1;
            }
        }
        n += 1;
    }
    lines
}

#[derive(Debug, thiserror::Error)]
#[error("diff of {path}: {message}")]
pub struct DiffError {
    pub path: String,
    pub message: String,
}

/// The `side` changes of `path` (relative to the work tree) in the repo at
/// `repo`, with 3 lines of context.
pub fn file_diff(repo: &Path, path: &str, side: Side) -> Result<FileDiff, DiffError> {
    let side_name = match side {
        Side::Unstaged => "unstaged",
        Side::Staged => "staged",
    };
    crate::trace::timed(
        |_| {
            format!(
                "{side_name} diff of {path} in {}",
                crate::paths::contract_tilde(repo)
            )
        },
        || read_file_diff(repo, path, side),
    )
}

fn read_file_diff(repo: &Path, path: &str, side: Side) -> Result<FileDiff, DiffError> {
    let err = |e: &dyn std::fmt::Display| DiffError {
        path: path.to_string(),
        message: e.to_string(),
    };
    let repo = crate::isolation::open(repo).map_err(|e| err(&e))?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| err(&"no work tree"))?
        .to_path_buf();
    let index = repo.index_or_empty().map_err(|e| err(&e))?;
    let rela: &BStr = path.into();
    let in_index = index
        .entry_by_path(rela)
        .map(|e| (e.id, e.mode.to_tree_entry_mode().map(|m| m.kind())));
    let null = gix::ObjectId::null(repo.object_hash());
    let kind = |k: Option<Option<EntryKind>>| k.flatten().unwrap_or(EntryKind::Blob);
    // (id, kind) for each side; a null id is an empty (missing) version
    let (old, new, new_root) = match side {
        Side::Unstaged => {
            let old = in_index.unwrap_or((null, None));
            let exists = workdir.join(path).symlink_metadata().is_ok();
            let new_kind = in_index.map(|(_, k)| k).unwrap_or(None);
            (
                (old.0, kind(Some(old.1))),
                (null, kind(Some(new_kind))),
                exists.then(|| workdir.clone()),
            )
        }
        Side::Staged => {
            let in_head = repo
                .head_tree()
                .ok()
                .and_then(|t| t.lookup_entry_by_path(path).ok().flatten())
                .map(|e| (e.object_id(), Some(e.mode().kind())));
            let old = in_head.unwrap_or((null, None));
            let new = in_index.unwrap_or((null, None));
            ((old.0, kind(Some(old.1))), (new.0, kind(Some(new.1))), None)
        }
    };
    blob_diff(&repo, rela, old, new, new_root).map_err(|e| err(&e))
}

/// The changes `commit` (any revision) made to `path` against its first
/// parent (an empty file for a root commit), with 3 lines of context. A
/// rename is given as `old -> new`, as [`crate::history`] lists it.
pub fn commit_file_diff(repo: &Path, commit: &str, path: &str) -> Result<FileDiff, DiffError> {
    crate::trace::timed(
        |_| {
            format!(
                "diff of {path} in {commit} in {}",
                crate::paths::contract_tilde(repo)
            )
        },
        || read_commit_file_diff(repo, commit, path),
    )
}

fn read_commit_file_diff(repo: &Path, commit: &str, path: &str) -> Result<FileDiff, DiffError> {
    let err = |e: &dyn std::fmt::Display| DiffError {
        path: path.to_string(),
        message: e.to_string(),
    };
    let (old_path, new_path) = path.split_once(" -> ").unwrap_or((path, path));
    let repo = crate::isolation::open(repo).map_err(|e| err(&e))?;
    let c = repo
        .rev_parse_single(commit)
        .map_err(|e| err(&e))?
        .object()
        .map_err(|e| err(&e))?
        .try_into_commit()
        .map_err(|e| err(&e))?;
    let null = gix::ObjectId::null(repo.object_hash());
    let entry = |tree: Option<gix::Tree<'_>>, p: &str| -> (gix::ObjectId, EntryKind) {
        tree.and_then(|t| t.lookup_entry_by_path(p).ok().flatten())
            .map(|e| (e.object_id(), e.mode().kind()))
            .unwrap_or((null, EntryKind::Blob))
    };
    let new = entry(c.tree().ok(), new_path);
    let parent = c
        .parent_ids()
        .next()
        .and_then(|p| repo.find_commit(p).ok())
        .and_then(|p| p.tree().ok());
    let old = entry(parent, old_path);
    blob_diff(&repo, new_path.into(), old, new, None).map_err(|e| err(&e))
}

/// The diff between two versions of `rela` (a null id: missing), the new
/// one read from `new_root` when given (the work tree).
fn blob_diff(
    repo: &gix::Repository,
    rela: &BStr,
    old: (gix::ObjectId, EntryKind),
    new: (gix::ObjectId, EntryKind),
    new_root: Option<std::path::PathBuf>,
) -> Result<FileDiff, Box<dyn std::error::Error + Send + Sync>> {
    let mut cache = repo.diff_resource_cache(
        Mode::ToGitUnlessBinaryToTextIsPresent,
        WorktreeRoots {
            old_root: None,
            new_root,
        },
    )?;
    cache.set_resource(old.0, old.1, rela, ResourceKind::OldOrSource, &repo.objects)?;
    cache.set_resource(
        new.0,
        new.1,
        rela,
        ResourceKind::NewOrDestination,
        &repo.objects,
    )?;
    let outcome = cache.prepare_diff()?;
    let converted = outcome.old_or_new_is_derived;
    let algorithm = match outcome.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::SourceOrDestinationIsBinary => return Ok(FileDiff::Binary),
        Operation::ExternalCommand { .. } => return Ok(FileDiff::External),
    };
    // lines keep their endings, as git compares them: a file losing its
    // last newline changes its last line
    let input = blob::InternedInput::new(outcome.old.intern_source(), outcome.new.intern_source());
    let diff = blob::diff_with_slider_heuristics(algorithm, &input);
    let collect = Collect {
        hunks: vec![],
        exact: true,
    };
    let (hunks, exact) =
        UnifiedDiff::new(&diff, &input, collect, ContextSize::symmetrical(3)).consume()?;
    Ok(match (converted, exact) {
        (true, _) => FileDiff::Converted(hunks),
        (false, true) => FileDiff::Text(hunks),
        (false, false) => FileDiff::Inexact(hunks),
    })
}

/// The hunks, and whether their lines are the file's own, byte for byte
/// (every one valid UTF-8).
struct Collect {
    hunks: Vec<Hunk>,
    exact: bool,
}

impl ConsumeHunk for Collect {
    type Out = (Vec<Hunk>, bool);

    fn consume_hunk(
        &mut self,
        h: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let lines = lines
            .iter()
            .map(|(k, bytes)| {
                // the ending is a newline, with the carriage return before
                // it if there's one: any others are the line's own
                let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
                let (body, cr) = match body.strip_suffix(b"\r") {
                    Some(body) => (body, true),
                    None => (body, false),
                };
                self.exact &= std::str::from_utf8(body).is_ok();
                Line {
                    kind: match k {
                        DiffLineKind::Context => LineKind::Context,
                        DiffLineKind::Add => LineKind::Add,
                        DiffLineKind::Remove => LineKind::Remove,
                    },
                    text: String::from_utf8_lossy(body).into_owned(),
                    no_newline: !bytes.ends_with(b"\n"),
                    cr,
                }
            })
            .collect();
        self.hunks.push(Hunk {
            old_start: h.before_hunk_start,
            old_len: h.before_hunk_len,
            new_start: h.after_hunk_start,
            new_len: h.after_hunk_len,
            lines,
        });
        Ok(())
    }

    fn finish(self) -> Self::Out {
        (self.hunks, self.exact)
    }
}
