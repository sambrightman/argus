//! Highlighting the changed words in a hunk, as magit's "refine hunk":
//! each run of removed lines and the run of added lines after it are split
//! into words and diffed (with `similar`); words that differ are marked.

use argus_core::diff::LineKind;
use similar::{ChangeTag, TextDiff};

/// A line's text in segments, `true` where it changed.
pub type Segments = Vec<(bool, String)>;

/// Runs longer than this many lines aren't refined.
const MAX_RUN: usize = 200;

/// Segments for each of `lines` (kind and text, as in a hunk): removed and
/// added lines that pair up get their changed words marked; everything
/// else is one unmarked segment.
pub fn refine(lines: &[(LineKind, &str)]) -> Vec<Segments> {
    let mut out: Vec<Segments> = lines
        .iter()
        .map(|(_, t)| vec![(false, t.to_string())])
        .collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].0 != LineKind::Remove {
            i += 1;
            continue;
        }
        let removed = i..(i..lines.len())
            .find(|&j| lines[j].0 != LineKind::Remove)
            .unwrap_or(lines.len());
        let end = (removed.end..lines.len())
            .find(|&j| lines[j].0 != LineKind::Add)
            .unwrap_or(lines.len());
        let added = removed.end..end;
        i = end;
        if added.is_empty() || removed.len() > MAX_RUN || added.len() > MAX_RUN {
            continue;
        }
        let join = |r: &std::ops::Range<usize>| {
            lines[r.clone()]
                .iter()
                .map(|(_, t)| *t)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (old, new) = (join(&removed), join(&added));
        let diff = TextDiff::from_words(old.as_str(), new.as_str());
        let mut old_lines = Lines::new(removed.len());
        let mut new_lines = Lines::new(added.len());
        for c in diff.iter_all_changes() {
            let changed = c.tag() != ChangeTag::Equal;
            if c.tag() != ChangeTag::Insert {
                old_lines.push(changed, c.value());
            }
            if c.tag() != ChangeTag::Delete {
                new_lines.push(changed, c.value());
            }
        }
        for (k, segs) in removed.zip(old_lines.0) {
            out[k] = segs;
        }
        for (k, segs) in added.zip(new_lines.0) {
            out[k] = segs;
        }
    }
    out
}

/// Segments being collected for consecutive lines; `\n` moves to the next.
struct Lines(Vec<Segments>, usize);

impl Lines {
    fn new(n: usize) -> Self {
        Lines(vec![vec![]; n], 0)
    }

    fn push(&mut self, changed: bool, text: &str) {
        for (k, piece) in text.split('\n').enumerate() {
            if k > 0 {
                self.1 += 1;
            }
            if piece.is_empty() {
                continue;
            }
            let Some(line) = self.0.get_mut(self.1) else {
                return;
            };
            match line.last_mut() {
                Some((c, t)) if *c == changed => t.push_str(piece),
                _ => line.push((changed, piece.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LineKind::*;

    fn marked(segs: &Segments) -> Vec<&str> {
        segs.iter().filter(|s| s.0).map(|s| s.1.as_str()).collect()
    }

    fn text(segs: &Segments) -> String {
        segs.iter().map(|s| s.1.as_str()).collect()
    }

    #[test]
    fn changed_words_in_paired_runs_are_marked() {
        let lines = [
            (Context, "same"),
            (Remove, "hello world"),
            (Remove, "keep this"),
            (Add, "hello there"),
            (Add, "keep that"),
            (Context, "end"),
        ];
        let r = refine(&lines);
        assert_eq!(marked(&r[1]), ["world"]);
        assert_eq!(marked(&r[3]), ["there"]);
        assert_eq!(marked(&r[2]), ["this"]);
        assert_eq!(marked(&r[4]), ["that"]);
        // text is kept whole
        for (k, (_, t)) in lines.iter().enumerate() {
            assert_eq!(text(&r[k]), *t, "line {k}");
        }
        // inverse: context lines, and unpaired runs, are unmarked
        assert!(marked(&r[0]).is_empty() && marked(&r[5]).is_empty());
        let r = refine(&[(Remove, "gone"), (Context, "x"), (Add, "new")]);
        assert!(r.iter().all(|s| marked(s).is_empty()));
    }

    #[test]
    fn long_runs_are_left_unrefined() {
        let changed = |n: usize, m: usize| {
            let old: Vec<String> = (0..n).map(|i| format!("a {i}")).collect();
            let new: Vec<String> = (0..m).map(|i| format!("b {i}")).collect();
            let lines: Vec<(LineKind, &str)> = old
                .iter()
                .map(|l| (Remove, l.as_str()))
                .chain(new.iter().map(|l| (Add, l.as_str())))
                .collect();
            refine(&lines).iter().any(|s| !marked(s).is_empty())
        };
        // up to MAX_RUN lines on either side: refined
        assert!(changed(MAX_RUN, 1) && changed(1, MAX_RUN) && changed(MAX_RUN, MAX_RUN));
        // one more on either side: not
        assert!(!changed(MAX_RUN + 1, 1));
        assert!(!changed(1, MAX_RUN + 1));
    }

    #[test]
    fn segments_alternate() {
        let r = refine(&[(Remove, "one two three four"), (Add, "one 2 3 four")]);
        for segs in &r {
            assert!(segs.windows(2).all(|w| w[0].0 != w[1].0), "{segs:?}");
        }
        // the unchanged space between them keeps two changed words apart
        assert_eq!(marked(&r[1]), ["2", "3"]);
        // consecutive changed words (and spaces) form one segment
        let r = refine(&[(Remove, "keep one two"), (Add, "keep")]);
        assert_eq!(marked(&r[0]), [" one two"]);
    }

    #[test]
    fn uneven_runs_and_blank_lines() {
        let r = refine(&[(Remove, "a b"), (Add, "a c"), (Add, ""), (Add, "d")]);
        assert_eq!(marked(&r[0]), ["b"]);
        assert_eq!(text(&r[1]), "a c");
        assert_eq!(text(&r[2]), "");
        assert_eq!(text(&r[3]), "d");
        assert_eq!(marked(&r[3]), ["d"]);
    }
}
