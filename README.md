# argus

See which of your many git clones need attention, clean them up, and keep
them (and whole GitHub orgs) up to date. TUI first, CLI for scripts and cron.

```
 argus  15 repos · 8 need attention
    acme/widget                           main                 ↑7
    sambrightman/dotfiles                 master               ↑2 ✚1 …1
    ~/dev/argus                           main                 ✚9 …2
    emacs-mirror/emacs                    ➦a360712c9d2
    sambrightman/imapclient               allow-sequence-sets  ↑1
    acme/site                             main
      vendor/widget                       ➦8a624e3
```

Status for 300 repos takes ~0.1s warm: in-process gix reads, parallelised
with rayon. See [DESIGN.md](DESIGN.md) for the survey of existing tools,
language choice and architecture.

## Install

```sh
cargo install --path crates/argus-cli
argus init            # writes ~/.config/argus/config.toml
$EDITOR ~/.config/argus/config.toml
argus                 # TUI
```

## Configuration

```toml
roots = ["~/dev", "~/work"]
concurrency = 8                  # parallel fetch/pull/push/clone

[[track]]                        # keep a whole org cloned and pulled
owner = "my-org"
into = "~/work/my-org"
clone = "auto"                   # or "notify": only report new repos
# team = "platform"              # only this team's repos
# include_archived = false
# include_forks = true

[[rule]]                         # glob on github.com/org/repo, org/repo or path
match = "my-org/core-*"
favourite = true

[[rule]]
match = "someone/dotfiles"
sync = true

[[rule]]
match = "~/dev/scratch/**"
exclude = true

[fix]
auto = ["push", "switch-default"]  # kinds `x x` / `argus fix` apply without asking

[tools]
git_ui = "lazygit"               # or "emacsclient -t -e '(magit-status)'"

[ui]
theme = "terminal"               # terminal | dark | light | mono
icons = "unicode"                # unicode | nerd | ascii
[ui.colors]                      # any role: name, #rrggbb, 0-255, "bold red on black"
favourite = "yellow"
```

Marks you set in the TUI (favourite, sync, exclude, snooze, "new") live in
`~/.local/state/argus/state.toml`, never in your hand-written config. They
override config rules.

Credentials and git behaviour are yours: argus runs `git pull`/`push`/`clone`
itself, so `pull.rebase`, `rebase.autoStash`, hooks, signing, credential
helpers and ssh config all apply. GitHub listing uses `GH_TOKEN`,
`GITHUB_TOKEN` or `gh auth token`. Clone protocol follows `gh config get git_protocol`.

## TUI keys

Arrows, emacs and vi motions all work at the same time. Prefix keys open
transient-style menus. Press the item key, or use ↑↓ + Enter.

| keys | |
|---|---|
| `↑↓` `C-p C-n` `j k` | move |
| `n p` `M-n M-p` | next / previous repo |
| `PgUp PgDn` `C-v M-v` `C-u C-d` | page |
| `Home End` `M-< M->` `gg G` | top / bottom |
| `Tab` `Enter` | unfold details: issues, available fixes, branches, files |
| `Space` / `U` | mark / unmark (actions apply to marked repos, else current) |
| `/` `C-s` | filter (readline editing) |
| `a` / `H` | attention-only / show hidden |
| `g` `C-l` / `R` | refresh statuses / rescan roots |
| `!` | shell in repo (TUI suspends; `exit` returns) |
| `$` | process log |
| `?` | help |
| `F` | **Pull**: `p` selected, `a` all syncable, `s` sync (forges + clone new + pull) |
| `f` | **Fetch**: `f` selected, `a` all visible |
| `P` | **Push**: `p` push, `u` publish (set upstream) |
| `z` `c` | **Stash** / **Commit** (prompts for message) |
| `b` | **Branch**: `d` switch to default, `f` fast-forward stale branches |
| `x` | **Fix**: `x` auto-fix selected, `a` all visible |
| `m` | **Mark**: `f` favourite, `s` sync, `x` exclude, `z` snooze, `u` unsnooze, `a` ack new, `c` clear |
| `o` | **Open**: `s` shell, `g` git UI, `e` editor, `w` forge page |
| `v` | **View**: toggles, expand/collapse all, log |

## CLI

```sh
argus status [-a] [--all] [--json]     # -a: attention only
argus sync [-n] [--no-forge] [PATTERN…]
argus fix [-n] [-k switch-default|push|publish|fast-forward|pull|stash]… [PATTERN…]
argus mark PATTERN… [--favourite|--no-favourite] [--sync|--no-sync] [--exclude|--include] [--snooze 3d|--unsnooze] [--clear]
argus ack [PATTERN…]
argus forge list ORG [--team T] [--json]
```

A daily crontab entry:

```cron
0 8 * * 1-5  ~/.cargo/bin/argus sync >> ~/.local/state/argus/sync.log 2>&1
```

`sync` exits 2 if any job failed.

## What counts as needing attention

Sorted favourites first, then by severity, then by amount of change:

conflicts / in-progress merge or rebase › commits on a detached HEAD held by no
ref › **unpushed** work (on *no* remote, so a fork branch pushed to `origin`
while tracking `upstream/main` is fine) › uncommitted changes › upstream
deleted with unmerged work › unpublished branch › other local branches with
unpushed work › detached HEAD › not on default branch › stashes › no remote ›
behind upstream (sync-only).

Fixes are planned from status and are never destructive. `switch-default`
needs a clean tracked tree and nothing unpushed. `push` (a default auto-fix) is fast-forward only,
and only to a same-named upstream branch. Nothing is offered during a conflict.
Each auto-fix action applies **one** fix: an unpushed feature branch is
pushed by one `x x` (or `argus fix`) and switched back to the default
branch by the next. Or snooze it in between (`m z`).

## Development

```sh
cargo test --workspace      # ~140 tests, real git repos in temp dirs
cargo clippy --workspace --all-targets
cargo run -p argus-tui --example snapshot -- ~/dev 'jj\t'   # render a frame as text
```

Tests build real repositories with the `git` CLI under an isolated git config
(`argus_core::testkit`), with local bare repos standing in for GitHub. Each
detector is tested together with its inverse.
