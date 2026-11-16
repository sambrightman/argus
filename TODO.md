# To do

What's been asked for, offered or noticed and not done yet. Remove an item
when it's done (the commit says what changed); add one when something is
deferred.

## Asked for

- **Stable machine-readable output.** `--json` works for every command;
  pull, fix, tag, ack, root and paths build their documents explicitly in
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

None known.
