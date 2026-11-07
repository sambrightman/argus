# argus

See which of your many git clones need attention, clean them up, and keep
them (and whole GitHub orgs) up to date. TUI first, CLI for scripts and cron.

```
 argus  15 repos · 8 need attention
    acme/widget                           main                 ↑7
    sambrightman/dotfiles                 master               ↑2 ±1 ?1
    ~/dev/argus                           main                 ±9 ?2
    emacs-mirror/emacs                    @a360712c9d2
    sambrightman/imapclient               allow-sequence-sets  ↑1
    acme/site                             main
      vendor/widget                       @8a624e3
```

Status for 300 repos takes ~0.1s warm: in-process gix reads, parallelised
with rayon. See [DESIGN.md](DESIGN.md) for the survey of existing tools,
language choice and architecture.

## Install

```sh
cargo install --path crates/argus-cli
cd ~/dev && argus     # TUI over the current directory
argus root add ~/dev ~/work   # save roots; then `argus` works from anywhere
```

With no roots configured, argus scans the current directory and says so.
On first run it writes `~/.config/argus/config.toml`: every setting with
its default and documentation, commented out, generated from the code (so
it can't drift). `argus root add|remove|list` edit it in place, keeping
your comments. With extended files, `root add` appends to the `roots` list
that takes effect (the highest-precedence file that has one), else to an
`extend_roots`, else starts `roots` at the top of the main file; `root
remove` takes the directory out of every file that lists it. Each names
the file it edited when it isn't the main one. If `max_depth` stops a scan before the bottom of a tree,
argus warns and names a directory it skipped.

## Configuration

A taste (see the generated file for everything):

```toml
extend = ["~/.config/argus/work.toml"]  # read first; this file overrides them
roots = ["~/dev", "~/work"]
extend_exclude_paths = ["build-cache"]  # any list: extend_<name> adds instead of replacing
max_depth = 6                    # levels below each root; 0: no limit

# repos or globs on github.com/org/repo, org/repo or path; later entries
# win, ! turns off; marking in the TUI or `argus mark` edits these lists
favourite = ["my-org/core-*"]
sync = ["someone/dotfiles", "my-org/*", "!my-org/huge-monorepo"]
ignore = ["~/dev/scratch/**"]  # hidden, and left out of sync, clones and bulk actions

[concurrency]                    # 0: automatic
disk = 0                         # threads for discovery and status
network = 0                      # fetches, pulls, pushes, clones at once

[[track]]                        # keep a whole org cloned and pulled
owner = "my-org"
into = "~/work/my-org"
clone = "auto"                   # or "notify": only report new repos
# team = "platform"              # only this team's repos
# include_archived = false
# include_forks = true

[fix]
auto = ["push", "switch-default"]  # kinds `x x` / `argus fix` apply without asking

[tools]
git_ui = "lazygit"               # or "emacsclient -t -e '(magit-status)'"

[ui]
theme = "terminal"               # terminal | dark | light | mono
icons = "unicode"                # unicode | nerd (Nerd Font) | ascii
[ui.icon_overrides]              # any icon, by its legend name
favourite = "★"
[ui.colors]                      # any role: name, #rrggbb, 0-255, "bold red on black"
favourite = "yellow"
```

`argus demo` opens the TUI on throwaway sample repos in every state
(dirty, ahead, behind, diverged, mid-merge, detached on a tag, worktrees,
submodules, errors…), deleted when you quit. In the TUI, `v t` and `v i`
cycle themes and icon sets for the session, and `?` ends with a legend of
every icon. Icons cover every symbol argus prints, badges and interface
alike (spinner, marked rows, job results), so `icons = "ascii"` is ASCII
throughout. The `unicode` icons use characters common monospace fonts
have, with a few known gaps; with a Nerd Font, `icons = "nerd"` uses its
glyphs. Override single icons in `[ui.icon_overrides]` if your font has
something you prefer. `argus demo --glyphs` prints every icon, the
punctuation argus uses as text, and alternatives, between bars and
letters (`|◆|M◆M`): a glyph your font lacks is drawn from a fallback
font and may run into its neighbours. `*` marks glyphs that take two
cells in CJK (ambiguous-width) mode.

A detached HEAD shows its tag if the commit has one (`@v1.2 +1` when there
are more: annotated tags first, then the highest version), otherwise the
commit id. Details list the commit and all its tags.

Favourite, sync and ignore live in config: marking a repo in the TUI or
with `argus mark` adds its name to the list (or `!name` to turn off what a
pattern says) and saves the change, keeping the rest of the file as
written; clearing removes the repo's own entries, so patterns decide
again. With extended files, an entry is removed from whichever file holds it, and
new entries go into your main file (as `extend_favourite` etc. when an
extended file sets the list, so its entries stay). Nothing is dropped for
repos that have gone. Snoozes and "new"
flags are kept in `~/.local/state/argus/state.toml`. Marks an older argus
kept there are shown as config lines to copy, and left in place.

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
| `Tab` `Enter` | unfold details: issues, available fixes, branches, files; on a file, its diff (unstaged, then staged); on a hunk, fold it. Changed words are highlighted (`[ui] refine = "all"`, `"selected"` for the hunk under the cursor, or `"off"`) |
| `Space` / `U` | mark / unmark (actions apply to marked repos, else current); on a file in the details, marks the file |
| `s` `u` `K` | stage / unstage / discard files: the marked ones, else the one under the cursor in the details. Discard returns them to HEAD (deleting untracked files) and always asks first |
| `/` `C-s` | filter (readline editing) |
| `a` / `H` | show all / attention only (the default, `[ui] attention_only`); show hidden |
| `g` `C-l` / `R` | refresh statuses / rescan roots |
| `!` | shell in repo (TUI suspends; `exit` returns) |
| `$` | process log |
| `?` | help |
| `F` | **Pull**: `p` selected, `a` all syncable, `s` sync (forges + clone new + pull) |
| `f` | **Fetch**: `f` selected, `a` all shown except hidden |
| `P` | **Push**: `p` push, `u` publish (set upstream) |
| `z` | **Stash**: switches `u` include untracked, `k` keep index (from `[stash]`, for the session); `z` all, `i` staged only, `m` with a message, `f` files marked in the details |
| `c` | **Commit** (prompts for message) |
| `b` | **Branch**: `d` switch to default, `f` fast-forward stale branches |
| `x` | **Fix**: `x` auto-fix selected, `a` all shown except hidden |
| `m` | **Mark**: `f` favourite, `s` sync, `i` ignore, `z` snooze, `u` unsnooze, `a` ack new, `c` clear |
| `o` | **Open**: `s` shell, `g` git UI, `e` editor, `w` forge page |
| `v` | **View**: toggles, expand/collapse all, log |

## CLI

```sh
argus status [-a|--all] [--hidden]     # repos needing attention; -a: clean too
argus sync [-n] [--no-forge] [PATTERNS…]
argus fix [-n] [-F] [-k KINDS]… [PATTERNS…]   # -k push,stash -k pull: lists combine; -F fetches first
argus mark PATTERNS… [--favourite|--no-favourite] [--sync|--no-sync] [--ignore|--unignore] [--snooze 3d|--unsnooze] [--clear]
argus ack [PATTERNS…]
argus forge list ORG [--team T]
argus root add [DIRS…] | remove DIRS… | list
argus paths                            # config and state file locations
argus completions bash|zsh|fish|elvish|powershell
```

Every command except the TUI and completions takes `--json`: one JSON
document on stdout (messages and progress stay on stderr). For example
`argus --json fix -n` lists each repo with its `planned` fix, `then`, and
`alternatives`; `argus --json sync` lists each job's `outcome` (`ok`,
`failed`, `skipped`) and a `summary`.

Completions cover commands, options and your repos' names (`argus mark
acme/<Tab>`). The shell asks `argus` for them as you type, so load the
script fresh at shell startup, where it always matches the installed argus:
`source <(argus completions zsh)` in `~/.zshrc` (likewise bash), or
`argus completions fish | source` in `~/.config/fish/config.fish`. Repo
names come from your config (or `ARGUS_CONFIG`); `--root` on the command
line isn't seen while completing.

Every command takes `--config FILE` / `--state FILE`, or the environment
variables `ARGUS_CONFIG` / `ARGUS_STATE`, and `--root DIR` (repeatable)
to scan other directories just this once; `--max-depth N` overrides
`max_depth` for one run (0: no limit), and `-L` / `--follow-symlinks` follows symlinked
directories (or set `follow_symlinks = true`).

Anything that permanently loses data asks first: in the TUI a `y/n` in the
status line, in the CLI a prompt on the terminal. `-y` / `--yes` proceeds
without asking (scripts, cron); without a terminal and without `--yes`,
the command refuses and changes nothing. Today that's discarding file
changes (always) and clearing the marks of more than one repo.

Directories whose name starts with `.` are skipped (`skip_hidden`); add
one as a root to scan it, e.g. `argus root add ~/.emacs.d`. `exclude_paths` lists
directories never scanned, like gitignore: without a `/` a pattern is a
directory name at any depth (`node_modules`, `*.egg-info`); starting with
`/` or `~` it's a full path (`~/dev/archive`, `~/src/*/third_party`); any
other with a `/` is below each root (`go/pkg`, in the defaults: the Go
module cache under a root). Roots are always scanned.
To keep scanning repos but hide them, list them in `ignore`. Hidden
repos (ignored or snoozed; `H` shows them) are never part of a bulk
action such as fetch all or fix all, even while shown; choosing one
explicitly still works.

A daily crontab entry:

```cron
0 8 * * 1-5  ~/.cargo/bin/argus sync >> ~/.local/state/argus/sync.log 2>&1
```

`sync` exits 2 if any job failed.

## What counts as needing attention

Repos needing attention come before clean ones; within each group,
favourites first, then by severity, then by amount of change:

conflicts / in-progress merge or rebase › commits on a detached HEAD held by no
ref › **unpushed** work (on *no* remote, so a fork branch pushed to `origin`
while tracking `upstream/main` is fine) › uncommitted changes › upstream
deleted with unmerged work › unpublished branch › other local branches with
unpushed work › detached HEAD › not on default branch › stashes › no remote ›
behind upstream (sync-only).

Fixes are planned from status and are never destructive. `switch-default`
needs a clean tracked tree and nothing unpushed. `push` (a default auto-fix) is fast-forward only,
and only to a same-named upstream branch. Nothing is offered during a conflict.
Each auto-fix action applies **one** fix: the first kind in `[fix] auto`
(or `-k`, in the order given) that the repo needs. An unpushed feature
branch is pushed by one `x x` (or `argus fix`) and switched back to the
default branch by the next. Or snooze it in between (`m z`).
Kinds you don't list rank after the ones you do, in the default order:
stash, commit, push, publish, switch-default, pull, fast-forward.

`argus fix -n` shows, per repo, the fix this run would apply, then the
chain later runs would continue with, then other fixes available now
(with the `-k` that would allow them):

```
acme/feature: would stash (1 uncommitted path(s))
  then: push (1 commit(s) ahead of origin/feat)
  then: switch main (feat has nothing unpushed)
  alternative: fast-forward dev (dev is 2 behind origin/dev), with -k fast-forward
```

The chain is predicted from status, assuming each step succeeds. The TUI
details show the same (`auto-fix`, `then`, `fix`). Status never fetches,
so "behind" is as of the last fetch (`sync`, `f` in the TUI, cron);
`fix --fetch` (`-F`) fetches the selected repos first, with `-n` too.

`pull` on a branch that has diverged from its upstream (commits on both
sides) is only automatic when a trial merge (`git merge-tree`) predicts
no conflicts and your config says whether to merge or rebase
(`pull.rebase`, or `pull.ff`); otherwise `fix -n` says why
(`not automatic: would conflict in README`). `sync` fetches such repos
instead of pulling them. Any pull argus runs that still stops with
conflicts is aborted, leaving the repo as it was, autostash included.
`pull` isn't in the default `[fix] auto`.

`stash` saves staged, unstaged and untracked changes (not ignored files);
`git stash pop --index` puts them back as they were. `[stash] untracked =
false` leaves untracked files alone, and `keep_index = true` leaves staged
changes in the index after stashing them.

## Development

```sh
cargo nextest run --workspace     # the tests (~310; real git repos in temp dirs)
cargo clippy --workspace --all-targets
cargo run -p argus-tui --example snapshot -- ~/dev 'jj\t'   # render a frame as text

# after a sizeable batch of work: mutation-test what changed
cargo mutants --in-diff <(git diff <base>) --baseline=skip

# now and then (weekly, before a release)
cargo mutants                                               # everything (hours)
cargo llvm-cov nextest --workspace --html                   # coverage: target/llvm-cov/html
ARGUS_RANDOM_TREES=5000 cargo nextest run --workspace       # more random trees for -L
cargo deny check                  # advisories, licences, duplicate versions, sources
cargo machete                     # unused dependencies
cargo outdated --workspace --root-deps-only
cargo bench -p argus-core -- --save-baseline before         # criterion; after a change:
cargo bench -p argus-core -- --baseline before              # compare (likewise -p argus-tui)
scripts/bench-cli.sh              # hyperfine: whole commands on 200 generated repos (--help)
```

Tools: `cargo install --locked cargo-nextest cargo-llvm-cov cargo-mutants
cargo-deny cargo-machete cargo-outdated` and `brew install hyperfine`, and `rustup component add
llvm-tools-preview` for coverage. Settings: `.config/nextest.toml`,
`.cargo/mutants.toml`, `deny.toml`. Plain `cargo test` works too (nextest
is faster and stops at the first failure). On macOS, list your terminal
under System Settings → Privacy & Security → Developer Tools: otherwise
macOS scans every freshly built test binary before it runs, adding about
30 s after each rebuild.

Tests build real repositories with the `git` CLI under an isolated git config
(`argus_core::testkit`), with local bare repos standing in for GitHub. Each
detector is tested together with its inverse.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in argus
by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.
