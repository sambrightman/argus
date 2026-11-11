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
| one file's diff (unstaged: index → work tree; staged: HEAD → index) | **gix** blob diff | Content compared as git stores it (`Mode::ToGit`), so `core.autocrlf` and other filters apply; binary detection as git's. Lines keep their endings when diffed, as in git, so a lost final newline shows. A test checks hunks against `git diff` for several shapes of change. textconv isn't applied. |
| stage, unstage, discard files | **`git` CLI**: `add`, `restore`, `rm`, `clean` | Mutations. Unstaging a newly added file uses `rm --cached`, which also works before the first commit. Discard asks first (permanent). |
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
               fix planning, pull orchestration, sorting/attention. No UI, no network to forges.
  argus-forge  library: Forge implementations: GitHub, and `dir` (a directory of owners' repos: mirrors, file servers; tests use it offline). Pluggable: GitLab/Gitea later.
  argus-tui    library: ratatui app (state machine separate from rendering, so it's testable).
  argus-cli    binary `argus`: clap subcommands (status, pull, fix, forge, tui). Default = TUI.
```
A future `argus-server` (axum + JSON) would depend on core + forge only.

**VCS abstraction.** Status is a plain data model (`RepoStatus`) produced by
the `git` backend module. Another backend (jj, hg) would produce the same
model. Ops are an enum (`Op`) executed by a backend runner.

**Concurrency.** Status uses rayon over discovered repos, all CPU/disk work in
process, on a pool of `concurrency.disk` threads (default the performance
cores, else half the CPUs); discovery uses the same pool.
Network ops (fetch/pull/clone/push) run on a bounded worker pool
(default 8, configurable) and stream `Event`s over a channel. CLI and TUI
consume the same events.

**Where status time goes** (profiled 2026-11 with Instruments on 200 packed
repos, `scripts/bench-cli.sh`): over 80% in the kernel, mostly `open()`.
Two findings:
- Ref lookups by name each try a loose file (usually missing, as clones
  pack refs) and then packed-refs. Status now reads branches, remote
  branches and tags once per repo and answers from that (status 26–31%
  faster in criterion).
- File system calls from many threads slow each other down in the kernel.
  With rayon's default of one thread per logical CPU (10 on an M-series
  with 4 performance cores), kernel time rose 4.5× from 1 to 10 threads,
  and wall time stopped improving between 4 and 6. Half the CPUs halves the
  kernel time at the same wall time. Discovery is sharper still: on 2,000
  repos and 18,000 directories it was fastest with 4 threads (61 ms), 30%
  slower with 5 and 40% slower with 10, with kernel time tripling. Threads
  beyond the performance cores land on efficiency cores and hold everyone
  up, so the default is the performance-core count (`sysctl` crate, which
  calls the API, not the command), else half the CPUs. gix starts two threads per repo's status
  (HEAD vs index, index vs worktree), plus up to one per CPU to check files
  in large repos; limiting those made no difference on small repos.
- Opening a repo was 29% of CPU: mostly reading config files (each
  repo's own `.git/config`, plus the global ones again for every repo),
  then checking the path is a repo and looking up the current directory.
  The global config made no measurable difference to wall time. The TUI
  keeps repos open between refreshes (`KeptRepos`): 58 → 36 ms per refresh
  of 200 repos, 295 → 184 ms for 1,000, with no files left open. gix reads
  config once per open by design, and re-checks refs, the index and packs
  itself. Before reuse, a kept repo's files are compared with what they
  were at open (inode, size and time: a stat each, not OS watches, so no
  limits): the files its config came from, the places global and system
  config could appear, HEAD when a section depends on the branch, the index
  (gix re-reads it only when its time is newer, which a same-second
  rewrite on a coarse-time file system would beat) and the pack directory
  (gix rescans packs only when an object is missing, which is correct as
  objects never change, but keeps packs a gc deleted mapped). The checks
  cost about 1 ms per refresh of 200 repos. Missed: an included file that
  didn't exist at open. The CLI opens each repo once anyway and doesn't
  keep them.
- gix assumes the system config is `/etc/gitconfig` unless allowed to ask
  git (`git_binary`), so it never read e.g. Homebrew's
  `/opt/homebrew/etc/gitconfig`. argus now asks: one `git config -l` per
  process, plus reading that file for each repo, costs 5.5 ms on 200 repos
  (+9%) and 17 ms on 1,000 (+5%). Starting the query during discovery
  didn't hide it.

## 5. Concepts

* **Roots.** Directories scanned recursively for repos. Nested containers are
  supported; the walk stops at a repo, unless `scan_inside_repos` asks it to
  look for repos nested in work trees (submodules come from their parent's
  status either way). Skips
  `node_modules`, `target`, `.git` internals and configurable globs.
* **Identity.** `host/owner/name` parsed from the `origin` remote URL (or the
  first remote).
* **Marks: `favourite`, `pull`, `ignore.repos`** are lists in config, one per
  kind. An entry is a repo or a glob, matched against identity, `owner/name`
  or path; the last matching entry wins and `!` turns the mark off, as in
  gitignore. Names: `ignore.repos` hides a repo and leaves it out of
  pulls, clones and bulk actions; *hidden* is the wider set, ignored or
  snoozed. The other `[ignore]` lists turn off one issue (non-default-branch,
  detached, no remote, no upstream, stashes, unpushed branches, behind):
  ignoring a whole repo is the special case of ignoring everything about
  it, so one section holds both. `repo@value` pins the branch, tag,
  describe or commit (glob allowed), and the TUI and CLI write the pinned
  form, so a repo that moves is flagged again: an expectation, not
  silence. Ignored issues don't count towards attention, show dimmed in
  the details, and turn off the auto-fix that would undo them
  (`fix::turned_off_by`).
* **Stashes are ignored one by one, and argus's own aren't by default.**
  A detached HEAD's tags, describe and commit are names for one thing, so
  an entry matching any of them covers it; stashes are separate things, so
  each is checked against the list by itself (last match wins, per stash)
  and the issue counts those left. The default, `["**", "!**@argus *"]`,
  is plain config rather than a special case: your own stashes are
  deliberate, the stash fix's are work argus tucked away for you. A pin
  matches the message after git's `On <branch>: ` (the part you wrote);
  pins are written as literals (`globset::escape`) and only for stashes
  not already covered. Ignoring from the TUI or CLI doesn't pin stashes by
  default (new stashes stay ignored) but does pin where HEAD is; `P` /
  `--pin` / `--no-pin` choose the other way. An entry splits at its first
  `@`, as a message may hold another.
* **A list with a default is added to with `extend_`.** When no file has
  the list yet, the TUI and `argus mark` write `extend_<list>`, since a
  plain list replaces the default.
* **Ignoring hides warnings, never facts.** A badge stating a fact (the
  behind count, the stash count, where HEAD is) shows whether or not its
  issue is ignored; one that only warns (no upstream, unpushed branches)
  comes from the issues still counted, so ignoring removes it. Conflicts, operations in progress and commits
  only a detached HEAD holds can't be ignored: they risk losing work.
  `exclude_paths` is the other kind of exclusion: directories discovery
  never enters, matched by name or path, as rsync's and ripgrep's
  `--exclude` mean it. Tracked owners' repos are pulled unless `pull` says `!`. Marking
  from the TUI or CLI drops the repo's own (glob-free) entries, then adds
  its name, or `!name`, only if the patterns left don't already give the
  wanted value; clearing drops its own entries. Changes are written as
  removals and appends with toml_edit, so the rest of the file stays as
  written. They used to live in state, so config stayed hand-edited, but
  argus already edits config for roots, and in config they can be
  versioned and shared; nothing about them is temporary.
* **Snooze.** Temporary exclusion until a date. Kept in the *state* file, not
  config, so config stays hand-edited.
* **`extend` and `extend_*`.** Config files are merged as TOML before
  becoming a `Config`: the defaults, then each file after the files it
  extends, so a file overrides its bases wherever its `extend` line is
  (as ruff's `extend`, tsconfig's and ESLint's `extends`; not git's
  positional `include`). Named `extend` rather than `include` for that
  reason: "include" suggests pasting the file in at that point. (Paths
  relative to the extending file, globs in the file name only,
  cycles and missing files are errors). Tables merge key by key, values
  and lists replace, lists of tables (`[[track]]`) add up, and
  `extend_<list>` appends at any level, for every list setting without
  listing them; on a non-list or unknown key it's an error. The merge
  records the order files were merged in. Roots and the mark
  lists are edited by one rule, chosen so an addition always takes
  effect: append to the highest-precedence file with the list or its
  `extend_` form, into `extend_` if it has both (it applies last, so a
  `!` entry added there wins over the file's own patterns); with
  neither anywhere, start the list in the main file (`roots` at the
  top). Nothing above that file sets the list, so the addition counts;
  and when a shared base sets a list that the main file extends,
  additions stay in the main file (an empty `extend_favourite = []`
  there steers them). Precedence: the main file, then a later extended
  file before an earlier one, a file before the files it extends
  (`Layers.files` reversed). Removal takes an entry out of every file
  listing it; `root remove` also reports removals that changed nothing
  because a higher `roots` replaces that file's. Every file is checked
  as writable before any changes, so a failure leaves them all as they
  were. Old setting names aren't kept
  (nothing released yet): they fail as unknown keys.
* **Describing a detached HEAD.** With no tag on HEAD, the branch slot
  shows `git describe --tags` (gix's, same algorithm). gix's describe
  can't be bounded, and with no tag in history it walks all of it:
  measured on packed repos, 2.4 ms per 1,000 commits without a
  commit-graph file, 0.15 ms with one (50,000 commits: 121 ms, 7.9 ms).
  So a bounded walk (`describe_limit`, 1,000 commits: about 2.4 ms at
  worst) first checks that a tagged commit is in reach; no tags at all
  costs nothing. A walk that runs out of history first is a final "no
  tag"; one that stops at the limit is recorded (`describe_gave_up`), and
  opening the repo's details runs the unbounded describe in the
  background (`describe_fully`), as diffs are computed on demand.
  Answers are remembered per repo, HEAD and tags (names included), with
  the limit a walk gave up at, so a longer walk isn't answered by a
  shorter one; a full answer is final, so the list's badge picks it up
  and refreshes don't repeat it.
  Benchmark: `describe, no reachable tag` in `benches/core.rs`.
* **Diff view.** Tab on a file in the details shows its unstaged then
  staged hunks (from `diff::file_diff`, cached per file and side, dropped
  when the repo's status changes); Tab on a hunk folds it. The cursor
  keeps its row across rebuilds (a folded hunk's lines move it to the
  header). Changed words are found as magit does: each run of removed
  lines and the added run after it are split into words and diffed, here
  with `similar`; changed words are drawn reversed, which works in every
  theme. `[ui] refine` picks all hunks shown, the cursor's, or none.
* **File operations** (`s` `u` `K`) act on the marked files, else the file
  under the cursor, planned per file by `ops::stage_ops` / `unstage_ops` /
  `discard_ops` and run as jobs (one family per repo, so in order).
  Discard always asks, naming the file or the count. Hunk and line
  operations would build on the same diff rows.
* **No forgetting.** Nothing about a repo is dropped because scans no
  longer find it (an earlier version dropped marks after 30 days; that
  broke the rule that permanent loss needs confirmation). Expired snoozes
  are removed, as they end by design. Marks an older version kept in state
  are left there and shown as config lines to copy.
* **Tracked owners.** `[[track]] forge="github" owner="acme" [team="platform"] into="~/dev/acme" clone="auto"|"notify"`.
  `argus pull` lists the forge, diffs it against local clones, and clones missing repos
  (`auto`) or flags them as *new on forge* (`notify`). Newly cloned repos are
  marked + new until acknowledged. Tracked owners' repos are pulled by default.
* **Attention.** A repo needs attention if it has any of: conflicts or an
  in-progress operation, uncommitted changes, unpushed commits, a non-default
  branch, detached HEAD, stashes, a gone upstream, or no upstream for the
  current branch. Sort order: needing attention before clean, then
  favourites, then severity, then amount of change, then name.
* **Auto-fixes** (planned and shown first, then executed; never destructive:
  the destructive kinds, `abort` and `delete-branches`, are confirmed
  first in the TUI and never chosen automatically):
  * `switch-default`: on a non-default branch, worktree clean, branch fully
    pushed (or merged into default, if only locally) → `git switch <default>`.
  * `push`: ahead of upstream, nothing behind → `git push`. When `git
    push` would go to a branch other than the upstream and that's the
    same-named one (status's `own`: `push.default` `current` on a branch
    that pulls from `main`) → `git push <remote>
    refs/heads/<b>:refs/heads/<b>`, explicit so `push.default` can't
    redirect it. Nothing when `git push` would go to a differently named
    branch (`upstream` to `main`) or refuse (`simple`). Ahead *and*
    behind its remote branch (rewritten after pushing) → shown, blocked:
    it needs force.
  * `publish`: no upstream → `git push --set-upstream`; upstream gone with
    unpushed work → the same; `own` branch not on the remote yet → the
    explicit push above, leaving the upstream alone.
  * `push-branches`: other branches, the same as `push`, to remote
    branches that exist (never created). One fix per branch.
  * `reattach`: detached HEAD at a branch tip (status's `at_head`) →
    `git switch <branch>`; the default branch if it's there, else the only
    one, else blocked. Not in submodules.
  * `stash`: something to stash → `git stash push -m "argus <timestamp>"`
    with the `[stash]` defaults (git has no config for these): untracked
    files (on by default, `-u`) and keep index (off). `git stash pop
    --index` restores each change as it was. Previews simulate the options,
    so the predicted chain stays right. `ops::stash_problem` refuses what
    git can't do cleanly: no commit yet, nothing to stash, or a
    staged-only stash (`--staged`) git would save but then fail to take
    out of the work tree. git does that by reverse-applying the HEAD →
    index patch (one line of context) to the work tree, which fails where
    a file's unstaged changes overlap its staged ones; the check runs the
    same patch through `git apply -R --check` (plumbing `diff-index`, as
    `git diff` follows settings like `diff.noprefix`), only for files with
    both, and a test checks it against git's own result. The TUI's `z` menu has magit-style switches
    (untracked, keep index) that start from `[stash]` and last the session;
    choosing a switch keeps the menu open. Its actions stash everything,
    staged changes only, with a message, or the files marked in the details
    view (Space on a file line; marks on files no longer changed are
    dropped as rows are rebuilt). A refusal in any selected repo stashes
    none. No `argus stash` command: for one repo it
    would only pass git's own options through, and `argus fix -k stash`
    stashes in bulk.
  * `commit`: needs a message (TUI prompt) → `git add -A && git commit`.
  * `branch`: detached HEAD with commits on no branch → `git switch -c
    <name>`, the name asked for (TUI `b c`).
  * `add-remote`: no remote → `git remote add -- origin <url>`, the URL
    asked for (TUI `P r`, one repo at a time).
  * `abort`: an operation in progress → `git <op> --abort` (`git bisect
    reset`). The only fix offered then. Confirmed first: in the TUI, and in
    the CLI (`argus fix -k abort`) after listing, or with `--yes`. Unlike
    the one-per-run auto-fixes, the CLI applies every one listed.
  * `delete-branches`: other branches merged into the default branch,
    upstream gone or none → `git branch -d`, which refuses what it doesn't
    see as merged. Confirmed first.
  * `stash-pop`: stashes, no tracked changes → `git stash pop --index`,
    after `git merge-tree` of the stash into HEAD finds no conflicts
    (checked when it runs). Turned off for auto-fix while any stash is
    ignored. Dropping a stash isn't a fix (nothing in status says which):
    the TUI drops the one under the cursor, confirmed, and only if
    `stash@{n}` still has the id status read (`stash_ids`).
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
  * transient-style prefix keys: `f` fix, `F` pull, `m` mark, `o` open, `?` help.
    One more key executes. `C-g`/Esc cancels.
  * emacs (`C-n C-p C-v M-v M-< M->`, `C-s` search) and vi (`j k g G /`) motions.
  * Menus of choices have only item keys. Menus that list things (the
    commits to reset to) also move with `n p j k`, so their items are
    keyed from `menu::LIST_KEYS`, which leaves those out (and `q`): every
    future list must use it, or a key meant to move would pick an item.
    A key for an item scrolled out of sight only scrolls to it.
* `!` drops to `$SHELL` (or `tools.shell`) in the repo (TUI suspended), as a
  login shell, as terminals start one: `argv[0]` is `-bash` (for
  `/opt/homebrew/bin/bash`), so it reads `~/.bash_profile`, `~/.zprofile`…
  Without that it was an interactive non-login shell, reading only
  `~/.bashrc`, and set-ups living in the profile looked broken. `o g` opens the
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

* **Properties** (proptest): refining never changes a line's text; random
  key sequences keep the TUI's cursor on a row and always lead back to
  normal mode; `extend_` appends; version order is a total order; URL and
  duration parsing never panic. The discovery optimizations are checked
  against the plain walk on random trees.
* **Mutation testing**: while developing, targeted mutations of each new
  condition (the ways it could plausibly be wrong) must each fail a test;
  survivors become tests, or show code to delete. cargo-mutants adds
  breadth with generic mutations (`--in-diff` for changes).
* **Coverage** (cargo-llvm-cov, region and line): about 93% overall.
  Effects are carried out by `Runtime`, separate from the terminal loop,
  so tests drive it (`settle` waits for background work): saving marks
  and state and reporting failures, scans, refreshes, jobs, fixes, a pull
  and clone against a fake forge cloning for real, the URL opener (configurable, a
  recording script in tests), and running suspended programs. What's left
  uncovered in `argus-tui/src/lib.rs` is the terminal loop itself. Branch
  coverage needs a nightly toolchain.
* **Benchmarks** (criterion, `benches/`): discovery variants including
  `-L` and its optimizations, status and scanning, a large file's diff,
  config extending other files, rule matching, word refinement, rebuilding and
  drawing the TUI. For before/after comparisons with baselines; timing
  isn't gated, as laptop noise makes that unreliable.

## 8. Decisions made during the build

* **Permanently destructive operations ask first** (project rule). TUI:
  `Mode::Confirm`, a `y/n` in the status line; any key but `y` cancels.
  CLI: a `[y/N]` prompt when stdin and stderr are terminals; `-y`/`--yes`
  proceeds; otherwise the command refuses before changing anything. Each
  such operation is tested declined, refused and confirmed. Recoverable
  operations (stash, commit, switch, fast-forward, pull, push) don't ask.
  Clearing marks asks when more than one repo has entries of its own in
  the favourite/pull/ignore.repos lists; snoozes and "new" flags don't count. `root remove` doesn't ask.

* **"Unpushed" means "on no remote-tracking ref"**, not "ahead of upstream".
  On real data, a fork branch that tracked `upstream/master` but was pushed
  to `origin/<branch>` was flagged as unpushed. Worse, a `push` fix was
  offered that would have pushed to the upstream owner's repo. `push` now
  requires a same-named upstream. `↑n` still shows git's ahead count.
  Being merged into the default branch doesn't make work pushed: when the
  remote has no default branch yet (just added, say), that's the local one,
  and everything merged into it was counted as safe, the default branch
  itself included. `switch-default` still accepts locally merged work, since
  switching to the branch that holds it hides nothing. Without a remote,
  other branches aren't flagged as unpushed: `no remote` says it once.
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
  "auto-fix all" may chain. Defaults: `push` (fast-forward, to the
  branch's own name only), `push-branches` (the same for other branches,
  existing remote branches only), `switch-default` and `reattach`.
* **The order of `[fix] auto` (or `-k`) is the priority.** Each action
  applies the first listed kind the repo needs; unlisted kinds rank after,
  in `FixKind::ALL` order: clean up uncommitted work (stash, commit), share
  commits (push, publish), leave the branch (switch-default), then catch
  up (pull, fast-forward) on the branch you end up on. "Behind" comes from
  the remote-tracking refs as of the last fetch: status never fetches
  (`f`/`F` in the TUI, `argus pull` or cron do).
* **Shell completion** uses clap_complete's dynamic engine (the
  `unstable-dynamic` feature): the shell code asks `argus` for candidates
  (`COMPLETE=<shell> argus -- <words>`), so repo patterns complete with
  names from discovery plus each repo's remote, no status (~10 ms on
  `~/dev`). The interface between shell code and binary may change
  between versions, hence sourcing it fresh at shell startup.
* **Pulls are made safe rather than avoided.**
  * A diverged branch gets a conflict prediction (`merge-tree`, above) and
    a check of `pull.rebase`/`branch.<b>.rebase`/`pull.ff`: without them
    git refuses to reconcile, and `pull.ff = only` always refuses. The
    pull fix is shown but *blocked* (never chosen) when it would conflict
    or be refused, or when tracked files are dirty (autostash's re-apply can
    itself conflict).
  * `argus pull` fetches instead of pulling such repos, and repos mid-operation.
  * `Op::Pull` refuses to start during an operation in progress, and
    aborts (`git merge|rebase --abort`) a pull that stops with conflicts,
    which also restores any autostash: an automatic pull completes or
    leaves the repo as it found it.
  * A merge commit never waits for an editor: git only edits when on a
    terminal, which argus never gives it, and `GIT_MERGE_AUTOEDIT=no` is
    set as well.
  * A clean trial merge doesn't guarantee a clean rebase; the abort covers
    that case.
  * The prediction also says whether git would merge or rebase
    (`branch.<b>.rebase` over `pull.rebase`; `merges` and `interactive`
    count as rebase; `pull.ff` alone merges), so previews count the merge
    commit a merging pull adds: the push that follows has one more commit.
    A rebase is assumed to keep every commit (one already upstream would
    be dropped; not predicted).
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
* **`one_file_system` (`-x`) is off by default and not reported.** Each
  root's own filesystem is the boundary (as `find -xdev`), for
  directories and link targets alike. It costs an `lstat` per
  subdirectory, only when on. Unlike `max_depth`'s cut-off, skipping a
  mount is the point of turning it on, so nothing is warned about. Unix
  only (std has no stable volume id on Windows). Tested on macOS with a
  disk image attached as an ordinary user; Linux would need a user
  namespace to mount, so it has no test yet.
* **Automatic network concurrency** (`concurrency.network = 0`, the default): git's
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
* **`exclude_paths`** (directories never scanned; was `skip`): one list, gitignore-style: no `/` means a directory name at
  any depth, a leading `/` or `~` a full path, any other `/` a path below
  each root. Checked on each subdirectory from the listing (no extra
  syscalls); roots are always scanned, like hidden directories.
* **`exclude_paths` and symlinks.** Rule: a directory is skipped if any spelling of
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
  ones, `--hidden` ignored/snoozed ones, and the footer counts what was
  left out. `--json` uses the same filter. The TUI starts the same way
  (`[ui] attention_only = true`); the header says how many repos there are
  in all and that `a` shows them.
* **`sync` is now `pull`** (command, `pull` list and `[[track]] pull`,
  `mark --pull`, the `pull` icon): it pulls, and clones new repos of
  tracked owners, but never pushes, which "sync" suggested. Cloning stays
  part of it (`--no-clone` to skip), so one command serves cron. Old names
  are errors, except the state file's `last_sync`, read as `last_pull`.
* **`fix --fetch`** fetches only the selected repos (once per family, as
  worktrees share refs), then refreshes their status before planning.
  Fetch failures are reported and planning continues with what is known.

## 8a. Build and test setup

Measured locally (macOS, 10 cores, stable 1.99), compiling the workspace's
tests without running them:

* Full build 19 s, incremental after editing a core file 2.8 s. The
  parallel front end (nightly `-Zthreads=8`) gave nothing (19.4 s, 3.2 s):
  it speeds up big crates, and ours are small. Optimizing dependencies in
  dev builds (`opt-level = 2` for `"*"`) didn't speed the tests up.
* **macOS scans each newly linked binary the first time it runs**
  (XProtect, about 1 s per binary, queued). With ~25 test binaries that's
  30–36 s after every rebuild, by far the largest cost in the edit-test
  loop, and in every cargo-mutants mutant. Listing the terminal under
  System Settings → Privacy & Security → Developer Tools exempts what it
  starts (`spctl developer-mode enable-terminal` shows that pane).
  Integration tests are one binary per crate (`tests/it/`, a module per
  former file): 7 test binaries instead of 19, which took a rebuild after
  an edit from 44 s to 18 s with the scan still in place.
* Debug info is already split, unpacked (Cargo's default on macOS: no
  `dsymutil`). `debug = "line-tables-only"`: full build 17 s instead of
  21 s and 15% less disk, incremental unchanged (3.0 s); not worth losing
  variables in the debugger locally, worth it in CI.
* The linker is Apple's ld-prime; mold and wild only link Linux binaries.
* With binaries already scanned: `cargo test` 24.5 s, cargo-nextest
  20.5 s. The tests are CPU-bound (they spawn git), and the slowest ones
  (random discovery trees, building the demo repos, 7–9 s each) set the
  floor.
* Coverage (`cargo llvm-cov nextest`): 36 s, about 15 s more than a plain
  run.
* `cargo mutants --in-diff --test-tool nextest --jobs 4`: a typical change
  (14 mutants) took 4.6 min with the scan still applying, 3.5 min
  without it: about half fixed costs (the baseline, and each job's first
  build from scratch, competing), then 2–10 s build and 12–33 s test per
  mutant. `--baseline=skip` saves the baseline (~30 s) when the tests
  just passed; a full run (~1,700 mutants) is about 3 h.

**CI** (GitHub Actions, `.github/workflows/`). Different trade-offs from
local work: incremental off (`CARGO_INCREMENTAL=0`: its caches are large
and rarely reused), line tables only for debug info, dependencies cached
with `Swatinem/rust-cache`, and mold as the Linux linker.

* `ci.yml`, on every push to main and every PR: rustfmt, clippy and
  rustdoc with warnings as errors, and cargo-machete; the tests with
  nextest's `ci` profile (no fail-fast, two retries, JUnit uploaded) plus
  doctests, on Linux and macOS; a check on the MSRV (`rust-version`,
  1.90); cargo-deny; coverage with cargo-llvm-cov (summary on the run's
  page, lcov uploaded as an artifact and to Codecov for history); and `cargo mutants --in-diff` on what the push or
  PR changed, failing on a missed mutant.
* `mutants.yml`, weekly and on demand: the full cargo-mutants run in 16
  shards on each of Linux and macOS, `--in-place` on the cached build and
  skipping the baseline (CI has just run the tests). cargo-mutants doesn't
  evaluate `#[cfg]` (its issues #50 and #623; a `cfg_attr` skip applies on
  every platform), so code built only on the other platform always looks
  untested. The summary job therefore counts a mutant only if both
  platforms missed it, lists those and any timeouts, and fails if there
  are any, or if a shard failed (which would leave the intersection
  incomplete). The first full run (Linux only, 2026-11-11): 1,842 caught,
  380 missed, 254 unviable.
* `bench.yml`, on every push to main: the criterion benchmarks, prebuilt
  into an image (`.github/bench`, with git for the fixtures), run on
  Bencher's bare-metal `intel-v1` runners, which are dedicated, so runs
  are comparable, and tracked per commit with a t-test threshold that
  fails the job on a regression. The free plan stops a job at 5 minutes,
  so the run uses 1 s of warm-up and 3 s of measurement per benchmark
  (the whole suite takes about 4 minutes locally at criterion's defaults).
  Skipped until the `BENCHER_API_KEY` secret exists.
* Dependabot groups cargo and action updates weekly, major versions
  included, which covers what cargo-outdated would report. It waits two
  weeks after a release before proposing it (`cooldown`); security
  updates don't wait. Actions are
  pinned to commit hashes, with the version in a comment.
* Windows isn't covered: parts of argus are Unix-only.

**Release builds (planned).** Measure before choosing: `lto` (thin or
fat) with `codegen-units = 1` for speed and size, `strip` for size while
keeping split, packed debug info (a `.dSYM` on macOS) aside to symbolicate
crash reports, and possibly `panic = "abort"` (the TUI restores the
terminal from its panic hook, which still runs). argus is mostly bound by
file-system calls and git processes, so expect size to change more than
speed.

## 9. Not done yet / next steps

* Embedded terminal pane; worktree pruning.
* Snoozing per issue (and per repo, as now): today a snooze hides the
  whole repo until it expires.
* Fixup commits (`c f`, as in magit): commit what's staged as a fixup
  of a commit picked like reset's, for a later autosquash.
* Pull requests: fetched with the remote, shown per branch; a merged PR
  makes a squash-merged branch safe to delete.
* Orphans (local clones the forge no longer lists): reported only, no
  archive/move action.
* GitLab/Gitea forges (implement `argus_core::forge::Forge`); jj backend.
* `argus-server` (axum, JSON over the same `Workspace`).
* Filesystem watching for live refresh. Today: `g`, and after each operation.
* Stable machine-readable output. `--json` works for every command; pull,
  fix, mark, ack, root and paths build their documents explicitly in the
  CLI, but `status` and `forge list` still serialise internal structs
  (`Entry`, `RemoteRepo`), which isn't a stable contract. Consider a
  versioned schema and/or a line-oriented `--porcelain` format (like
  `git status --porcelain=v2`).
* Python bindings to `argus-core` (pyo3/maturin), for scripting over the
  same workspace, status and fix planning.
