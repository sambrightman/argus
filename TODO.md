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
  Mono, `✓` from Hack, `✗` from Fira Code, Hack and Source Code Pro,
  the messages' fold marks `▸` `▾` from Monaco (they're checked in Menlo,
  SF Mono, Monaco and DejaVu Sans only); kitty's default font isn't
  checked. Whether the partly marked icon (`│`), which every font has,
  reads as "less than `▌`" in each. Rerun the counts once
  the other fonts are installed, and look over the whole set (the nerd
  icons for off the default branch and the two worktree issues were
  picked without a font to check them in; acceptable as they are).

## Offered, not decided

- **Remember unpushed counts between runs** (`reach`), not only within
  one: an answer is kept per tip and set of others, so it holds until
  either moves.
- **A review of the wording** of messages, prompts and the help
  (paused partway).
- **Contention, from each step's CPU time** beside its wall time: a step
  that waited on threads or the disk shows more wall than CPU time. Not
  yet: wall time alone says how much ran side by side, not whether it
  waited.

- **Terminals drawing ambiguous-width symbols two cells wide** (CJK
  setting): detect it and swap those icons. Today `demo --glyphs` marks
  them with `*`.

- **`s`, `u` and `K` for the marked repos.** Fixes, pull and the rest
  apply to every marked repo (else the cursor's); these three take one
  repo, having had no meaning for a whole repo until lately. Proposed:
  - *When:* the cursor on a repo's own row, or a row of its details
    with no binding of its own, and some repo marked. On a file, hunk or
    section nothing changes.
  - *What:* for each marked repo, as for one today: what's marked in
    its details if anything is, else everything on the key's side.
  - *Always asked,* being more than one repo: "Stage everything in 3
    marked repos (12 files)?", and for `K` what goes, as now.
  - *The which-repo menu* stays for files or lines marked in a repo
    outside the set being taken, so no mark is taken or passed over
    unseen.

  To settle first: whether marks inside a marked repo narrow it
  (suggested) or a marked repo is always the whole of it; whether the
  cursor's own repo is in when it isn't marked (every other action says
  no); and the menu's title, "marked repos", which means repos with
  marks in their details and would read as the marked repos too.

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
- **Upstream to gix:** peeling packed refs without reading their objects
  where `packed-refs` says `fully-peeled` (written and tested locally:
  emacs's 259 remote branches cold went from 45ms to 1ms, its status
  from 120-170ms to 85-110ms); and an issue on peeling reading whole
  objects where a header lookup would do, which isn't free with the
  object cache on.

## Known gaps

- **A stash of marked lines takes unstaged text changes only.** With
  lines marked (or a file marked under unstaged alone that's staged
  too), `z m` refuses a file marked whole that's untracked, listed
  under staged, deleted, not text or with its mode changed, and refuses
  staged lines; they'd need the stash's tree built file by file. To do
  after 0.3.0.
- **Dropping a stash can drop another process's.** git drops stashes
  only by position (`stash@{n}`, not a commit id: `drop <id>` is "not a
  stash reference"), and locks for one command, not across two. So
  argus checks the position still holds the stash it means, then drops
  it, and a stash pushed in between (by an editor's `--autostash`, say)
  is dropped instead: by the stash menu's drop, and by a line stash
  undoing itself when its lines can't be taken out. The commit stays
  in the repo until gc (`git fsck --unreachable` finds it). Nothing
  built from git commands closes this; a cheap mitigation is to read
  the list (with gix) after the drop and `git stash store` back every
  entry gone that wasn't meant to go, which errs toward keeping a stash.
