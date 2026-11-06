# argus — design notes

argus is a multi-repository overview and cleanup tool. It is "a multi-directory
magit" only in the narrow sense: see many clones at once, spot the ones needing
attention, fix them, and keep everything (including whole forge orgs) up to date.

## 1. Existing options (surveyed 2026-11)

| Tool | Lang | Status overview | Bulk pull | Forge org sync | TUI | Fix actions | Verdict |
|---|---|---|---|---|---|---|---|
| gfold | Rust | ✔ fast, read-only | ✘ | ✘ | ✘ | ✘ | Great scanner, nothing else |
| git-workspace | Rust | ✘ | ✔ fetch | ✔ GitHub/GitLab/Gitea orgs | ✘ | ✘ | Closest on forge sync, no status/fix |
| git-repo-manager (grm) | Rust | partial | ✔ | ✔ | ✘ | ✘ | Config-driven, worktree-centric |
| ghorg | Go | ✘ | ✔ | ✔ clone whole org | ✘ | ✘ | Cloner only |
| ghq | Go | ✘ | ✘ | ✘ | ✘ | ✘ | Directory layout convention only |
| gita | Python | ✔ | ✔ | ✘ | ✘ | ✘ (runs commands) | CLI only, groups |
| mani / myrepos / mrepo | Go/Perl | ✔ | ✔ | ✘ (hand-written config) | mani/mrepo basic | ✘ | Task runners |
| git-dash / RepoNest / git-scope / hrd | Rust/Go | ✔ | some | ✘ / roadmap | ✔ | pull/push only | Dashboards, no org model |
| lazygit / gitui / magit | Go/Rust/elisp | single repo | — | — | ✔ | ✔ | Excellent *per repo*; argus should hand off to them |

None combines: forge-aware org/team membership (auto-detect new repos), sync
flags, favourites/exclusions/snoozes, attention-sorted status, safe auto-fixes,
and a TUI with both arrow-key and transient-style menus. So we build, and
**delegate to existing tools** where they already excel (git for network
mutations and user config, `gh` for auth, lazygit/magit/`$SHELL` for deep work).

## 2. Language: Rust

| Concern | Rust | Python |
|---|---|---|
| TUI | ratatui (immediate mode, fast, huge ecosystem: tui-term for embedded pty) | Textual (excellent; built-in themes + CSS) |
| Git library | gix (pure Rust, parallel status, fast) + git2 | pygit2 (libgit2), GitPython (porcelain wrapper), dulwich (slow) |
| Parallelism | real threads, rayon, no GIL | free-threading is new; libgit2 releases GIL in places |
| Library reuse (CLI/TUI/server) | cargo workspace of crates; server via axum later | packages; fine |
| Distribution | single static binary | venv/uv |
| Testing | cargo test, insta snapshots, ratatui TestBackend | pytest, Textual pilot/snapshots |

Rust wins on performance, parallelism and distribution, which the brief puts
first. Python's real advantage is Textual's theming. ratatui has no theming
system, so argus adds a **small theme layer**. The default `terminal` theme
uses only ANSI palette colours, so **your terminal's own theme applies for
free**. Named built-in themes and per-key overrides come from config.

## 3. Programmatic access vs porcelain ⚠️

The rule: **read with gix, mutate with `git`.**

| Operation | Implementation | Why |
|---|---|---|
| discovery, HEAD, branches, upstream, ahead/behind, default branch, stash count, worktree list, submodule list, in-progress op (rebase/merge…), dirty/untracked counts | **gix** (in-process, parallel) | Pure reads. This is the hot path across hundreds of repos, and spawning processes would dominate. |
| fetch, pull, push, clone | **`git` CLI** | Must honour *your* credentials (credential helpers, ssh-agent, `~/.ssh/config`, `insteadOf`) and *your* pull settings (`pull.rebase`, `rebase.autoStash`, `pull.ff`, hooks). gix has no pull/rebase orchestration and push is incomplete. libgit2 has no `pull` and ignores much of that config. |
| commit, stash push, switch branch | **`git` CLI** | Runs hooks (pre-commit, post-checkout), honours `commit.gpgsign`, LFS/filters, sparse checkout and submodule recursion settings. gix lists these as "needs plumbing". |
| conflict prediction for a diverged branch | **`git merge-tree --write-tree`** (git 2.38+), the one porcelain read | A read-only trial merge that matches what `git pull` will do, including merge settings. It only runs for branches that are ahead *and* behind, which are rare, so the process cost is negligible. gix's tree merge exists but needn't agree with git's. |
| forge listing (org/team repos) | HTTPS to GitHub REST API, token from `GH_TOKEN`/`GITHUB_TOKEN`, falling back to `gh auth token` | Reuses existing `gh` login, no new credential store. Clone protocol defaults to `gh config get git_protocol`. |

Mutations run non-interactively with `GIT_TERMINAL_PROMPT=0` and stdin
closed, so a missing credential fails fast instead of hanging a background job.
If a job does need interaction, drop into a shell for that repo.

## 4. Architecture

```
crates/
  argus-core   library: config, state, discovery, status (gix), ops (git CLI),
               fix planning, sync orchestration, sorting/attention. No UI, no network to forges.
  argus-forge  library: Forge trait + GitHub implementation. Pluggable: GitLab/Gitea later.
  argus-tui    library: ratatui app (state machine separate from rendering, so it's testable).
  argus-cli    binary `argus`: clap subcommands (status, sync, fix, forge, tui). Default = TUI.
```
A future `argus-server` (axum + JSON) would depend on core + forge only.

**VCS abstraction.** Status is a plain data model (`RepoStatus`) produced by
the `git` backend module. Another backend (jj, hg) would produce the same
model. Ops are an enum (`Op`) executed by a backend runner.

**Concurrency.** Status uses rayon over discovered repos, all CPU/disk work in
process. Network ops (fetch/pull/clone/push) run on a bounded worker pool
(default 8, configurable) and stream `Event`s over a channel. CLI and TUI
consume the same events.

## 5. Concepts

* **Roots.** Directories scanned recursively for repos. Nested containers are
  supported, and so are repos nested inside repos (non-submodule). Skips
  `node_modules`, `target`, `.git` internals and configurable globs.
* **Identity.** `host/owner/name` parsed from the `origin` remote URL (or the
  first remote).
* **Marks: `favourite`, `sync`, `exclude`** are lists in config, one per
  kind. An entry is a repo or a glob, matched against identity, `owner/name`
  or path; the last matching entry wins and `!` turns the mark off, as in
  gitignore. Tracked owners' repos sync unless `sync` says `!`. Marking
  from the TUI or CLI drops the repo's own (glob-free) entries, then adds
  its name, or `!name`, only if the patterns left don't already give the
  wanted value; clearing drops its own entries. Changes are written as
  removals and appends with toml_edit, so the rest of the file stays as
  written. They used to live in state, so config stayed hand-edited, but
  argus already edits config for roots, and in config they can be
  versioned and shared; nothing about them is temporary.
* **Snooze.** Temporary exclusion until a date. Kept in the *state* file, not
  config, so config stays hand-edited.
* **Includes and `extend_*`.** Config files are merged as TOML before
  becoming a `Config`: the defaults, then each file after its includes
  (paths relative to the including file, globs in the file name only,
  cycles and missing files are errors). Tables merge key by key, values
  and lists replace, lists of tables (`[[track]]`) add up, and
  `extend_<list>` appends at any level, for every list setting without
  listing them; on a non-list or unknown key it's an error. The merge
  records which file each list element came from, so saving marks can
  remove an entry where it lives and add new ones to the main file
  without overriding an included list. Renamed settings (`skip_dirs`)
  are renamed during the merge.
* **No forgetting.** Nothing about a repo is dropped because scans no
  longer find it (an earlier version dropped marks after 30 days; that
  broke the rule that permanent loss needs confirmation). Expired snoozes
  are removed, as they end by design. Marks an older version kept in state
  are left there and shown as config lines to copy.
* **Tracked owners.** `[[track]] forge="github" owner="acme" [team="platform"] into="~/dev/acme" clone="auto"|"notify"`.
  Sync lists the forge, diffs it against local clones, and clones missing repos
  (`auto`) or flags them as *new on forge* (`notify`). Newly cloned repos are
  marked + new until acknowledged. Tracked owners are implicitly syncable.
* **Attention.** A repo needs attention if it has any of: conflicts or an
  in-progress operation, uncommitted changes, unpushed commits, a non-default
  branch, detached HEAD, stashes, a gone upstream, or no upstream for the
  current branch. Sort order: needing attention before clean, then
  favourites, then severity, then amount of change, then name.
* **Auto-fixes** (planned and shown first, then executed; never destructive):
  * `switch-default`: on a non-default branch, worktree clean, branch fully
    pushed (or merged into default) → `git switch <default>`.
  * `push`: ahead of upstream, nothing behind → `git push`.
  * `stash`: something to stash → `git stash push -m "argus <timestamp>"`
    with the `[stash]` defaults (git has no config for these): untracked
    files (on by default, `-u`) and keep index (off). `git stash pop
    --index` restores each change as it was. Previews simulate the options,
    so the predicted chain stays right. `ops::stash_problem` refuses what
    git can't do cleanly: nothing to stash, or a staged-only stash
    (`--staged`) of a file that also has unstaged changes, which git saves
    but then fails to remove. The TUI's `z` menu has magit-style switches
    (untracked, keep index) that start from `[stash]` and last the session;
    choosing a switch keeps the menu open. Its actions stash everything,
    staged changes only, with a message, or the files marked in the details
    view (Space on a file line; marks on files no longer changed are
    dropped as rows are rebuilt). A refusal in any selected repo stashes
    none. No `argus stash` command: for one repo it
    would only pass git's own options through, and `argus fix -k stash`
    stashes in bulk.
  * `commit`: needs a message (TUI prompt / `--message`) → `git add -A && git commit`.
  * `pull` / `fetch`: plain `git pull` / `git fetch`, so your config decides.

## 6. TUI

* Table, one line per repo: `◆ + name  branch  ↑2 ↓1  ●3 ±1 ?4  $2  ! rebase`.
  Grouping by owner, with folding.
* Enter / Tab unfolds detail: commit and tags, files, branches, stashes,
  worktrees, submodules.
* **Icons:** one struct (`icons::Icons`) holds every symbol the
  interface prints: repo badges, plus the spinner, marked rows, job
  results and the all-clear. A badge is an icon with a count and a colour
  role for one repo; the other icons are used directly. Plain
  punctuation (`icons::PUNCTUATION`: … · — ↑↓) stays as text. A test
  scans string literals in all crates (outside tests) and fails on any
  other symbol, so new ones become icons. The `unicode` set uses
  characters present in common monospace fonts (Menlo, SF Mono,
  Inconsolata, DejaVu, Fira Code, Hack, Source Code Pro), with known gaps
  listed in a test (◆ not in SF Mono, ✓ not in Hack, ✗ not in Fira Code,
  Hack or Source Code Pro) and fallbacks known to fit the cell (braille
  spinner, emoji). A glyph missing from the
  font is drawn from a fallback font and can overlap its neighbour. The
  `nerd` set uses Nerd Font glyphs for every icon, each followed by a
  space, since non-Mono Nerd Fonts draw them up to two cells wide.
  `[ui.icon_overrides]` replaces single icons on top of either set, for
  fonts with better glyphs (e.g. ★); unknown names warn.
* **Detached on a tag** shows the tag (annotated first, then highest
  version by numeric comparison, `+N` for the rest). Tags on HEAD are
  found from packed refs' peeled ids, so this costs no object reads
  except for loose annotated tags.
* **`argus demo`** builds sample repos in every state (a test checks the
  list against every issue, kind and flag) in a temp dir, with git
  isolated from the user's config, and opens the TUI with the user's
  theme and icons. `v t` / `v i` cycle theme and icons for the session.
  `--glyphs` prints a test strip instead (no terminal needed): whether a
  fallback glyph overlaps depends on the terminal and OS, so it has to be
  seen, not computed from font tables.
* Two input styles at once:
  * arrows/Enter plus menus you can navigate with the cursor;
  * transient-style prefix keys: `f` fix, `s` sync, `m` mark, `o` open, `?` help.
    One more key executes. `C-g`/Esc cancels.
  * emacs (`C-n C-p C-v M-v M-< M->`, `C-s` search) and vi (`j k g G /`) motions.
* `!` drops to `$SHELL` in the repo (TUI suspended), and `o g` opens the
  configured git UI (lazygit/`emacsclient -e (magit-status)`). An embedded pty
  pane (tui-term) is planned.

## 7. Testing

Real repositories in temp dirs, built with `git` (fixture builder), with local
bare "remotes" for network operations. That gives no mocks for git
semantics and makes tests deterministic. Each detector has a positive and
inverse test (e.g. "clean repo reports no attention"). Corner cases include
unborn HEAD, detached HEAD, no remote, upstream gone, worktrees, submodules,
nested repos, bare repos and symlink loops. The forge uses a fake `Forge` impl
plus JSON fixture parsing tests. The TUI state machine is unit-tested, and
rendering is snapshot-tested via `TestBackend`.

## 8. Decisions made during the build

* **Permanently destructive operations ask first** (project rule). TUI:
  `Mode::Confirm`, a `y/n` in the status line; any key but `y` cancels.
  CLI: a `[y/N]` prompt when stdin and stderr are terminals; `-y`/`--yes`
  proceeds; otherwise the command refuses before changing anything. Each
  such operation is tested declined, refused and confirmed. Recoverable
  operations (stash, commit, switch, fast-forward, pull, push) don't ask.
  Clearing marks asks when more than one repo has entries of its own in
  the favourite/sync/exclude lists; snoozes and "new" flags don't count. `root remove` doesn't ask.

* **"Unpushed" means "on no remote-tracking ref"**, not "ahead of upstream".
  On real data, a fork branch that tracked `upstream/master` but was pushed
  to `origin/<branch>` was flagged as unpushed. Worse, a `push` fix was
  offered that would have pushed to the upstream owner's repo. `push` now
  requires a same-named upstream. `↑n` still shows git's ahead count.
* **Untracked collapsing is done by argus.** gix's collapsed directory walk
  reported a directory holding only ignored files and empty directories as
  untracked (seen in an Xcode project). We ask gix for individual files and
  collapse to the outermost directory with no tracked content, matching
  `git status`.
* **Linked worktrees don't repeat branch issues** (branches are shared; the
  main checkout reports them). **Submodules don't report detached HEAD**:
  being detached at the pinned commit is normal, and drift shows as a
  modification in the parent.
* **Detached HEAD commits held by no ref** (garbage-collection risk) are
  their own high-severity issue.
* **Shell/git-UI hand-off suspends the TUI**, as lazygit does. An embedded pty
  pane (portable-pty + tui-term) is the next step for "contained within the TUI".

* **One fix per auto-fix action.** No chaining (push *then* switch is two
  actions); the user can snooze or tolerate in between. A future
  "auto-fix all" may chain. Defaults: `push` (fast-forward, same-named
  upstream only) and `switch-default`.
* **The order of `[fix] auto` (or `-k`) is the priority.** Each action
  applies the first listed kind the repo needs; unlisted kinds rank after,
  in `FixKind::ALL` order: clean up uncommitted work (stash, commit), share
  commits (push, publish), leave the branch (switch-default), then catch
  up (pull, fast-forward) on the branch you end up on. "Behind" comes from
  the remote-tracking refs as of the last fetch: status never fetches
  (`f`/`F` in the TUI, `argus sync` or cron do).
* **Pulls are made safe rather than avoided.**
  * A diverged branch gets a conflict prediction (`merge-tree`, above) and
    a check of `pull.rebase`/`branch.<b>.rebase`/`pull.ff`: without them
    git refuses to reconcile, and `pull.ff = only` always refuses. The
    pull fix is shown but *blocked* (never chosen) when it would conflict
    or be refused, or when tracked files are dirty (autostash's re-apply can
    itself conflict).
  * `sync` fetches instead of pulling such repos, and repos mid-operation.
  * `Op::Pull` refuses to start during an operation in progress, and
    aborts (`git merge|rebase --abort`) a pull that stops with conflicts,
    which also restores any autostash: an automatic pull completes or
    leaves the repo as it found it.
  * A merge commit never waits for an editor: git only edits when on a
    terminal, which argus never gives it, and `GIT_MERGE_AUTOEDIT=no` is
    set as well.
  * A clean trial merge doesn't guarantee a clean rebase; the abort covers
    that case.
* **Previews separate the chain from the alternatives.** `fix::simulate`
  predicts the status after a fix (pure, no git), and `fix::chain`
  repeats choose+simulate to give what successive auto-fix actions would
  do. Tests compare each predicted chain with repeatedly applying fixes to
  a real repo. `fix --dry-run` and the TUI details show the next fix, the
  rest of the chain (`then`), and other fixes available now
  (`alternative`, with a `-k` hint). If chaining in one action ever
  arrives, the same chain is what it would apply.

* **No `init`.** Without configured roots the current directory is
  scanned (with a hint). The config file is created on first run as a
  fully commented example generated from the code: the JSON schema
  (schemars) supplies doc comments, `Default` values and `example`
  annotations, so the documentation can't drift from the code. Roots
  are managed with `argus root add|remove|list` (toml_edit, so comments are kept).
* **`max_depth` is reported**, not silent: a scan names a directory it
  didn't enter. `max_depth = 0` means no limit (0 as "the root only" had no
  use: the walk stops at a repo anyway).
* **Automatic concurrency** (`concurrency = 0`, the default): git's
  `fetch.parallel` when set above 0 (git's own 0 also means "pick a
  default"), else the CPU count capped at 8. The work is network-bound,
  and OpenSSH allows 10 sessions per multiplexed connection by default,
  so more CPUs don't mean more useful parallelism.
* **Discovery syscalls:** per directory, one `lstat` of `.git` (settles
  the common case, a work tree) and otherwise one listing, which also
  serves bare-repo detection (`HEAD`, `objects/`, `refs/`) and yields the
  subdirectories with their types (no per-entry stat).
* **Hidden directories are skipped** (`skip_hidden = true`): in a home
  directory they're mostly caches, so this avoids a long exclude list.
  Roots are always scanned, so a dot directory can be added as a root.
* **`skip`**: one list, gitignore-style: no `/` means a directory name at
  any depth, a leading `/` or `~` a full path, any other `/` a path below
  each root. Checked on each subdirectory from the listing (no extra
  syscalls); roots are always scanned, like hidden directories.
* **`skip` and symlinks.** Rule: a directory is skipped if any spelling of
  it matches, wherever it's reached. A path pattern's literal part (all
  of it, or up to the directory before the first glob) is resolved once,
  following every link. Walks start from real roots, so without `-L`
  every walked path is real and that's the whole story. With `-L`, a
  directory below k followed links has k + 1 spellings (as walked, then
  with each link resolved in turn; the last is real), built from the
  chain of links the walk already keeps for cycle detection, so no extra
  syscalls; a link also matches by its target's path and name. A
  wildcard spanning a link can match only one route's spelling, so the
  real paths of skipped directories are recorded and repos inside them
  dropped after the walk, unless a root is at or inside that directory.
  Not covered: symlinks the walk doesn't follow (without `-L` those
  directories aren't scanned anyway).
* **`-L` performance.** Two optimizations, each switchable (`Tuning`) and
  checked against the plain walk on random trees:
  * Found repos' real paths come from the link chain, not a
    `canonicalize` each. Exact: roots and link targets are canonical, and
    the walk enters nothing else that could be a link.
  * A link isn't followed when the walk without links reaches its target
    anyway and decides everything below it the same way: the target is
    under a root, at the same depth or shallower; every directory on the
    way is listed by its parent and isn't hidden, `.git`, skipped or a
    repo (which stops the walk); no path pattern could match below any
    spelling of the link (its literal part is checked against them); and
    the link is above `max_depth` (at the limit it still counts towards
    truncation). Same repos; a repo reachable only through further links
    below such a target may be spelled by another of its paths, and
    truncation reports through the link are dropped.
  On a synthetic tree (15,789 directories, 2,000 repos, 91 links: aliases,
  chains, loops) with name skips, `-L` went from 161 ms to 40 ms, against
  35 ms without `-L` (`cargo run --release -p argus-core --example
  discover`).
* **Symlinks** are not followed unless asked (`-L`, `follow_symlinks`).
  When followed, cycles are cut by remembering the real directory at each
  link taken on the way down; a link whose target contains one of them
  would walk in a circle. A repo reached by several paths is listed once,
  by its real path if that was walked, else the first path in sort order.
* **`argus status` lists only repos needing attention**; `--all` adds clean
  ones, `--hidden` excluded/snoozed ones, and the footer counts what was
  left out. `--json` uses the same filter. The TUI starts the same way
  (`[ui] attention_only = true`); the header says how many repos there are
  in all and that `a` shows them.
* **`fix --fetch`** fetches only the selected repos (once per family, as
  worktrees share refs), then refreshes their status before planning.
  Fetch failures are reported and planning continues with what is known.

## 9. Not done yet / next steps

* Embedded terminal pane; per-file actions in the details view (stage,
  discard, diff); deleting merged branches; worktree pruning.
* Orphans (local clones the forge no longer lists): reported only, no
  archive/move action.
* GitLab/Gitea forges (implement `argus_core::forge::Forge`); jj backend.
* `argus-server` (axum, JSON over the same `Workspace`).
* Filesystem watching for live refresh. Today: `g`, and after each operation.
* Stable machine-readable output. `--json` works for every command; sync,
  fix, mark, ack, root and paths build their documents explicitly in the
  CLI, but `status` and `forge list` still serialise internal structs
  (`Entry`, `RemoteRepo`), which isn't a stable contract. Consider a
  versioned schema and/or a line-oriented `--porcelain` format (like
  `git status --porcelain=v2`).
* Stash options: let the user choose whether untracked files are included
  (git has no config for this). Today: staged + unstaged + untracked.
* Path subsets for operations (stash/commit/discard selected files), e.g.
  from the details view.
* Completing repo patterns (names, keys) in shell completions; today
  `argus completions <shell>` covers commands and options only.
* Python bindings to `argus-core` (pyo3/maturin), for scripting over the
  same workspace, status and fix planning.
