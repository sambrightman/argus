//! Matching as you type, fuzzily: the query's letters in order, not
//! necessarily together (nucleo, Helix's matcher). Lower case matches
//! either case, upper case only itself; fzf's operators work (`^start`,
//! `end$`, `'exact`, `!not`).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Fuzzy {
    pattern: Pattern,
    matcher: Matcher,
    buf: Vec<char>,
}

impl Fuzzy {
    pub fn new(query: &str) -> Self {
        Self {
            pattern: Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
            matcher: Matcher::new(Config::DEFAULT),
            buf: vec![],
        }
    }

    /// How well `text` matches (higher is better); `None` when it doesn't.
    /// An empty query matches everything equally.
    pub fn score(&mut self, text: &str) -> Option<u32> {
        self.pattern
            .score(Utf32Str::new(text, &mut self.buf), &mut self.matcher)
    }

    /// The indices of the `texts` that match, best first; ties keep their
    /// order.
    pub fn rank<'t>(&mut self, texts: impl IntoIterator<Item = &'t str>) -> Vec<usize> {
        let mut scored: Vec<(usize, u32)> = texts
            .into_iter()
            .enumerate()
            .filter_map(|(i, t)| Some((i, self.score(t)?)))
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(i, _)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_in_order_match_best_first() {
        let names = ["main", "feature/login-page", "topic", "fix-log"];
        assert_eq!(Fuzzy::new("tpc").rank(names), [2]);
        assert_eq!(
            Fuzzy::new("mn").rank(["main", "mn-tools"]),
            [1, 0],
            "together first"
        );
        assert_eq!(
            Fuzzy::new("").rank(names),
            [0, 1, 2, 3],
            "everything, in order"
        );
        assert_eq!(Fuzzy::new("zzz").rank(names), Vec::<usize>::new());
        // smart case: upper case only matches itself
        assert_eq!(Fuzzy::new("Main").rank(names), Vec::<usize>::new());
        assert!(Fuzzy::new("MAIN").score("MAIN").is_some());
    }
}
