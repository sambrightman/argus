//! Text from outside (file and branch names, commit subjects, a forge's
//! answers, git's output) made safe to put on a terminal.

use std::borrow::Cow;

/// What stands in for a character that isn't shown.
const REPLACED: char = '\u{fffd}';

/// `s` without control characters (escape among them), each replaced:
/// what a name or a title can show of itself. Line breaks and tabs stay.
pub fn printable(s: &str) -> Cow<'_, str> {
    let bad = |c: char| unshown(c) && c != '\n' && c != '\t';
    match s.contains(bad) {
        false => Cow::Borrowed(s),
        true => Cow::Owned(
            s.chars()
                .map(|c| if bad(c) { REPLACED } else { c })
                .collect(),
        ),
    }
}

/// Whether `c` does something to the text around it instead of showing:
/// a control character, a line or paragraph separator, or one of those
/// that turn the direction of what follows (which make text read as
/// other than it is).
fn unshown(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `s` if it's a web address fit to hand to a terminal or a browser:
/// `http://` or `https://`, with no control character or space in it.
pub fn web_url(s: &str) -> Option<&str> {
    // (the `url` crate reads as a browser does, past tabs and line breaks
    // and with the slashes left out: here an address is written in full)
    let written = s.starts_with("https://") || s.starts_with("http://");
    if !written || s.contains(|c: char| unshown(c) || c.is_whitespace()) {
        return None;
    }
    // (http or https, as written; and one the crate takes for an address)
    url::Url::parse(s).ok().map(|_| s)
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

/// The most parts of an operating system command `vte` keeps.
const OSC_PARTS: usize = 16;

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
        self.out.push(if unshown(c) { REPLACED } else { c });
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
        // (as many parts as the parser keeps: there may have been more,
        // and the address would be short of them)
        let whole = params.len() < OSC_PARTS;
        let address = match params {
            [b"8", b"", address @ ..] if !bell_terminated && whole => {
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
        // nor what turns the text around it
        assert_eq!(printable("evil\u{202e}txt.exe"), "evil\u{fffd}txt.exe");
        assert_eq!(
            printable("שלום \u{200f}ok"),
            "שלום \u{200f}ok",
            "a mark, not an override"
        );
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
            "https://x/\u{202e}gpj.exe",
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
            // text that turns the direction of what follows, or breaks the
            // line where no line break is
            ("a\u{202e}b\u{2066}c\u{2028}d", format!("a{r}b{r}c{r}d")),
            // an address in more parts than are kept isn't a link: it
            // would be to somewhere else
            (
                "\x1b]8;;https://x/0;1;2;3;4;5;6;7;8;9;10;11;12;13;14;15\x1b\\t",
                format!("{r}t"),
            ),
            (
                "\x1b]8;;https://x/0;1;2;3;4;5;6;7;8;9;10;11;12\x1b\\t\x1b]8;;\x1b\\",
                "\x1b]8;;https://x/0;1;2;3;4;5;6;7;8;9;10;11;12\x1b\\t\x1b]8;;\x1b\\".to_string(),
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
            // a colour sequence with something between its codes and its
            // end, or with more codes than are kept, isn't argus's either
            ("\x1b[1 mx", format!("{r}x")),
            (
                "\x1b[1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1;1mx",
                format!("{r}x"),
            ),
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
