//! Glyphs come from `icons::Icons`, so every set (and `[ui.icon_overrides]`)
//! can replace them and `argus demo --glyphs` shows them. This guards
//! against symbols typed straight into the interface: string and char
//! literals in every crate's sources, outside tests, may only use ASCII
//! and `icons::PUNCTUATION`.

use std::path::{Path, PathBuf};

use argus_core::icons::{ALTERNATIVES, PUNCTUATION};

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

/// Files that may use more, and what: the icon sets themselves, and the
/// `[ui.icon_overrides]` example in the config schema.
fn extra_allowed(file: &str, c: char) -> bool {
    match file {
        "icons.rs" => true,
        "config.rs" => ALTERNATIVES.contains(c),
        _ => false,
    }
}

#[test]
fn symbols_come_from_icons() {
    let mut found = vec![];
    for f in sources() {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&f).unwrap();
        for lit in literals(&without_tests(&src)) {
            for c in lit.chars() {
                if !c.is_ascii() && !PUNCTUATION.contains(c) && !extra_allowed(&name, c) {
                    found.push(format!("{c} U+{:04X} in {name}: {lit:?}", c as u32));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "use an icon (icons::Icons) for these, or add plain punctuation \
         to icons::PUNCTUATION: {found:#?}"
    );
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
