# To do

What's been asked for, offered or noticed and not done yet. Remove an item
when it's done (the commit says what changed); add one when something is
deferred.

## Asked for

- **Stable machine-readable output.** `--json` works for every command;
  pull, fix, mark, ack, root and paths build their documents explicitly in
  the CLI, but `status` and `forge list` still serialise internal structs
  (`Entry`, `RemoteRepo`), which isn't a stable contract. Consider a
  versioned schema and/or a line-oriented `--porcelain` format (like
  `git status --porcelain=v2`).
- **Embedded terminal pane** (alacritty_terminal / tui-term) instead of
  suspending the TUI for a shell or git UI.
- **A review of the glyphs,** font gaps included: `◆` is missing from SF
  Mono, `✓` from Hack, `✗` from Fira Code, Hack and Source Code Pro;
  Monaco and kitty's default font aren't checked. Rerun the counts once
  the other fonts are installed, and look over the whole set (the nerd
  icons for off the default branch and the two worktree issues were
  picked without a font to check them in; acceptable as they are).

## Offered, not decided

- **Terminals drawing ambiguous-width symbols two cells wide** (CJK
  setting): detect it and swap those icons. Today `demo --glyphs` marks
  them with `*`.

## Next steps

- **More from the forge:** the details show GitHub's open issue and pull
  request counts and each branch's pull request (`[ui] forge_info`,
  `status --forge`). Not yet: in `--json`; forges other than GitHub.
- **GitLab/Gitea forges** (implement `argus_core::forge::Forge`); a **jj
  backend**.
- **`argus-server`** (axum, JSON over the same `Workspace`).
- **Filesystem watching** for live refresh (not for now). Today: `g`, and
  after each operation.
- **Python bindings** to `argus-core` (pyo3/maturin), for scripting over
  the same workspace, status and fix planning.

## Known gaps

From the review, not fixed yet:

- **Tidying:** pull and ignore are marks in config but still carried
  apart inside (`Paint.pull`/`pull_coloured`, `config::acts`, key
  remapping) where plain mark indices would do; `Issue::ignore_list()`
  in place of the matches that map an issue to its list; the Owner
  menu's `state_labels` found by action, not by place; working-tree and
  commit diffs built by two functions that could be one.
- **Lower-severity data loss:** an instant fixup or squash rebases
  without `--rebase-merges` (merges after the target are flattened);
  switching branches runs `git submodule update --recursive` whatever
  the submodules hold; spin-off's `reset --hard` of the branch left
  behind; soft and mixed resets aren't asked about.
