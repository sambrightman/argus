//! Transient-style menus: a prefix key opens a menu, one more key runs an
//! action. Items can also be chosen with arrows + Enter.

use crate::app::{Action, Scope, StashKind, Switch};
use argus_core::ops::ResetMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Pull,
    Fetch,
    Push,
    Stash,
    Commit,
    Branch,
    Fix,
    Tag,
    Open,
    View,
    /// A repo's issues to ignore, built when opened (no prefix key).
    Ignore,
    /// Where to add ignore rules for untracked files, built when opened.
    Gitignore,
    /// How to reset the current branch.
    Reset,
    /// A commit to reset to or fix up, built when opened.
    PickCommit,
    /// The Tag menu's tags for the selected repo's owner as a whole.
    Owner,
    /// Which marks a key is for, asked when some are in a repo the cursor
    /// isn't in: built when opened.
    Across,
}

pub struct Item {
    pub key: char,
    pub label: String,
    pub action: Action,
    /// A switch, shown as `[x]`/`[ ]`; choosing it keeps the menu open.
    pub switch: Option<Switch>,
    /// Choosing it keeps the menu open, to show what changed.
    pub stays: bool,
}

pub struct Menu {
    pub id: MenuId,
    pub title: String,
    pub items: Vec<Item>,
    /// Said dimmed under the items, never chosen.
    pub notes: Vec<String>,
}

pub fn item(key: char, label: impl Into<String>, action: Action) -> Item {
    Item {
        key,
        label: label.into(),
        action,
        switch: None,
        stays: false,
    }
}

/// An item that changes something shown in the menu, which stays open.
pub fn stepper(key: char, label: impl Into<String>, action: Action) -> Item {
    Item {
        stays: true,
        ..item(key, label, action)
    }
}

pub fn switch(key: char, label: impl Into<String>, s: Switch) -> Item {
    Item {
        key,
        label: label.into(),
        action: Action::Toggle(s),
        switch: Some(s),
        stays: true,
    }
}

impl MenuId {
    /// Prefix key that opens the menu from the repo list (magit conventions).
    pub fn key(self) -> char {
        match self {
            MenuId::Pull => 'F',
            MenuId::Fetch => 'f',
            MenuId::Push => 'P',
            MenuId::Stash => 'z',
            MenuId::Commit => 'c',
            MenuId::Branch => 'b',
            MenuId::Fix => 'x',
            MenuId::Tag => 't',
            MenuId::Open => 'o',
            MenuId::View => 'v',
            MenuId::Ignore => 'I',
            MenuId::Gitignore => 'i',
            MenuId::Reset => 'X',
            MenuId::PickCommit => 'X',
            MenuId::Owner => 't',
            // (opened by s, u or K: no key of its own)
            MenuId::Across => ' ',
        }
    }

    pub const ALL: [MenuId; 11] = [
        MenuId::Pull,
        MenuId::Fetch,
        MenuId::Push,
        MenuId::Stash,
        MenuId::Commit,
        MenuId::Branch,
        MenuId::Fix,
        MenuId::Tag,
        MenuId::Open,
        MenuId::View,
        MenuId::Reset,
    ];

    /// A list of things (commits) rather than of choices: `n p j k` move
    /// through it and are never its keys. A new list goes here, with its
    /// items keyed from [`LIST_KEYS`].
    pub fn is_list(self) -> bool {
        matches!(self, MenuId::PickCommit)
    }

    pub fn from_key(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == c)
    }

    pub fn menu(self) -> Menu {
        let (title, items) = match self {
            MenuId::Pull => (
                "Pull",
                vec![
                    item('p', "pull selected", Action::Pull(Scope::Selected)),
                    item('a', "pull all tagged for pull", Action::Pull(Scope::All)),
                    item(
                        'c',
                        "clone new repos of tracked owners, then pull all tagged",
                        Action::PullAndClone,
                    ),
                ],
            ),
            MenuId::Fetch => (
                "Fetch",
                vec![
                    item('f', "fetch selected", Action::Fetch(Scope::Selected)),
                    item(
                        'a',
                        "fetch all shown, except hidden",
                        Action::Fetch(Scope::All),
                    ),
                ],
            ),
            MenuId::Push => (
                "Push",
                vec![
                    item('p', "push to its remote branch", Action::Push),
                    item(
                        'u',
                        "publish: put the branch on the remote",
                        Action::Publish,
                    ),
                    item('b', "push other branches", Action::PushBranches),
                    item('r', "add a remote (origin)…", Action::AddRemote),
                ],
            ),
            MenuId::Stash => (
                "Stash",
                vec![
                    // (starred: these two switches, and what goes by them)
                    switch('u', "include untracked*", Switch::Untracked),
                    switch('k', "keep index*", Switch::KeepIndex),
                    switch('M', "message", Switch::Message),
                    item('z', "stash all*", Action::Stash(StashKind::All)),
                    item('m', "stash marked*", Action::Stash(StashKind::MarkedFiles)),
                    item('g', "stash staged", Action::Stash(StashKind::Staged)),
                    item('p', "pop…", Action::PopStash),
                    item('d', "drop…", Action::DropStash),
                ],
            ),
            MenuId::Commit => (
                "Commit",
                vec![
                    item('c', "commit staged changes…", Action::Commit { all: false }),
                    item('A', "commit all changes…", Action::Commit { all: true }),
                    item('a', "amend the last commit…", Action::Amend),
                    item(
                        'f',
                        "fixup a commit with what's staged…",
                        Action::Fixup {
                            squash: false,
                            instant: false,
                        },
                    ),
                    item(
                        'F',
                        "instant fixup: fold what's staged into a commit…",
                        Action::Fixup {
                            squash: false,
                            instant: true,
                        },
                    ),
                    item(
                        's',
                        "squash: a squash! commit of a commit, adding a message…",
                        Action::Fixup {
                            squash: true,
                            instant: false,
                        },
                    ),
                    item(
                        'S',
                        "instant squash: fold what's staged and a message into a commit…",
                        Action::Fixup {
                            squash: true,
                            instant: true,
                        },
                    ),
                ],
            ),
            MenuId::Branch => (
                "Branch",
                vec![
                    item('b', "checkout a branch…", Action::Checkout),
                    item(
                        '-',
                        "checkout the previous branch",
                        Action::CheckoutPrevious,
                    ),
                    item(
                        's',
                        "spin off: unpushed commits to a new branch, checked out…",
                        Action::SpinOff { stay: false },
                    ),
                    item(
                        'S',
                        "spin out: unpushed commits to a new branch, staying here…",
                        Action::SpinOff { stay: true },
                    ),
                    item('d', "switch to default branch", Action::SwitchDefault),
                    item(
                        'r',
                        "reattach detached HEAD to its branch",
                        Action::Reattach,
                    ),
                    item(
                        'c',
                        "create a branch at HEAD and check it out…",
                        Action::NewBranch,
                    ),
                    item(
                        'x',
                        "delete merged branches (upstream gone or none)…",
                        Action::DeleteMergedBranches,
                    ),
                    item(
                        'k',
                        "delete the branch under the cursor in the details…",
                        Action::DeleteBranch,
                    ),
                    item(
                        'f',
                        "fast-forward stale local branches",
                        Action::FastForward,
                    ),
                ],
            ),
            // built when opened: see App::add_fixes
            MenuId::Fix => ("Fix", vec![]),
            MenuId::Tag => (
                "Tag",
                vec![
                    item('p', "toggle pull", Action::TogglePull),
                    item('i', "ignore issues…", Action::IgnoreIssues),
                    item('I', "toggle ignoring the repo", Action::ToggleIgnore),
                    item(
                        'z',
                        "snooze for… (on an issue line: that issue)",
                        Action::Snooze,
                    ),
                    item(
                        'u',
                        "unsnooze (on an issue line: that issue)",
                        Action::Unsnooze,
                    ),
                    item('a', "acknowledge new", Action::Ack),
                    item('c', "clear tags", Action::ClearTags),
                    item(
                        'o',
                        "the owner (all its repos)…",
                        Action::OpenMenu(MenuId::Owner),
                    ),
                ],
            ),
            MenuId::Open => (
                "Open",
                vec![
                    item('s', "shell in repo", Action::Shell),
                    item('g', "git UI (tools.git_ui)", Action::GitUi),
                    item('e', "editor", Action::Editor),
                    item('w', "forge page in browser", Action::Browse),
                ],
            ),
            MenuId::Ignore => ("Ignore issues", vec![]),
            MenuId::Gitignore => ("Gitignore", vec![]),
            MenuId::Across => ("Marked", vec![]),
            MenuId::Reset => (
                "Reset",
                vec![
                    item(
                        's',
                        "soft: keep the changes staged",
                        Action::Reset(ResetMode::Soft),
                    ),
                    item(
                        'm',
                        "mixed: keep the changes, unstaged",
                        Action::Reset(ResetMode::Mixed),
                    ),
                    item(
                        'k',
                        "keep: drop the commits' changes; refuse if uncommitted ones are in the way",
                        Action::Reset(ResetMode::Keep),
                    ),
                    item(
                        'h',
                        "hard: drop the commits' changes and every uncommitted one…",
                        Action::Reset(ResetMode::Hard),
                    ),
                ],
            ),
            MenuId::PickCommit => ("Commits", vec![]),
            // titled with the owner and labelled as toggled when opened; its
            // tags added then, as the Tag menu's
            MenuId::Owner => (
                "Owner",
                vec![
                    item(
                        'p',
                        "toggle pull",
                        Action::ToggleOwnerTag(argus_core::rules::Tag::Pull),
                    ),
                    item(
                        'I',
                        "toggle ignoring",
                        Action::ToggleOwnerTag(argus_core::rules::Tag::Ignore),
                    ),
                ],
            ),
            MenuId::View => (
                "View",
                vec![
                    item('a', "toggle attention only", Action::ToggleAttention),
                    item('h', "toggle hidden (ignored/snoozed)", Action::ToggleHidden),
                    item('e', "expand all", Action::ExpandAll),
                    item('c', "collapse all", Action::CollapseAll),
                    item('$', "toggle process log", Action::ToggleLog),
                    item('m', "message history", Action::History),
                    item('g', "refresh statuses", Action::Refresh),
                    item('G', "rescan roots", Action::Rescan),
                    item('t', "next theme", Action::NextTheme),
                    item('i', "next icon set", Action::NextIcons),
                ],
            ),
        };
        Menu {
            id: self,
            title: title.into(),
            items,
            notes: vec![],
        }
    }
}

/// Keys for the items of every list menu (see [`MenuId::is_list`]), in
/// order: never `n` `p` `j` `k`, which move through lists, nor `q`, which
/// closes menus. A list has at most this many items.
pub const LIST_KEYS: &str = "1234567890abcdefghilmorstuvwxyz";

/// Tags whose key the Tag menu (or the Owner menu) has already: they're
/// left out of it, so say so.
pub fn tag_key_clashes(c: &argus_core::Config) -> Vec<String> {
    let taken = |id: MenuId| {
        id.menu()
            .items
            .into_iter()
            .map(|i| i.key)
            .collect::<Vec<_>>()
    };
    let (tag, owner) = (taken(MenuId::Tag), taken(MenuId::Owner));
    let mut seen = vec![];
    c.tags
        .iter()
        .filter_map(|(name, d)| Some((name, d.key?)))
        .filter_map(|(name, k)| {
            // pull's and ignore's own items have their keys already
            let own = match name.as_str() {
                argus_core::config::PULL => Some('p'),
                argus_core::config::IGNORE => Some('I'),
                _ => None,
            };
            let clash = if own == Some(k) {
                None
            } else if tag.contains(&k) || owner.contains(&k) {
                Some(format!(
                    "[tags.{name}] key {k:?} is the Tag menu's own: choose another"
                ))
            } else if seen.contains(&k) {
                Some(format!(
                    "[tags.{name}] key {k:?} is another tag's: choose another"
                ))
            } else {
                None
            };
            seen.push(k);
            clash
        })
        .collect()
}

/// The Fix menu's key for a fix of `kind`.
pub fn fix_key(kind: argus_core::fix::FixKind) -> char {
    use argus_core::fix::FixKind::*;
    match kind {
        Abort => 'k',
        Stash => 's',
        Commit => 'c',
        Gitignore => 'i',
        Branch => 'b',
        Push => 'p',
        Publish => 'u',
        AddRemote => 'R',
        PushBranches => 'P',
        SwitchDefault => 'd',
        Reattach => 'r',
        Pull => 'F',
        FastForward => 'f',
        DeleteBranches => 'D',
        StashPop => 'z',
        SubmoduleUpdate => 'S',
        PruneWorktrees => 'w',
        RemoveWorktree => 'W',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tags_key_the_menu_has_is_warned_about() {
        let c = argus_core::Config::parse(
            "[tags.work]\nkey = 'w'\n[tags.pulled]\nkey = 'z'\n[tags.other]\nkey = 'w'\n[tags.nokey]\n",
        )
        .unwrap();
        let w = tag_key_clashes(&c);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("pulled") && w[0].contains("Tag menu"),
            "{w:?}"
        );
        assert!(
            w[1].contains("other") && w[1].contains("another tag"),
            "{w:?}"
        );
        assert!(tag_key_clashes(&argus_core::Config::default()).is_empty());
        // pull and ignore may take other keys, but not the menu's own or
        // another tag's
        let ok = argus_core::Config::parse("[tags.pull]\nkey = 'P'\n[tags.ignore]\nkey = 'x'\n");
        assert!(tag_key_clashes(&ok.unwrap()).is_empty());
        let c = argus_core::Config::parse("[tags.pull]\nkey = 'z'\n[tags.ignore]\nkey = 'f'\n")
            .unwrap();
        let w = tag_key_clashes(&c);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w[0].contains("[tags.pull]") && w[0].contains("Tag menu"),
            "{w:?}"
        );
        assert!(
            w[1].contains("[tags.ignore]") && w[1].contains("another tag"),
            "{w:?}"
        );
    }

    #[test]
    fn list_keys_leave_out_moving_and_closing() {
        for c in ['n', 'p', 'j', 'k', 'q'] {
            assert!(!LIST_KEYS.contains(c), "{c}");
        }
        let mut seen = std::collections::HashSet::new();
        assert!(LIST_KEYS.chars().all(|c| seen.insert(c)), "no repeats");
    }

    #[test]
    fn keys_unique_within_each_menu_and_across_prefixes() {
        let mut prefixes = vec![];
        for id in MenuId::ALL {
            assert_eq!(MenuId::from_key(id.key()), Some(id));
            prefixes.push(id.key());
            let m = id.menu();
            let mut keys: Vec<char> = m.items.iter().map(|i| i.key).collect();
            let n = keys.len();
            keys.sort();
            keys.dedup();
            assert_eq!(keys.len(), n, "duplicate key in {}", m.title);
            assert!(!keys.contains(&'q'), "q always cancels");
        }
        let n = prefixes.len();
        prefixes.sort();
        prefixes.dedup();
        assert_eq!(prefixes.len(), n);
    }
}
