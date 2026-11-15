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
    // (the `url` crate reads as a browser does, past tabs and line breaks
    // and with the slashes left out: here an address is written in full)
    let written = s.starts_with("https://") || s.starts_with("http://");
    if !written || s.contains(|c: char| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let url = url::Url::parse(s).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| !h.is_empty()))
        .then_some(s)
}

/// A line of output for a terminal: the colours and links argus itself
/// writes are kept (`ESC [ … m`, and `ESC ] 8 ; ; address ESC \`), every
/// other escape sequence and control character is replaced, whatever text
/// it came in with. Line breaks and tabs stay.
pub fn terminal_safe(s: &str) -> String {
    let mut kept = Kept {
        out: String::with_capacity(s.len()),
        linked: false,
    };
    // read as a terminal reads it (`vte`, the parser of terminals written
    // in Rust), so what's kept or not is what a terminal would act on
    vte::Parser::new().advance(&mut kept, s.as_bytes());
    // (a link left open would take in all that's written after it)
    if kept.linked {
        kept.out.push_str(LINK);
        kept.out.push_str(LINK_END);
    }
    kept.out
}

/// How a link sequence starts, before its address, and ends after it.
const LINK: &str = "\x1b]8;;";
const LINK_END: &str = "\x1b\\";

/// The colour and style codes argus writes (and a theme may ask for):
/// reset, bold, dim, italic, underline, reverse and their ends, and the
/// sixteen foreground colours. Not concealed text, nor backgrounds.
fn own_style(code: u16) -> bool {
    matches!(code, 0..=4 | 7 | 22..=24 | 27 | 30..=37 | 39 | 90..=97)
}

/// What [`terminal_safe`] keeps of what a terminal would be told.
struct Kept {
    out: String,
    /// A link has been started and not ended.
    linked: bool,
}

impl vte::Perform for Kept {
    fn print(&mut self, c: char) {
        // (the controls above ASCII come as characters)
        self.out.push(if c.is_control() { REPLACED } else { c });
    }

    fn execute(&mut self, byte: u8) {
        self.out.push(match byte {
            b'\n' => '\n',
            b'\t' => '\t',
            _ => REPLACED,
        });
    }

    fn csi_dispatch(&mut self, params: &vte::Params, between: &[u8], ignore: bool, action: char) {
        let plain = |p: &[u16]| matches!(p, [code] if own_style(*code));
        if action != 'm' || ignore || !between.is_empty() || !params.iter().all(plain) {
            return self.out.push(REPLACED);
        }
        let codes: Vec<String> = params.iter().flatten().map(u16::to_string).collect();
        self.out.push_str(&format!("\x1b[{}m", codes.join(";")));
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        // `8 ; ; address`, ended as argus ends it: a link to a web address,
        // or (no address) a link's end. An address's own `;` split it.
        let address = match params {
            [b"8", b"", address @ ..] if !bell_terminated => {
                String::from_utf8(address.join(&b';')).ok()
            }
            _ => None,
        };
        match address.filter(|a| a.is_empty() || web_url(a).is_some()) {
            Some(a) => {
                self.linked = !a.is_empty();
                self.out.push_str(&format!("{LINK}{a}{LINK_END}"));
            }
            None => self.out.push(REPLACED),
        }
    }

    fn esc_dispatch(&mut self, between: &[u8], _ignore: bool, byte: u8) {
        // (the `ESC \` that ends a link or another string is nothing itself)
        if byte != b'\\' || !between.is_empty() {
            self.out.push(REPLACED);
        }
    }

    fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {
        self.out.push(REPLACED);
    }
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
            "https://",
            "http://[::1",
            "https://exa mple.com/",
            "HTTPS:nothing",
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
            ("\x1b[m\x07", format!("\x1b[0m{r}")),
            ("\x1b[1;4;31m\x07", format!("\x1b[1;4;31m{r}")),
            ("\x1b]8;;\x1b\\\x07", format!("\x1b]8;;\x1b\\{r}")),
            (
                "\x1b]8;;https://x/123\x1b\\\x07",
                format!("\x1b]8;;https://x/123\x1b\\{r}\x1b]8;;\x1b\\"),
            ),
            // an address with `;` in it is one address
            (
                "\x1b]8;;https://x/a;b?c=d;e\x1b\\t\x1b]8;;\x1b\\",
                "\x1b]8;;https://x/a;b?c=d;e\x1b\\t\x1b]8;;\x1b\\".to_string(),
            ),
            // clear screen, cursor moves, the title, the clipboard, a bell:
            // each gone, with a mark where it was
            ("x\x1b[2Jy", format!("x{r}y")),
            ("x\x1b[10;10Hy", format!("x{r}y")),
            ("\x1b]0;title\x07", format!("{r}")),
            ("\x1b]52;c;AAAA\x1b\\z", format!("{r}z")),
            ("a\rb\x07", format!("a{r}b{r}")),
            ("a\u{9b}2Jb\u{85}", format!("a{r}2Jb{r}")),
            ("\x1bcx", format!("{r}x")),
            ("\x1bP1$r\x1b\\x", format!("{r}x")),
            // a link ended with a bell isn't argus's, nor one with more
            // said of it, nor one to anything but a web address
            ("\x1b]8;;https://x/\x07oops", format!("{r}oops")),
            ("\x1b]8;id=1;https://x/\x1b\\x", format!("{r}x")),
            ("\x1b]8;;file:///etc/passwd\x1b\\x", format!("{r}x")),
            ("\x1b]8;;javascript:alert(1)\x1b\\x", format!("{r}x")),
            // one never ended is ended, so it takes in nothing after it
            (
                "\x1b]8;;https://x/\x1b\\rest",
                "\x1b]8;;https://x/\x1b\\rest\x1b]8;;\x1b\\".to_string(),
            ),
            // styles that hide text, or aren't ones argus writes
            ("\x1b[8mhidden", format!("{r}hidden")),
            ("\x1b[30;40mx", format!("{r}x")),
            ("\x1b[38;2;0;0;0mx", format!("{r}x")),
            ("\x1b[4:3mx", format!("{r}x")),
            ("\x1b[?25l", format!("{r}")),
            ("\x1b[1 qx", format!("{r}x")),
            // a sequence the text ends in the middle of is nothing at all,
            // as a terminal would have it
            ("a\x1b]8;;https://x", "a".to_string()),
            ("a\x1b[31", "a".to_string()),
            ("a\x1b", "a".to_string()),
        ] {
            assert_eq!(terminal_safe(bad), want, "{bad:?}");
        }
    }
}
