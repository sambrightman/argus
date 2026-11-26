# Changelog

What changed in each release, newest first, in the form of [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/). A release's section is
its GitHub release's notes. Changes go under Unreleased as they're made;
`cargo release` names that section at the next release (see DESIGN.md,
Releasing).

<!-- next-header -->
## Unreleased - ReleaseDate

### New

- Whether each branch is squash- or rebase-merged is kept between runs (a file per repo in `~/.cache/argus/squash-merged/`), so with many old branches only the first load pays for working it out.
- With `-v`, each repo's `files` step says what it read to find changes: files read again because their details on disk no longer match the index (and how many of those hadn't changed), and the folders walked for untracked files.
- In the message history, `o` sorts the steps by their own time (when none of the steps they took ran) instead of all they took, slowest first or by kind.

### Fixed

- With `-v`, a branch's `unpushed` and a repo's `remote tips` steps say which branch and repo they're of.

**Full Changelog**: https://github.com/sambrightman/argus/compare/v0.3.0...HEAD

## 0.3.0 - 2026-11-22

### Upgrading from 0.2.0

Config, commands and a few keys changed. argus says what an old setting is called now when it meets one, rather than ignoring it.

- **Marks are tags.** Favourite, pull, ignore and your own labels were "marks", and so were the rows marked with space: two things by one name. The config's are now tags:
  - `[marks.<name>]` → `[tags.<name>]`, and colours `mark.<name>` → `tag.<name>`
  - `argus mark` / `unmark` → `argus tag` / `untag`; `status --mark` → `status --tag`
  - `"marks"` → `"tags"` in `--json`
  - The Tag menu is on `t` (was `m`).
- **`M` and `U` mark and unmark repos** wherever the cursor is (in a repo's details they used to mark its files). `C-g` and `Esc` cancel a region and what's marked in details, never the marked repos: that's `U`.
- **The process log is `v $`** in the View menu; `$` and `L` by themselves no longer toggle it. The new message history is `v m`.
- **Fix auto fast-forwards stale branches by default**, last in `[fix] auto`: only branches not checked out, only fast-forwards, no network. Leave `fast-forward` out of `[fix] auto` to keep 0.2.0's behaviour.
- **A theme or icon set picked with `v t` / `v i` is saved** as `[ui] theme` / `icons` in your config file.

### New

**Marking and regions**
- A file's mark is its hunks' and lines': space on a file marks it where it's listed, a hunk or line taken out leaves the rest marked, and a partly marked file or hunk has its own icon.
- `C-space` starts a region of lines, of whole hunks or of whole files, according to where it starts, shown by a background as in magit.
- Space on a section's heading marks its files.
- Marked lines survive a refresh that finds their file's diff unchanged; when it changed, argus says which marks went.

**Staging, stashing and discarding**
- `s`, `u` and `K` on a repo's own row take the whole repo, asking first.
- With marks in another repo, `s`, `u` and `K` ask whether you mean this repo or everything marked.
- Stash marked (`z m`) stashes marked lines by themselves, leaving the rest of the file and what's staged as they are.
- The stash menu is in parts, each with the switches that apply to it.
- Popping a stash restores its staging beside what's staged now. A pop that would overwrite a file's own changes, or a file it adds that's already here, is refused before anything is touched, saying which file.
- `s` and `u` never take a conflicted file: they leave it out and say so. `K`'s questions count conflicted files.
- Staging a whole repo counts without the repos nested in it, and says which it left out.

**Finding out what argus is doing**
- `-v` / `--verbose` says each step taken and how long it took: config and state read, the scan, each repo's status and its parts, each branch's checks, every git command and job, forge requests.
- The message history (`v m`) keeps everything the status line said, each job with its output, and with `-v` each step, holding the steps it took, folded: a scan holds its statuses, a status its parts. A step says how its time divides: `30ms scan (own 3ms, sub-steps 27ms from 45ms total)`. `s` lists every step slowest first, then totals by kind; `/` searches.
- A scan names a folder it walked a thousand folders into and found no repo in, for `exclude_paths` to skip.

**Scanning and status**
- With no roots configured, the repo the current directory is in is scanned too, and the TUI starts on it.
- Commits not on any remote are counted without walking every remote branch when that can be avoided, using git's commit-graph: in a repo with 259 remote branches, a branch's count went from 60ms to under 1ms.
- Git's installation config is remembered between runs, so opening repos doesn't wait for git (80ms or more cold); git is asked again alongside the scan in case it changed.
- The Fix menu says how many repos have fixes that `[fix] auto` leaves out, and which kinds.

### Fixed and hardened

- Patches of part of a diff say exactly where they go, so lines taken from a hunk can't land a few lines off; they keep a last line without a newline whole, and CRLF endings.
- What `s`, `u` and `K` take is decided in one place from what the rows show, checked by property tests from any state: nothing marked out of sight is taken, and the unmarked is left alone.
- Bytes that aren't text, carriage returns and odd file names are handled in patches and stashes.

**Full Changelog**: https://github.com/sambrightman/argus/compare/v0.2.0...v0.3.0

## 0.2.0 - 2026-11-16

### Upgrading from 0.1.0

Config and commands changed. argus says where each old setting went when it meets one, rather than ignoring it.

- **Favourites, pull and ignore are marks.** Each is a table under `[marks]`:
  - `favourite = [...]` → `[marks.favourite]` with `repos = [...]`
  - `pull = [...]` → `[marks.pull]` with `repos = [...]`
  - `[ignore]` (repos and issues) → `[marks.ignore]` and `[marks.ignore.issues]`
- **Marks set in the TUI are saved in the config**, not the state file. A state file with marks from 0.1.0 gets a notice saying what to copy across.
- **CLI:** `argus mark MARK PATTERNS…` and `argus unmark MARK PATTERNS…`; `argus unmark --all` (was `--clear`); `argus snooze PATTERNS --for 3d [--issue …]`, `argus unsnooze`; `argus ignore-issue` / `unignore-issue`.
- **TUI keys:** the Fix menu's abort moved to `k`. Fix once is `x`/`a` (selection/all), fix auto is `X`/`A`.
- **Building from source** needs Rust 1.100 or later.

### New

**Staging and changes**
- Changes are shown by section, as in magit: untracked, unstaged, staged. The details are a tree that folds with Tab.
- Stage, unstage and discard (`s`, `u`, `K`) a file, a hunk or single lines. Space marks lines (or files) to act on several at once, and C-space selects a region of diff lines.
- `e` edits a patch before staging or unstaging it, as `git add -p`'s `e` does.
- Diffs go through `textconv`, as `git diff` shows them.

**Commits and branches**
- Fixup commits (`c f`), and instant fixup and squash (`c F`, `c s`, `c S`).
- `b b` picks a branch by typing (fuzzy), `b -` goes back to the previous one, `b c` creates one at HEAD.
- Spin-off and spin-out (`b s`, `b S`) move unpushed commits to a new branch. `b k` deletes the branch under the cursor, saying what's known of it.
- Squash- and rebase-merged branches count as merged.

**Stashes**
- The stash menu has switches for untracked files, keep index and a message, and pops or drops any stash, not only the newest.

**Marks, filtering and snoozing**
- Your own marks: `[marks.<name>]` with a key, icon, colour and sort weight. They show in the flag column and in the Mark menu.
- `/` filters fuzzily, and by marks: `#work`, `!#work`, `#a | #b`. `argus status --filter` takes the same.
- Snooze one issue of a repo (`m z` on its line), not just the whole repo. Mark a repo's owner as a whole (`m o`).

**Fixes**
- `argus fix --auto` keeps fixing each repo until nothing more applies; the Fix menu's fix auto does the same.
- Finished worktrees and stale worktree records are flagged, with fixes to remove or prune them. `submodule-update` puts a moved submodule back.

**Forges**
- The details show a GitHub repo's open pull requests and issues, and each branch's pull request, asked in the background. Forks show their parent's counts.
- Forge names, counts and pull requests are terminal links, in the TUI and `argus status`.

**Status**
- `argus status --details` prints the TUI's details under each repo. The table's columns line up.
- Every issue has an icon in the row, including off the default branch, no remote, and worktree problems.

### Fixed and hardened

- Text from repos and forges can't reach the terminal as escape sequences; only argus's own colours and links pass.
- The forge token goes only to the forge's API. Clones stay in their folder. Discard never takes staged changes.
- Saves keep what another argus wrote meanwhile, keep file permissions and leave no temporary files.
- The TUI draws only when something may have changed, and results show within a frame.

**Full Changelog**: https://github.com/sambrightman/argus/compare/v0.1.0...v0.2.0

## 0.1.0 - 2026-11-12

The first release.

**Full Changelog**: https://github.com/sambrightman/argus/commits/v0.1.0
