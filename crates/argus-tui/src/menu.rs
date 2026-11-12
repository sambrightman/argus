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
    Mark,
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
    /// A branch to check out, built when opened.
    PickBranch,
    /// The Mark menu's marks for the selected repo's owner as a whole.
    Owner,
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
            MenuId::Mark => 'm',
            MenuId::Open => 'o',
            MenuId::View => 'v',
            MenuId::Ignore => 'I',
            MenuId::Gitignore => 'i',
            MenuId::Reset => 'X',
            MenuId::PickCommit => 'X',
            MenuId::PickBranch => 'b',
            MenuId::Owner => 'm',
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
        MenuId::Mark,
        MenuId::Open,
        MenuId::View,
        MenuId::Reset,
    ];

    /// A list of things (commits) rather than of choices: `n p j k` move
    /// through it and are never its keys. A new list goes here, with its
    /// items keyed from [`LIST_KEYS`].
    pub fn is_list(self) -> bool {
        matches!(self, MenuId::PickCommit | MenuId::PickBranch)
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
                    item('a', "pull all marked for pull", Action::Pull(Scope::All)),
                    item(
                        'c',
                        "clone new repos of tracked owners, then pull all marked",
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
                    switch('u', "include untracked", Switch::Untracked),
                    switch('k', "keep index", Switch::KeepIndex),
                    item('z', "stash all changes", Action::Stash(StashKind::All)),
                    item(
                        'i',
                        "stash staged changes only",
                        Action::Stash(StashKind::Staged),
                    ),
                    item(
                        'm',
                        "stash with a message…",
                        Action::Stash(StashKind::WithMessage),
                    ),
                    item(
                        'f',
                        "stash files marked in details",
                        Action::Stash(StashKind::MarkedFiles),
                    ),
                    item('p', "pop the newest stash", Action::PopStash),
                    item('d', "drop the stash under the cursor…", Action::DropStash),
                ],
            ),
            MenuId::Commit => (
                "Commit",
                vec![
                    item('c', "commit staged changes…", Action::Commit { all: false }),
                    item('A', "commit all changes…", Action::Commit { all: true }),
                    item('a', "amend the last commit…", Action::Amend),
                    item('f', "fixup a commit with what's staged…", Action::Fixup),
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
                    item('d', "switch to default branch", Action::SwitchDefault),
                    item(
                        'r',
                        "reattach detached HEAD to its branch",
                        Action::Reattach,
                    ),
                    item(
                        'c',
                        "create a branch at detached HEAD…",
                        Action::CreateBranch,
                    ),
                    item(
                        'x',
                        "delete merged branches (upstream gone or none)…",
                        Action::DeleteMergedBranches,
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
            MenuId::Mark => (
                "Mark",
                vec![
                    item('f', "toggle favourite", Action::ToggleFavourite),
                    item('p', "toggle pull", Action::TogglePull),
                    item('i', "ignore issues…", Action::IgnoreIssues),
                    item('I', "toggle ignoring the repo", Action::ToggleIgnore),
                    item('z', "snooze for…", Action::Snooze),
                    item('u', "unsnooze", Action::Unsnooze),
                    item('a', "acknowledge new", Action::Ack),
                    item('c', "clear marks", Action::ClearMarks),
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
            MenuId::PickBranch => ("Branches", vec![]),
            // titled with the owner and labelled as toggled when opened
            MenuId::Owner => (
                "Owner",
                vec![
                    item(
                        'f',
                        "toggle favourite",
                        Action::ToggleOwnerMark(argus_core::rules::Mark::Favourite),
                    ),
                    item(
                        'p',
                        "toggle pull",
                        Action::ToggleOwnerMark(argus_core::rules::Mark::Pull),
                    ),
                    item(
                        'I',
                        "toggle ignoring",
                        Action::ToggleOwnerMark(argus_core::rules::Mark::Ignore),
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
                    item('l', "toggle process log", Action::ToggleLog),
                    item('g', "refresh statuses", Action::Refresh),
                    item('G', "rescan roots", Action::Rescan),
                    item('t', "next theme (this session)", Action::NextTheme),
                    item('i', "next icon set (this session)", Action::NextIcons),
                ],
            ),
        };
        Menu {
            id: self,
            title: title.into(),
            items,
        }
    }
}

/// Keys for the items of every list menu (see [`MenuId::is_list`]), in
/// order: never `n` `p` `j` `k`, which move through lists, nor `q`, which
/// closes menus. A list has at most this many items.
pub const LIST_KEYS: &str = "1234567890abcdefghilmorstuvwxyz";

/// The Fix menu's key for a fix of `kind`.
pub fn fix_key(kind: argus_core::fix::FixKind) -> char {
    use argus_core::fix::FixKind::*;
    match kind {
        Abort => 'A',
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
