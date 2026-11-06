//! Non-ASCII characters the interface prints besides icons (those are in
//! [`crate::badges`]), and alternatives for `[ui.icon_overrides]`. `argus
//! demo --glyphs` shows them all; a test checks this list against the
//! sources, so a new glyph has to be added here.

/// Each glyph, and where it's used.
pub const UI_GLYPHS: &[(char, &str)] = &[
    ('…', "working…, truncated text, more items"),
    ('·', "separators in headers and messages"),
    ('—', "shell header"),
    ('↑', "ahead in branch details, key hints"),
    ('↓', "behind in branch details, key hints"),
    ('✓', "fix succeeded, job done"),
    ('✗', "fix failed, job failed"),
    ('▌', "marked row"),
    ('🎉', "nothing needs attention"),
    ('⠋', "scanning spinner"),
    ('⠙', "scanning spinner"),
    ('⠹', "scanning spinner"),
    ('⠸', "scanning spinner"),
    ('⠼', "scanning spinner"),
    ('⠴', "scanning spinner"),
    ('⠦', "scanning spinner"),
    ('⠧', "scanning spinner"),
    ('⠇', "scanning spinner"),
    ('⠏', "scanning spinner"),
];

/// Glyphs worth trying in `[ui.icon_overrides]`, from best to least
/// supported by common monospace fonts.
pub const ALTERNATIVES: &str = "◊○●•∙…‹›«»§¶±≠×÷∞◆✓■□▲▼▶◀◇◉⌂⌘⏎♦✗‼⋯↯⇅⇡⇣✔✘⚡★☆✦✧✱⚑⚠⎇";
