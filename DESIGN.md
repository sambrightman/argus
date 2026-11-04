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
  first remote). Rules match against identity or path with globs.
* **Rules.** `[[rule]] match = "github.com/acme/*"` sets `favourite`, `sync`
  or `exclude`. Later rules override earlier ones.
* **Snooze.** Temporary exclusion until a date. Kept in the *state* file, not
  config, so config stays hand-edited.
* **Tracked owners.** `[[track]] forge="github" owner="acme" [team="platform"] into="~/dev/acme" clone="auto"|"notify"`.
  Sync lists the forge, diffs it against local clones, and clones missing repos
  (`auto`) or flags them as *new on forge* (`notify`). Newly cloned repos are
  marked ✦ new until acknowledged. Tracked owners are implicitly syncable.
* **Attention.** A repo needs attention if it has any of: conflicts or an
  in-progress operation, uncommitted changes, unpushed commits, a non-default
  branch, detached HEAD, stashes, a gone upstream, or no upstream for the
  current branch. Sort order: favourites, then severity, then amount of
  change, then name.
* **Auto-fixes** (planned and shown first, then executed; never destructive):
  * `switch-default`: on a non-default branch, worktree clean, branch fully
    pushed (or merged into default) → `git switch <default>`.
  * `push`: ahead of upstream, nothing behind → `git push`.
  * `stash`: dirty → `git stash push -u -m "argus <timestamp>"`.
  * `commit`: needs a message (TUI prompt / `--message`) → `git add -A && git commit`.
  * `pull` / `fetch`: plain `git pull` / `git fetch`, so your config decides.

## 6. TUI

* Table, one line per repo: `★ ✦ name  branch  ↑2 ↓1  ●3 ✚1 …4  ⚑2  ⚠ rebase`.
  Grouping by owner, with folding.
* Enter / Tab unfolds detail: files, branches, stashes, worktrees, submodules.
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

## 9. Not done yet / next steps

* Embedded terminal pane; per-file actions in the details view (stage,
  discard, diff); deleting merged branches; worktree pruning.
* Orphans (local clones the forge no longer lists): reported only, no
  archive/move action.
* GitLab/Gitea forges (implement `argus_core::forge::Forge`); jj backend.
* `argus-server` (axum, JSON over the same `Workspace`).
* Filesystem watching for live refresh. Today: `g`, and after each operation.
