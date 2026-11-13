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
- **Auto-fix all, chaining** fixes until nothing more applies.
- **Glyph font gaps:** `◆` is missing from SF Mono, `✓` from Hack, `✗` from
  Fira Code, Hack and Source Code Pro; Monaco and kitty's default font
  aren't checked. Rerun the counts once the other fonts are installed, and
  decide whether to change those icons.

## Offered, not decided

- **Terminals drawing ambiguous-width symbols two cells wide** (CJK
  setting): detect it and swap those icons. Today `demo --glyphs` marks
  them with `*`.
- **Release profile options not measured:** fat LTO, `panic = "abort"`
  (the TUI's panic hook still restores the terminal), and split, packed
  debug info (a `.dSYM`) kept aside to symbolicate crash reports.
- **Editing a patch to discard** (`K` after `e`): reversed in the work
  tree, so it can't be undone; it would confirm after the editor closes.
- **`argus status --details`:** the TUI's details lines under each repo
  (today: one line per repo; `--json` and `argus fix -n` have the rest).
- **A row icon for worktree issues:** a warning mark on the main repo for
  stale records, and on a finished worktree's own row.
- **Caching the pruning checks** for trees with many symlinks, if they
  ever show up as slow.

## Next steps

- **Marks next:** pull and ignore as marks with behaviour fields (they
  keep their own rules for now; see DESIGN.md §9, custom marks).
- **Pull requests:** fetched with the remote, shown per branch; a merged PR
  makes a branch safe to delete, squash-merged ones included.
- **Orphans** (local clones the forge no longer lists): reported only, no
  archive/move action.
- **GitLab/Gitea forges** (implement `argus_core::forge::Forge`); a **jj
  backend**.
- **`argus-server`** (axum, JSON over the same `Workspace`).
- **Filesystem watching** for live refresh (not for now). Today: `g`, and
  after each operation.
- **Python bindings** to `argus-core` (pyo3/maturin), for scripting over
  the same workspace, status and fix planning.

## Known gaps

- A git config include file created after a repo was opened isn't noticed
  until the including file changes.
- Diffs don't apply textconv.
- `git clone` in a test's setup once failed copying `maintenance.lock`
  (rerun passed): watch for it.
- For a submodule with both modified and untracked files, whether its
  entry shows ` m` or ` ?` depends on which gix finds first; the order
  isn't confirmed in gix's code.
