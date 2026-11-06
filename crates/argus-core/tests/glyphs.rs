//! Every non-ASCII character the interface prints is listed, so `argus
//! demo --glyphs` can show it: icons in the icon sets, the rest in
//! `glyphs::UI_GLYPHS`. Checked against the string and char literals in
//! every crate's sources, outside tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use argus_core::badges::{self, ICON_NAMES};
use argus_core::glyphs::{ALTERNATIVES, UI_GLYPHS};

/// `src` with `#[cfg(test)]` items (the brace block that follows) removed.
fn without_tests(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(open) = after.find('{') else {
            return out;
        };
        let mut depth = 0;
        let mut end = after.len();
        for (i, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The contents of string and char literals, skipping comments.
fn literals(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ('"', _) => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                out.push(s);
                i += 1;
            }
            // a char literal, not a lifetime: 'x' or '\x'
            ('\'', Some(c)) if chars.get(i + 2) == Some(&'\'') && c != '\\' => {
                out.push(c.to_string());
                i += 3;
            }
            _ => i += 1,
        }
    }
    out
}

fn sources() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![];
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&crates)
        .unwrap()
        .map(|e| e.unwrap().path().join("src"))
        .filter(|p| p.is_dir())
        .collect();
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(files.len() > 20, "sources found: {files:?}");
    files
}

/// Non-ASCII characters in literals, and the files using each; the lists
/// themselves (glyphs.rs) are left out.
fn used() -> BTreeMap<char, BTreeSet<String>> {
    let mut used: BTreeMap<char, BTreeSet<String>> = BTreeMap::new();
    for f in sources() {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if name == "glyphs.rs" {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        for lit in literals(&without_tests(&src)) {
            for c in lit.chars().filter(|c| !c.is_ascii()) {
                used.entry(c).or_default().insert(name.clone());
            }
        }
    }
    used
}

fn icon_chars() -> BTreeSet<char> {
    badges::SETS
        .iter()
        .flat_map(|&s| {
            let set = badges::by_name(s);
            ICON_NAMES
                .iter()
                .flat_map(|n| set.get(n).unwrap().chars().collect::<Vec<_>>())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn every_glyph_the_interface_prints_is_listed() {
    let listed: BTreeSet<char> = UI_GLYPHS
        .iter()
        .map(|(c, _)| *c)
        .chain(icon_chars())
        .chain(ALTERNATIVES.chars())
        .collect();
    let unlisted: Vec<String> = used()
        .into_iter()
        .filter(|(c, _)| !listed.contains(c))
        .map(|(c, files)| format!("{c} U+{:04X} in {files:?}", c as u32))
        .collect();
    assert!(
        unlisted.is_empty(),
        "add to glyphs::UI_GLYPHS: {unlisted:#?}"
    );
}

#[test]
fn listed_glyphs_are_used_and_unique() {
    let used = used();
    let icons = icon_chars();
    let mut seen = BTreeSet::new();
    for (c, what) in UI_GLYPHS {
        assert!(seen.insert(c), "{c} listed twice");
        assert!(!what.is_empty(), "{c}: say where it's used");
        assert!(
            used.contains_key(c) || icons.contains(c),
            "{c} U+{:04X} ({what}) isn't used: remove it",
            *c as u32
        );
    }
}

#[test]
fn the_scanner_finds_literals_but_not_comments_or_tests() {
    let src = r#"
        // a comment: ★
        /* block ◆ */
        fn f<'a>(x: &'a str) -> char { let _ = "a·b\"…"; '✓' }
        #[cfg(test)]
        mod tests { fn t() { let _ = "ñ"; } }
        fn g() { let _ = "↑"; }
    "#;
    let found: String = literals(&without_tests(src)).concat();
    assert_eq!(found, "a·b\"…✓↑");
}
