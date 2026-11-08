//! Transient-style menus: a prefix key opens a menu, one more key runs an
//! action. Items can also be chosen with arrows + Enter.

use crate::app::{Action, Scope, StashKind, Switch};

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
}

pub struct Item {
    pub key: char,
    pub label: &'static str,
    pub action: Action,
    /// A switch, shown as `[x]`/`[ ]`; choosing it keeps the menu open.
    pub switch: Option<Switch>,
}

pub struct Menu {
    pub id: MenuId,
    pub title: &'static str,
    pub items: Vec<Item>,
}

fn item(key: char, label: &'static str, action: Action) -> Item {
    Item {
        key,
        label,
        action,
        switch: None,
    }
}

fn switch(key: char, label: &'static str, s: Switch) -> Item {
    Item {
        key,
        label,
        action: Action::Toggle(s),
        switch: Some(s),
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
        }
    }

    pub const ALL: [MenuId; 10] = [
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
    ];

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
                    item('p', "push selected to upstream", Action::Push),
                    item('u', "publish: push and set upstream", Action::Publish),
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
                ],
            ),
            MenuId::Commit => (
                "Commit",
                vec![item('c', "commit all changes…", Action::Commit)],
            ),
            MenuId::Branch => (
                "Branch",
                vec![
                    item('d', "switch to default branch", Action::SwitchDefault),
                    item(
                        'f',
                        "fast-forward stale local branches",
                        Action::FastForward,
                    ),
                ],
            ),
            MenuId::Fix => (
                "Fix",
                vec![
                    item('x', "auto-fix selected", Action::AutoFix(Scope::Selected)),
                    item(
                        'a',
                        "auto-fix all shown, except hidden",
                        Action::AutoFix(Scope::All),
                    ),
                ],
            ),
            MenuId::Mark => (
                "Mark",
                vec![
                    item('f', "toggle favourite", Action::ToggleFavourite),
                    item('p', "toggle pull", Action::TogglePull),
                    item('i', "toggle ignored", Action::ToggleIgnore),
                    item(
                        'I',
                        "ignore the issue under the cursor (or not)",
                        Action::ToggleIgnoreIssue,
                    ),
                    item(
                        'P',
                        "the same, pinned the other way (stashes to their messages; branch or HEAD not)",
                        Action::ToggleIgnoreIssueOtherPin,
                    ),
                    item('z', "snooze for…", Action::Snooze),
                    item('u', "unsnooze", Action::Unsnooze),
                    item('a', "acknowledge new", Action::Ack),
                    item('c', "clear marks (patterns decide)", Action::ClearMarks),
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
            title,
            items,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
