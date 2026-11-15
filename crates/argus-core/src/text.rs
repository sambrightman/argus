//! Text from outside (file and branch names, commit subjects, a forge's
//! answers, git's output) made safe to put on a terminal.

use std::borrow::Cow;

/// What stands in for a character that isn't shown.
const REPLACED: char = '\u{fffd}';

/// `s` without control characters (escape among them), each replaced:
/// what a name or a title can show of itself. Line breaks and tabs stay.
pub fn printable(s: &str) -> Cow<'_, str> {
    let bad = |c: char| c.is_control() && c != '\n' && c != '\t';
    match s.contains(bad) {
        false => Cow::Borrowed(s),
        true => Cow::Owned(
            s.chars()
                .map(|c| if bad(c) { REPLACED } else { c })
                .collect(),
        ),
    }
}

/// `s` if it's a web address fit to hand to a terminal or a browser:
/// `http://` or `https://`, with no control character or space in it.
pub fn web_url(s: &str) -> Option<&str> {
    let web = s.starts_with("https://") || s.starts_with("http://");
    (web && !s.contains(|c: char| c.is_control() || c.is_whitespace())).then_some(s)
}

/// A line of output for a terminal: the colours and links argus itself
/// writes are kept (`ESC [ … m`, and `ESC ] 8 ; ; address ESC \`), every
/// other escape sequence and control character is replaced, whatever text
/// it came in with. Line breaks and tabs stay.
pub fn terminal_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if let Some(n) = own_sequence(rest) {
            out.push_str(&rest[..n]);
            rest = &rest[n..];
            continue;
        }
        out.push(match c.is_control() && c != '\n' && c != '\t' {
            true => REPLACED,
            false => c,
        });
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The length of the colour or link sequence `s` starts with, if it does.
fn own_sequence(s: &str) -> Option<usize> {
    if let Some(sgr) = s.strip_prefix("\x1b[") {
        let digits = sgr
            .find(|c: char| !c.is_ascii_digit() && c != ';')
            .unwrap_or(sgr.len());
        let after = sgr[digits..].strip_prefix('m')?;
        return Some(s.len() - after.len());
    }
    let link = s.strip_prefix("\x1b]8;;")?;
    let end = link.find(|c: char| c.is_control())?;
    let after = link[end..].strip_prefix("\x1b\\")?;
    Some(s.len() - after.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_replaces_control_characters_and_nothing_else() {
        assert_eq!(printable("plain ünïcode ✓"), "plain ünïcode ✓");
        assert!(matches!(printable("plain"), Cow::Borrowed(_)));
        assert_eq!(
            printable("a\x1b[2Jb\x07c\u{9b}d\x7f"),
            "a\u{fffd}[2Jb\u{fffd}c\u{fffd}d\u{fffd}"
        );
        // line breaks and tabs are text
        assert_eq!(printable("one\n\ttwo"), "one\n\ttwo");
        assert_eq!(printable("a\rb"), "a\u{fffd}b");
    }

    #[test]
    fn a_web_address_is_http_with_nothing_odd_in_it() {
        assert_eq!(
            web_url("https://github.com/a/b/pull/1"),
            Some("https://github.com/a/b/pull/1")
        );
        assert_eq!(web_url("http://ghe.corp/x"), Some("http://ghe.corp/x"));
        for bad in [
            "file:///Applications/Calculator.app",
            "--help",
            "javascript:alert(1)",
            "https://x/\x1b\\",
            "https://x/\x07",
            "https://x/a b",
            "https://x/a\nb",
            "",
        ] {
            assert_eq!(web_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn terminal_output_keeps_colours_and_links_and_nothing_else() {
        let ok = "a \x1b[1;31mred\x1b[0m \x1b]8;;https://x/1\x1b\\#1\x1b]8;;\x1b\\ z\n\tnext";
        assert_eq!(terminal_safe(ok), ok);
        let r = '\u{fffd}';
        for (bad, want) in [
            // what follows a kept sequence is still looked at, however long
            // the sequence
            ("\x1b[m\x07", format!("\x1b[m{r}")),
            ("\x1b[1;4;31m\x07", format!("\x1b[1;4;31m{r}")),
            ("\x1b]8;;\x1b\\\x07", format!("\x1b]8;;\x1b\\{r}")),
            (
                "\x1b]8;;https://x/123\x1b\\\x07",
                format!("\x1b]8;;https://x/123\x1b\\{r}"),
            ),
            // clear screen, cursor moves, the title, the clipboard, a bell
            ("x\x1b[2Jy", format!("x{r}[2Jy")),
            ("x\x1b[10;10Hy", format!("x{r}[10;10Hy")),
            ("\x1b]0;title\x07", format!("{r}]0;title{r}")),
            ("\x1b]52;c;AAAA\x1b\\", format!("{r}]52;c;AAAA{r}\\")),
            ("a\rb\x07", format!("a{r}b{r}")),
            // a link sequence that doesn't end properly isn't one
            (
                "\x1b]8;;https://x/\x07oops",
                format!("{r}]8;;https://x/{r}oops"),
            ),
            ("\x1b]8;;https://x", format!("{r}]8;;https://x")),
            // nor a colour sequence that isn't one
            ("\x1b[31", format!("{r}[31")),
            ("\x1b[?25l", format!("{r}[?25l")),
            ("\x1b", format!("{r}")),
        ] {
            assert_eq!(terminal_safe(bad), want, "{bad:?}");
        }
    }
}
