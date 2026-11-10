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
        let range = |start: u32, len: u32| match len {
            0 => format!("{},0", start.saturating_sub(1)),
            1 => format!("{start}"),
            n => format!("{start},{n}"),
        };
        format!(
            "@@ -{} +{} @@",
            range(self.old_start, self.old_len),
            range(self.new_start, self.new_len)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum FileDiff {
    /// No hunks: no changes on that side.
    Text(Vec<Hunk>),
    Binary,
    /// An external diff program is configured for the file (`diff=…`).
    External,
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
        Mode::ToGit,
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
    let algorithm = match outcome.operation {
        Operation::InternalDiff { algorithm } => algorithm,
        Operation::SourceOrDestinationIsBinary => return Ok(FileDiff::Binary),
        Operation::ExternalCommand { .. } => return Ok(FileDiff::External),
    };
    // lines keep their endings, as git compares them: a file losing its
    // last newline changes its last line
    let input = blob::InternedInput::new(outcome.old.intern_source(), outcome.new.intern_source());
    let diff = blob::diff_with_slider_heuristics(algorithm, &input);
    let hunks =
        UnifiedDiff::new(&diff, &input, Collect(vec![]), ContextSize::symmetrical(3)).consume()?;
    Ok(FileDiff::Text(hunks))
}

struct Collect(Vec<Hunk>);

impl ConsumeHunk for Collect {
    type Out = Vec<Hunk>;

    fn consume_hunk(
        &mut self,
        h: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        let lines = lines
            .iter()
            .map(|(k, bytes)| {
                let text = String::from_utf8_lossy(bytes);
                Line {
                    kind: match k {
                        DiffLineKind::Context => LineKind::Context,
                        DiffLineKind::Add => LineKind::Add,
                        DiffLineKind::Remove => LineKind::Remove,
                    },
                    text: text.trim_end_matches(['\n', '\r']).to_string(),
                    no_newline: !bytes.ends_with(b"\n"),
                }
            })
            .collect();
        self.0.push(Hunk {
            old_start: h.before_hunk_start,
            old_len: h.before_hunk_len,
            new_start: h.after_hunk_start,
            new_len: h.after_hunk_len,
            lines,
        });
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.0
    }
}
