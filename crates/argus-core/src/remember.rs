//! Answers worked out for a repo that its commits alone decide, so they
//! hold until those commits change: whether a branch is squash-merged
//! (absorbed into the default branch), how many of a tip's commits are
//! on no remote (or no ref), and how many of one commit's are not
//! another's (a branch ahead of and behind its upstream, or merged).
//!
//! A repo's answers are those its last status asked for: a branch moved
//! or gone, or a remote branch moved, leaves its old answers behind, so
//! they never outgrow its branches and need no cap. They're kept in
//! memory and, once [`keep_in`] names a folder, in a file of the repo's
//! own there, read when the repo is first statused (nothing at start) and
//! written, whole, only when a status asks for other answers than it has.
//!
//! The file: a first line naming the repo, then a line an answer:
//!
//! ```text
//! # /home/me/dev/argus/.git
//! absorbed <branch tip> <default branch tip> <1|0>
//! unpushed <tip> <the others' fingerprint> <count>
//! between <commit> <other commit> <count>
//! ```
//!
//! Other lines are passed over, so one an older or newer argus wrote is
//! read for what it can be.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use gix::ObjectId;

/// Of two commits, the first and what it's compared with: a branch's tip
/// and the default branch's ([`absorbed`](Remembered::absorbed)), or a tip
/// and the [`fingerprint`] of the tips it's counted against.
type Pair = (ObjectId, ObjectId);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Answers {
    absorbed: HashMap<Pair, bool>,
    unpushed: HashMap<Pair, usize>,
    between: HashMap<Pair, usize>,
}

/// One repo's answers during a status: those it had, and those asked for.
pub struct Remembered {
    /// The repo's common dir (its worktrees share the answers; reached by
    /// another path, through a link, it has a file of its own).
    repo: PathBuf,
    had: Answers,
    asked: Answers,
}

/// Each repo's answers, by its common dir, as its last status left them.
fn known() -> &'static Mutex<HashMap<PathBuf, Answers>> {
    static KNOWN: OnceLock<Mutex<HashMap<PathBuf, Answers>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// The folder [`keep_in`] named.
fn kept_in() -> &'static Mutex<Option<PathBuf>> {
    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    &DIR
}

/// Keep each repo's answers in `dir` from now on, a file for each, so
/// later runs needn't work them out again.
pub fn keep_in(dir: &Path) {
    *kept_in().lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
}

/// Forget the answers held in memory, as a new process starts without
/// them (for tests).
#[doc(hidden)]
pub fn forget() {
    if let Ok(mut known) = known().lock() {
        known.clear();
    }
}

/// The set of tips `sorted` as one id, the same in every run: an unpushed
/// count's is part of its key.
pub(crate) fn fingerprint(sorted: &[ObjectId]) -> ObjectId {
    let bytes: Vec<u8> = sorted
        .iter()
        .flat_map(|id| id.as_bytes().to_vec())
        .collect();
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes)
        .unwrap_or_else(|_| ObjectId::null(gix::hash::Kind::Sha1))
}

impl Remembered {
    /// `repo`'s answers: those in memory, or else in its file.
    pub fn of(repo: &gix::Repository) -> Self {
        let path = repo.common_dir().to_path_buf();
        let had = known().lock().ok().and_then(|k| k.get(&path).cloned());
        let had = had.unwrap_or_else(|| match file_of(&path) {
            Some(file) => read(&file),
            None => Answers::default(),
        });
        Remembered {
            repo: path,
            had,
            asked: Answers::default(),
        }
    }

    /// Whether `tip` is absorbed into `default`, as remembered, or else as
    /// `work_out` finds (`None`, when it can't: no answer to keep).
    pub(crate) fn absorbed(
        &mut self,
        tip: ObjectId,
        default: ObjectId,
        work_out: impl FnOnce() -> Option<bool>,
    ) -> Option<bool> {
        let pair = (tip, default);
        let answer = self.had.absorbed.get(&pair).copied().or_else(work_out)?;
        self.asked.absorbed.insert(pair, answer);
        Some(answer)
    }

    /// The count remembered for `tip` against the tips fingerprinted `of`.
    pub(crate) fn unpushed(&mut self, tip: ObjectId, of: ObjectId) -> Option<usize> {
        let n = self.had.unpushed.get(&(tip, of)).copied()?;
        self.asked.unpushed.insert((tip, of), n);
        Some(n)
    }

    /// The count worked out for `tip` against the tips fingerprinted `of`.
    pub(crate) fn counted(&mut self, tip: ObjectId, of: ObjectId, n: usize) {
        self.asked.unpushed.insert((tip, of), n);
    }

    /// How many commits `a` has that `b` hasn't, as remembered, or else
    /// as `work_out` counts (an error is no answer to keep).
    pub(crate) fn between<E>(
        &mut self,
        a: ObjectId,
        b: ObjectId,
        work_out: impl FnOnce() -> Result<usize, E>,
    ) -> Result<usize, E> {
        let n = match self.had.between.get(&(a, b)) {
            Some(&n) => n,
            None => work_out()?,
        };
        self.asked.between.insert((a, b), n);
        Ok(n)
    }

    /// Keep what this status asked for, if that's not what was kept.
    pub fn keep(self) {
        if self.asked == self.had {
            return;
        }
        if let Some(file) = file_of(&self.repo) {
            // (one that can't be written is worked out again next run)
            let _ = crate::paths::write_atomically(&file, &format(&self.repo, &self.asked));
        }
        if let Ok(mut known) = known().lock() {
            known.insert(self.repo, self.asked);
        }
    }
}

/// The repo's file, in the folder [`keep_in`] named, named for its path.
fn file_of(repo: &Path) -> Option<PathBuf> {
    let dir = kept_in().lock().ok()?.clone()?;
    let id = gix::objs::compute_hash(
        gix::hash::Kind::Sha1,
        gix::objs::Kind::Blob,
        repo.as_os_str().as_encoded_bytes(),
    )
    .ok()?;
    Some(dir.join(id.to_string()))
}

fn format(repo: &Path, answers: &Answers) -> String {
    let mut lines: Vec<String> = answers
        .absorbed
        .iter()
        .map(|((t, d), a)| format!("absorbed {t} {d} {}\n", u8::from(*a)))
        .chain(
            answers
                .unpushed
                .iter()
                .map(|((t, of), n)| format!("unpushed {t} {of} {n}\n")),
        )
        .chain(
            answers
                .between
                .iter()
                .map(|((a, b), n)| format!("between {a} {b} {n}\n")),
        )
        .collect();
    lines.sort();
    format!("# {}\n{}", repo.display(), lines.concat())
}

/// The answers in `file`; a file that can't be read holds none.
fn read(file: &Path) -> Answers {
    let mut answers = Answers::default();
    let Ok(text) = std::fs::read_to_string(file) else {
        return answers;
    };
    for line in text.lines() {
        let mut words = line.split(' ');
        let (Some(kind), Some(a), Some(b), Some(answer), None) = (
            words.next(),
            words.next(),
            words.next(),
            words.next(),
            words.next(),
        ) else {
            continue;
        };
        let (Ok(a), Ok(b)) = (
            ObjectId::from_hex(a.as_bytes()),
            ObjectId::from_hex(b.as_bytes()),
        ) else {
            continue;
        };
        match (kind, answer) {
            ("absorbed", "1") => _ = answers.absorbed.insert((a, b), true),
            ("absorbed", "0") => _ = answers.absorbed.insert((a, b), false),
            ("unpushed", n) => {
                if let Ok(n) = n.parse() {
                    answers.unpushed.insert((a, b), n);
                }
            }
            ("between", n) => {
                if let Ok(n) = n.parse() {
                    answers.between.insert((a, b), n);
                }
            }
            _ => {}
        }
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes_or_panic(&[n; 20])
    }

    /// A file's answers, a line each, others passed over (its first,
    /// saying the repo, and any not quite an answer); no file, or one that
    /// can't be read, holds none. Written back, it says the same.
    #[test]
    fn answers_are_read_a_line_each_passing_over_others() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("f");
        let (a, b) = (id(1), id(2));
        let text = format!(
            "# /some/repo\nabsorbed {a} {b} 1\nabsorbed {b} {a} 0\nunpushed {a} {b} 12\n\
             junk\nabsorbed {a} {a} 2\nabsorbed {b} {b} 1 more\nabsorbed ab cd 1\n\
             unpushed {b} {b} -1\nbranch {a} {b} 1\nbetween {b} {a} 7\nbetween {a} {a} x\n"
        );
        std::fs::write(&file, text).unwrap();
        let got = read(&file);
        assert_eq!(
            got,
            Answers {
                absorbed: HashMap::from([((a, b), true), ((b, a), false)]),
                unpushed: HashMap::from([((a, b), 12)]),
                between: HashMap::from([((b, a), 7)]),
            }
        );
        assert_eq!(
            format(Path::new("/some/repo"), &got),
            format!(
                "# /some/repo\nabsorbed {a} {b} 1\nabsorbed {b} {a} 0\nbetween {b} {a} 7\nunpushed {a} {b} 12\n"
            )
        );
        assert_eq!(read(&d.path().join("none")), Answers::default());
        assert_eq!(read(d.path()), Answers::default());
    }

    /// A set's fingerprint is its own, whatever the run: here, the same
    /// as computed independently, and another set's differs.
    #[test]
    fn a_fingerprint_is_the_sets_alone() {
        let one = fingerprint(&[id(1), id(2)]);
        assert_eq!(one, fingerprint(&[id(1), id(2)]));
        assert_ne!(one, fingerprint(&[id(1), id(3)]));
        assert_ne!(one, fingerprint(&[id(1)]));
        let bytes = [[1u8; 20], [2u8; 20]].concat();
        let blob = gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &bytes);
        assert_eq!(one, blob.unwrap());
    }
}
