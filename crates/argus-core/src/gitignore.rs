//! Adding ignore rules for untracked files: to the repo's top-level
//! `.gitignore`, the one in each file's directory, `.git/info/exclude`
//! (this clone only) or the global excludes file (every repo). Plain file
//! edits, not git operations; where the private and global files are is
//! asked of git.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which ignore file a rule goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IgnoreFile {
    /// `.gitignore` at the top of the work tree, shared through commits.
    TopLevel,
    /// `.gitignore` in each file's own directory, shared.
    Directory,
    /// `.git/info/exclude`: this clone only.
    Private,
    /// `core.excludesFile` (else `~/.config/git/ignore`): every repo.
    Global,
}

/// The rule matching `path` (repo-relative, a directory with a trailing
/// `/`) from the directory `base` (repo-relative, `""` for the top):
/// anchored, `/name` relative to `base`, matches only that path;
/// unanchored, the bare name, matches it in any directory below.
pub fn rule(path: &str, base: &str, anchored: bool) -> String {
    let dir = path.ends_with('/');
    let trimmed = path.trim_end_matches('/');
    let rel = match base {
        "" => trimmed,
        b => trimmed
            .strip_prefix(b.trim_end_matches('/'))
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(trimmed),
    };
    let name = if anchored {
        rel
    } else {
        rel.rsplit('/').next().unwrap_or(rel)
    };
    let mut r = String::new();
    if anchored {
        r.push('/');
    }
    r.push_str(&escape(name));
    if dir {
        r.push('/');
    }
    r
}

/// A name with gitignore's special characters taken literally: glob
/// characters and backslashes escaped, a leading `#` or `!` too, and
/// trailing spaces (which git would drop).
fn escape(name: &str) -> String {
    let mut s = String::new();
    for (i, c) in name.chars().enumerate() {
        if matches!(c, '*' | '?' | '[' | '\\') || (i == 0 && matches!(c, '#' | '!')) {
            s.push('\\');
        }
        s.push(c);
    }
    let kept = s.trim_end_matches(' ').len();
    let spaces = s.len() - kept;
    s.truncate(kept);
    s.push_str(&"\\ ".repeat(spaces));
    s
}

/// The directory `path` is in (repo-relative, `""` for the top).
pub fn dir_of(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or("", |(d, _)| d)
}

/// The rules adding `paths` to `file` would write, by the file each goes
/// into (absolute), in order.
pub fn plan(
    repo: &Path,
    file: IgnoreFile,
    paths: &[String],
    anchored: bool,
) -> Result<Vec<(PathBuf, String)>, String> {
    let shared = |dir: &str| repo.join(dir).join(".gitignore");
    let mut v = vec![];
    for p in paths {
        // a rule is one line
        if p.contains(['\n', '\r']) {
            return Err(format!(
                "{p:?} has a line break in its name: no rule can name it"
            ));
        }
        v.push(match file {
            IgnoreFile::TopLevel => (shared(""), rule(p, "", anchored)),
            IgnoreFile::Directory => (shared(dir_of(p)), rule(p, dir_of(p), anchored)),
            IgnoreFile::Private => (private_file(repo)?, rule(p, "", anchored)),
            IgnoreFile::Global => (global_file(repo)?, rule(p, "", anchored)),
        });
    }
    Ok(v)
}

/// `.git/info/exclude` as git finds it (the common one, for a linked
/// worktree).
fn private_file(repo: &Path) -> Result<PathBuf, String> {
    let o = crate::ops::git_in(repo, &["rev-parse", "--git-path", "info/exclude"]);
    if !o.success {
        return Err(o.output);
    }
    Ok(repo.join(o.output))
}

/// `core.excludesFile`, else git's default: `$XDG_CONFIG_HOME/git/ignore`
/// or `~/.config/git/ignore`.
fn global_file(repo: &Path) -> Result<PathBuf, String> {
    let o = crate::ops::git_in(repo, &["config", "--path", "--get", "core.excludesFile"]);
    excludes_file(o.success.then_some(o.output), |k| std::env::var_os(k))
}

/// [`global_file`], given `core.excludesFile` and the environment.
fn excludes_file(
    configured: Option<String>,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    if let Some(f) = configured.filter(|f| !f.is_empty()) {
        return Ok(PathBuf::from(f));
    }
    let config = env("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("no home directory for the global excludes file")?;
    Ok(config.join("git/ignore"))
}

/// Append each rule to its file (created, with its directories, if
/// missing), on a line of its own; a rule the file has already is left
/// out. Returns the rules added, by file.
pub fn add(
    repo: &Path,
    file: IgnoreFile,
    paths: &[String],
    anchored: bool,
) -> Result<Vec<(PathBuf, String)>, String> {
    let mut added = vec![];
    for (target, rule) in plan(repo, file, paths, anchored)? {
        // a shared file is the repo's content, which may not be the
        // user's own: one that's a link could lead anywhere
        let shared = matches!(file, IgnoreFile::TopLevel | IgnoreFile::Directory);
        if shared && target.is_symlink() {
            return Err(format!(
                "{} is a symbolic link: not written through",
                target.display()
            ));
        }
        // only a file that isn't there starts empty: one that can't be
        // read (not text, no permission) is never replaced
        let text = match std::fs::read_to_string(&target) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", target.display())),
        };
        if text.lines().any(|l| l == rule) {
            continue;
        }
        let mut out = text;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&rule);
        out.push('\n');
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, out).map_err(|e| format!("{}: {e}", target.display()))?;
        added.push((target, rule));
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_global_file_is_configured_else_where_git_looks() {
        let env = |xdg: &'static str| {
            move |k: &str| match k {
                "XDG_CONFIG_HOME" => Some(xdg.into()),
                "HOME" => Some("/home/me".into()),
                _ => None,
            }
        };
        let file = |configured: Option<&str>, xdg| {
            excludes_file(configured.map(String::from), env(xdg)).unwrap()
        };
        assert_eq!(file(Some("/x/ignore"), "/xdg"), Path::new("/x/ignore"));
        assert_eq!(file(Some(""), "/xdg"), Path::new("/xdg/git/ignore"));
        assert_eq!(file(None, "/xdg"), Path::new("/xdg/git/ignore"));
        assert_eq!(file(None, ""), Path::new("/home/me/.config/git/ignore"));
        assert!(excludes_file(None, |_| None).is_err());
    }

    #[test]
    fn rules_anchor_or_match_anywhere() {
        assert_eq!(rule("build/", "", true), "/build/");
        assert_eq!(rule("build/", "", false), "build/");
        assert_eq!(rule("a/b/out.log", "", true), "/a/b/out.log");
        // unanchored: the bare name, matching at any depth
        assert_eq!(rule("a/b/out.log", "", false), "out.log");
        // relative to the directory of its .gitignore
        assert_eq!(rule("a/b/out.log", "a/b", true), "/out.log");
        assert_eq!(rule("a/b/tmp/", "a/b", true), "/tmp/");
        assert_eq!(dir_of("a/b/out.log"), "a/b");
        assert_eq!(dir_of("a/b/tmp/"), "a/b");
        assert_eq!(dir_of("top.txt"), "");
    }

    #[test]
    fn special_characters_are_taken_literally() {
        assert_eq!(rule("#notes", "", false), "\\#notes");
        assert_eq!(rule("!x", "", false), "\\!x");
        assert_eq!(rule("a#b", "", false), "a#b", "only a leading #");
        assert_eq!(rule("f*[1]?.txt", "", true), "/f\\*\\[1]\\?.txt");
        assert_eq!(rule("back\\slash", "", false), "back\\\\slash");
        assert_eq!(rule("trailing  ", "", false), "trailing\\ \\ ");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_overwritten() {
        use crate::testkit::{Sandbox, write};
        let sb = Sandbox::new();
        let r = sb.init("r");
        write(&r, "junk", "x");
        // not text argus can read: an error, the file as it was
        let private = r.join(".git/info/exclude");
        let bytes = b"# caf\xe9 notes\nbuild/\n".to_vec();
        std::fs::write(&private, &bytes).unwrap();
        let e = add(&r, IgnoreFile::Private, &["junk".into()], true).unwrap_err();
        assert!(e.contains("exclude"), "{e}");
        assert_eq!(std::fs::read(&private).unwrap(), bytes);
        // inverse: one that isn't there yet is started
        std::fs::remove_file(&private).unwrap();
        add(&r, IgnoreFile::Private, &["junk".into()], true).unwrap();
        assert_eq!(std::fs::read_to_string(&private).unwrap(), "/junk\n");
        // and one that's there is added to
        add(&r, IgnoreFile::TopLevel, &["junk".into()], true).unwrap();
        write(&r, "more", "x");
        add(&r, IgnoreFile::TopLevel, &["more".into()], true).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join(".gitignore")).unwrap(),
            "/junk\n/more\n"
        );
    }

    #[test]
    fn nothing_is_written_through_a_link_in_the_work_tree_or_for_a_name_with_a_line_break() {
        use crate::testkit::{Sandbox, write};
        let sb = Sandbox::new();
        let r = sb.init("r");
        let outside = sb.path("outside");
        std::fs::write(&outside, "mine\n").unwrap();
        std::os::unix::fs::symlink(&outside, r.join(".gitignore")).unwrap();
        write(&r, "junk", "x");
        let e = add(&r, IgnoreFile::TopLevel, &["junk".into()], true).unwrap_err();
        assert!(e.contains("symbolic link"), "{e}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "mine\n");
        // a rule is one line: a name with a line break can't be one
        std::fs::remove_file(r.join(".gitignore")).unwrap();
        let e = add(&r, IgnoreFile::TopLevel, &["a\nb".into()], true).unwrap_err();
        assert!(e.contains("line break"), "{e}");
        assert!(!r.join(".gitignore").exists());
    }
}
