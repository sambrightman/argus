//! TUI application state machine. Pure: keys and worker messages go in,
//! [`Effect`]s come out. No terminal or threads here, so it's unit-testable.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use argus_core::attention::Issue;
use argus_core::badges::Role;
use argus_core::config::StashConfig;
use argus_core::diff::{self, FileDiff, Side};
use argus_core::fix::{self, Applied, FixKind};
use argus_core::gitignore::IgnoreFile;
use argus_core::ops::{self, Op, OpOutcome, ResetMode};
use argus_core::pull::{self, Job, PullPlan};
use argus_core::rules::{IgnoreState, IssueList, Tag};
use argus_core::status::RepoStatus;
use argus_core::{Entry, ViewFilter, Workspace, paths, plural};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::details::{DetailLine, DiffRef, Expand, Section, Seg, ago, describe, details};
use crate::input::{Edit, LineEditor};
use crate::menu::{self, Item, Menu, MenuId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Marked repos, or the repo under the cursor if none are marked.
    Selected,
    /// Every visible repo (for pull: every one tagged for pull).
    All,
}

/// Which of the ignore menu's issues to move, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreWhich {
    /// That one (by its place), to its next state.
    One(usize),
    /// Each remaining (counted) one, to its default way of ignoring.
    All,
    /// Each ignored one, to counted.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    NextRepo,
    PrevRepo,
    /// The next / previous section line: like Down / Up, skipping body
    /// text (see [`DetailLine::body`]).
    NextSection,
    PrevSection,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
    ToggleMark,
    /// Mark the repos listed.
    MarkAll,
    /// End a region and unmark the files and lines marked in details.
    Cancel,
    /// Stage, unstage or discard in the cursor's repo alone (what's marked
    /// in it, else what the cursor is on), or what's marked in every repo:
    /// the answers when the key is pressed with marks in another repo.
    TakeHere(HunkVerb),
    TakeAll(HunkVerb),
    /// Stash what's marked in every repo, and the gitignore menu for the
    /// untracked files marked in every repo or (`false`) for the cursor's
    /// repo alone: the answers to the same question.
    StashAll,
    GitignoreIn(bool),
    /// Start a region of diff lines at the cursor (`C-space`), or end it,
    /// keeping its lines as marks.
    ToggleRegion,
    /// Unmark the repos listed.
    UnmarkAll,
    OpenMenu(MenuId),
    StartFilter,
    ClearFilter,
    Help,
    /// The message history.
    History,
    Quit,
    ToggleLog,
    Fetch(Scope),
    Pull(Scope),
    Push,
    Publish,
    PushBranches,
    Stash(StashKind),
    /// Flip a menu switch for this session.
    Toggle(Switch),
    /// Stage, unstage or discard the marked files, else the one under the
    /// cursor in the details.
    StageFiles,
    UnstageFiles,
    DiscardFiles,
    /// In a diff, open the marked lines (else the hunk or line under the
    /// cursor) as a patch in git's editor; what's saved is staged, or
    /// unstaged on the staged side.
    EditPatch,
    /// Ask for a message, then commit what's staged, or (`all`) every
    /// change, untracked files included.
    Commit {
        all: bool,
    },
    /// Ask for a message, starting from the last commit's, then amend it
    /// with what's staged.
    Amend,
    SwitchDefault,
    Reattach,
    /// Ask for a name, then create a branch at each selected detached HEAD
    /// that has commits on no branch.
    CreateBranch,
    /// Move the unpushed commits to a new branch (asked for), checked out
    /// or (`stay`) not.
    SpinOff {
        stay: bool,
    },
    /// A new branch at HEAD, checked out, in any repo with a work tree
    /// (`b c`); CreateBranch is the fix naming a detached HEAD's commits.
    NewBranch,
    /// Ask for a URL, then add it as `origin` to the current repo, if it
    /// has no remote.
    AddRemote,
    /// Abort the operation in progress in the selected repos, once confirmed.
    AbortOperation,
    /// Delete the selected repos' merged branches, once confirmed.
    DeleteMergedBranches,
    /// Delete the branch under the cursor in the details, once confirmed.
    DeleteBranch,
    PopStash,
    /// Drop the stash under the cursor in the details, once confirmed.
    DropStash,
    FastForward,
    /// Fix once: the one fix `[fix] auto` chooses for each repo.
    AutoFix(Scope),
    /// Fix auto: that, then the next, until nothing more applies.
    FixAuto(Scope),
    PullAndClone,
    /// Toggle the tag at this place in `[tags]` (its key in the Tag menu).
    ToggleLabel(usize),
    TogglePull,
    ToggleIgnore,
    /// A tag for the selected repo's owner as a whole.
    ToggleOwnerTag(Tag),
    /// Open the ignore menu for the repo under the cursor: its issues that
    /// can be ignored, each with its state.
    IgnoreIssues,
    /// In the ignore menu: move issues to another state.
    IgnoreStep(IgnoreWhich),
    /// Open the gitignore menu for the untracked files marked, or under
    /// the cursor.
    GitignoreMenu,
    /// Apply a fix of this kind to the selection, as its own action does
    /// (asking first, or for what it needs, where that does).
    ApplyFix(FixKind),
    /// Offer the current repo's commits to reset to, this way.
    Reset(ResetMode),
    /// In the reset menu: reset to that commit (by its place).
    /// The `n`th commit of the commit list (reset to it, or fix it up).
    PickCommit(usize),
    /// Commit what's staged as a fixup (or a squash commit, its message
    /// asked for) of a commit picked from a list, folded in at once with
    /// `instant`.
    Fixup {
        squash: bool,
        instant: bool,
    },
    /// Check out a branch picked from a list.
    Checkout,
    /// The `n`th branch of the branch list.
    PickBranch(usize),
    /// Check out the branch checked out before (`git switch -`).
    CheckoutPrevious,
    /// In the gitignore menu: add the rules to that file.
    Gitignore(IgnoreFile),
    Snooze,
    Unsnooze,
    Ack,
    ClearTags,
    Shell,
    GitUi,
    Editor,
    Browse,
    ToggleAttention,
    ToggleHidden,
    Refresh,
    Rescan,
    /// Drop the selected repos whose folder is gone from the list (not a
    /// stale worktree: pruning its record does that).
    DropGone,
    /// Next built-in theme, for this session only.
    NextTheme,
    /// Next icon set, for this session only.
    NextIcons,
}

/// Most commits a list unfolds into.
const MAX_COMMITS: usize = 50;
/// Most files an untracked directory unfolds into.
const MAX_DIR_FILES: usize = 200;

/// `line` indented `pad` more spaces.
fn padded(mut line: DetailLine, pad: usize) -> DetailLine {
    match line.parts.first_mut() {
        _ if pad == 0 => {}
        // more of the indent it starts with: its parts stay the same in
        // number and order (a diff line's are its indent, then its text)
        Some((None, text)) if text.chars().all(|c| c == ' ') => {
            text.push_str(&" ".repeat(pad));
        }
        _ => {
            line.parts.insert(0, (None, " ".repeat(pad)));
            for l in &mut line.links {
                l.0 += 1;
            }
        }
    }
    line
}

/// How far what `line` unfolds into is indented.
fn unfold_indent(line: &DetailLine) -> usize {
    let text: String = line.parts.iter().map(|(_, t)| t.as_str()).collect();
    (text.len() - text.trim_start().len() + 2).max(6)
}

/// What `x` unfolds into, read now, indented `indent` spaces.
fn unfold(repo: &std::path::Path, s: &RepoStatus, x: &Expand, indent: usize) -> Vec<DetailLine> {
    use argus_core::history;
    let pad = " ".repeat(indent);
    let line = |parts: Vec<(Option<Role>, String)>| {
        let mut p = vec![(None, pad.clone())];
        p.extend(parts);
        crate::details::line_of(p)
    };
    let body = |parts| DetailLine {
        body: true,
        ..line(parts)
    };
    let dim = |t: String| (Some(Role::Dim), t);
    let note = |part| vec![line(vec![part]).at(Seg::Note)];
    let failed = |e: String| note((Some(Role::Error), e));
    match x {
        Expand::Commits(range) => match history::commits(repo, range, MAX_COMMITS) {
            Err(e) => failed(e),
            Ok((v, _)) if v.is_empty() => note(dim("no commits".into())),
            Ok((v, more)) => {
                let mut rows: Vec<DetailLine> = v
                    .into_iter()
                    .map(|c| {
                        let when = c.time.map(|t| format!(", {}", ago(t))).unwrap_or_default();
                        DetailLine {
                            expand: Some(Expand::Commit(c.id.clone())),
                            ..line(vec![
                                dim(c.id.chars().take(7).collect()),
                                (None, format!(" {}", c.summary)),
                                dim(format!("  {}{when}", c.author)),
                            ])
                        }
                        .at(Seg::Commit(c.id))
                    })
                    .collect();
                if more {
                    rows.push(
                        line(vec![dim(format!(
                            "… more (the newest {MAX_COMMITS} shown)"
                        ))])
                        .at(Seg::More),
                    );
                }
                rows
            }
        },
        Expand::UnpushedBranches => s
            .branches
            .iter()
            .filter(|b| !b.is_head && b.is_unpushed())
            .map(|b| {
                DetailLine {
                    expand: Some(Expand::Commits(history::Range::NotOnRemote {
                        tip: format!("refs/heads/{}", b.name),
                    })),
                    ..line(vec![
                        (Some(Role::Branch), b.name.clone()),
                        dim(match b.unpushed {
                            Some(n) => format!("  {n} on no remote"),
                            None => "  on no remote".into(),
                        }),
                    ])
                }
                .at(Seg::Branch(b.name.clone()))
            })
            .collect(),
        Expand::Commit(id) => match history::commit_detail(repo, id) {
            Err(e) => failed(e),
            Ok(d) => {
                let when = d.time.map(|t| format!(", {}", ago(t))).unwrap_or_default();
                let mut rows = vec![
                    line(vec![dim("commit ".into()), (None, d.id.clone())]).at(Seg::Info("commit")),
                    line(vec![
                        dim("author ".into()),
                        (None, format!("{} <{}>{when}", d.author, d.email)),
                    ])
                    .at(Seg::Info("author")),
                ];
                let message = d.message.lines().enumerate();
                rows.extend(
                    message.map(|(n, l)| body(vec![(None, format!("  {l}"))]).at(Seg::Text(n))),
                );
                // (its files fold to this line)
                rows.push(line(vec![dim(format!("files ({})", d.files.len()))]).at(Seg::Files));
                rows.extend(d.files.iter().map(|(code, path)| {
                    let role = match code {
                        'A' => Role::Ahead,
                        'D' => Role::Behind,
                        _ => Role::Unstaged,
                    };
                    DetailLine {
                        expand: Some(Expand::CommitFile(d.id.clone(), path.clone())),
                        ..line(vec![
                            (Some(role), format!("  {code}")),
                            (None, format!(" {path}")),
                        ])
                    }
                    .at(Seg::Files)
                    .at(Seg::File(path.clone()))
                }));
                rows
            }
        },
        Expand::CommitFile(id, path) => {
            let d = diff::commit_file_diff(repo, id, path).map_err(|e| e.to_string());
            let row = |parts, part: DiffPart, _| DetailLine {
                body: matches!(part, DiffPart::Line(..)),
                ..line(parts)
            };
            diff_lines(&d, "converted to text (textconv)", &row)
        }
        Expand::Changes => crate::details::sections(s)
            .into_iter()
            .map(|l| padded(l, indent))
            .collect(),
        // its entry's details, which `rebuild_at` puts there
        Expand::Repo(_) => vec![],
        Expand::Dir(dir) => match argus_core::status::untracked_in(repo, dir, MAX_DIR_FILES) {
            Err(e) => failed(e.to_string()),
            Ok((files, more)) => {
                let mut rows: Vec<DetailLine> = files
                    .into_iter()
                    .map(|(f, kind)| {
                        let expand = crate::details::untracked_expand(&f, kind);
                        DetailLine {
                            file: Some(f.clone()),
                            section: Some(Section::Untracked),
                            ..line(vec![(Some(Role::Untracked), f.clone())])
                        }
                        .at(crate::details::file_seg(&f, expand.as_ref()))
                        .unfolding(expand)
                    })
                    .collect();
                if more {
                    rows.push(
                        line(vec![dim(format!(
                            "… more (the first {MAX_DIR_FILES} shown)"
                        ))])
                        .at(Seg::More),
                    );
                }
                rows
            }
        },
    }
}

/// Where a row is, to find it (or what's nearest) after the rows are
/// built again.
#[derive(Debug, Clone)]
struct Place {
    /// The listed repo it's under.
    root: PathBuf,
    /// The row's place there ([`DetailLine::id`]); the repo's own row: none.
    id: Vec<Seg>,
    /// For the row and each line it's under, outermost first: the lines
    /// beside it (their last step) and which of them it is.
    beside: Vec<(Vec<Seg>, usize)>,
}

/// A detail line's styled parts.
type Parts = Vec<(Option<Role>, String)>;

/// What a row of a diff is, for whoever makes the row.
enum DiffPart<'a> {
    /// Something said in place of content, or about it.
    Note,
    /// A hunk's header, by the hunk's place among the diff's.
    Header(usize),
    /// A line: its hunk, its place in it, itself.
    Line(usize, usize, &'a argus_core::diff::Line),
}

/// A diff as rows, each in its place under the line the diff is of: a
/// note where there's nothing to show, else each hunk's header and its
/// lines. `row` makes a row of its parts, told what it is and whether the
/// diff stands for the whole file only (converted to text: `converted`
/// says so first). The same for a changed file and for one in a commit.
fn diff_lines(
    d: &Result<FileDiff, String>,
    converted: &str,
    row: &dyn Fn(Parts, DiffPart, bool) -> DetailLine,
) -> Vec<DetailLine> {
    let note = |role, text: &str| {
        row(vec![(Some(role), text.to_string())], DiffPart::Note, false).at(Seg::Note)
    };
    let (hunks, whole) = match d {
        Err(e) => return vec![note(Role::Error, e)],
        Ok(FileDiff::Binary) => return vec![note(Role::Dim, "binary file")],
        Ok(FileDiff::External) => return vec![note(Role::Dim, "external diff configured")],
        Ok(FileDiff::Text(hunks)) => (hunks, false),
        Ok(FileDiff::Converted(hunks) | FileDiff::Inexact(hunks)) => (hunks, true),
    };
    if hunks.is_empty() {
        return vec![note(Role::Dim, "no change in content")];
    }
    let mut v = vec![];
    if whole {
        let inexact = "not all UTF-8 text, shown as near as can be: staged or discarded as a \
                       whole file";
        v.push(note(
            Role::Dim,
            match d {
                Ok(FileDiff::Inexact(_)) => inexact,
                _ => converted,
            },
        ));
    }
    let at = Seg::hunks(hunks);
    for (h, hunk) in hunks.iter().enumerate() {
        let header = vec![(Some(Role::DiffHunk), hunk.header())];
        v.push(row(header, DiffPart::Header(h), whole).at(at[h].clone()));
        for (i, l) in hunk.lines.iter().enumerate() {
            let line = row(crate::details::diff_line(l), DiffPart::Line(h, i, l), whole);
            v.push(line.at(at[h].clone()).at(Seg::Line(i)));
        }
    }
    v
}

/// The diff rows under a changed file's line, for that section's side:
/// each knows its file, hunk and line, to stage or discard by.
fn diff_rows(
    cache: &mut HashMap<(PathBuf, String, Side), Result<FileDiff, String>>,
    repo: &std::path::Path,
    file: &str,
    section: Section,
) -> Vec<DetailLine> {
    let side = section.side();
    let d = cache
        .entry((repo.to_path_buf(), file.to_string(), side))
        .or_insert_with(|| diff::file_diff(repo, file, side).map_err(|e| e.to_string()));
    let row = |mut parts: Parts, part: DiffPart, whole: bool| {
        parts.insert(0, (None, "      ".to_string()));
        // no patch of converted content to apply: every row stands for
        // the file
        let (hunk, line) = match part {
            DiffPart::Header(h) if !whole => (Some(h), None),
            DiffPart::Line(h, i, l) if !whole => (Some(h), Some((i, l))),
            _ => (None, None),
        };
        DetailLine {
            file: Some(file.to_string()),
            diff: Some(Box::new(DiffRef {
                side,
                hunk,
                line: line.map(|(_, l)| l.clone()),
                at: line.map(|(i, _)| i),
            })),
            body: whole && !matches!(part, DiffPart::Note),
            section: Some(section),
            ..crate::details::line_of(parts)
        }
    };
    let converted = "converted to text (textconv): staged or discarded as a whole file";
    diff_lines(d, converted, &row)
}

/// Where file operations apply, for when there's no file to act on.
const FILE_HINT: &str = "move to a file in the details (Tab), or mark files with space";

/// What a stash action takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StashKind {
    /// Everything (untracked files as the switch says).
    All,
    /// Only staged changes (untracked files and the index, never: the
    /// first two switches don't apply).
    Staged,
    /// The files marked in the details view.
    MarkedFiles,
    /// Those marked in the cursor's repo alone.
    MarkedHere,
}

/// Menu switches, kept for the session (starting from config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Stash untracked files too.
    Untracked,
    /// Leave staged changes in the index after stashing.
    KeepIndex,
    /// Ask for the stash's message first.
    Message,
    /// In the gitignore menu: rules match only their path (`/build/`), not
    /// the name anywhere (`build/`).
    Anchored,
}

/// Something for the runtime to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Jobs(Vec<Job>),
    Fix {
        paths: Vec<PathBuf>,
        kinds: Vec<FixKind>,
    },
    /// Discover + status everything.
    Scan,
    /// Re-status these repos.
    Refresh(Vec<PathBuf>),
    /// List forges and plan clones/pulls; answer with [`Msg::PullPlanned`].
    PlanPullAndClone,
    /// Describe a detached HEAD however far back its tag is; answer with
    /// [`Msg::Described`].
    Describe(PathBuf),
    /// Ask the forge of this `kind` about the repo and pull requests from
    /// `branches`; answer with [`Msg::Forge`].
    Forge {
        path: PathBuf,
        kind: String,
        id: argus_core::identity::RepoId,
        branches: Vec<String>,
        /// Whose pull requests count besides the repo's owner's: its
        /// other remotes' owners.
        owners: Vec<String>,
    },
    /// Leave the TUI, run a command in a directory, come back.
    Suspend {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        /// An interactive shell, started as a login shell (see
        /// [`crate::suspended_command`]).
        login_shell: bool,
    },
    Open(String),
    SaveState,
    /// Write the tag lists' changes since this config to the config file.
    SaveTags(Box<argus_core::Config>),
    Quit,
}

/// A repo a fix-auto action is working through.
#[derive(Debug, Default)]
struct Chain {
    /// The fixes applied so far.
    done: Vec<fix::Fix>,
    /// Its status was read a second time after the last fix, the first
    /// reading having still wanted that fix.
    read_again: bool,
}

/// A repo's forge, as far as it has been asked.
#[derive(Debug, Default)]
struct ForgeLook {
    /// The branches the last question was about.
    asked: Option<Vec<String>>,
    /// The last answer: kept while a newer question is out.
    answer: Option<Result<Option<argus_core::forge::RepoInfo>, String>>,
}

/// Results coming back from the runtime.
#[derive(Debug)]
pub enum Msg {
    /// A step taken (with `-v`), for the history.
    Step(argus_core::trace::Step),
    /// A full describe finished (its answer is remembered by status).
    Described(PathBuf),
    /// What a forge said of a repo, asked about these branches.
    Forge {
        path: PathBuf,
        branches: Vec<String>,
        info: Result<Option<argus_core::forge::RepoInfo>, String>,
    },
    Scanned {
        entries: Vec<Entry>,
        /// Directories skipped because of `max_depth`.
        truncated: Vec<PathBuf>,
    },
    Status(PathBuf, Box<Result<RepoStatus, String>>),
    JobStarted {
        path: PathBuf,
        label: String,
    },
    JobFinished {
        path: PathBuf,
        label: String,
        op: Op,
        outcome: OpOutcome,
    },
    FixDone {
        path: PathBuf,
        applied: Result<Option<Applied>, String>,
    },
    PullPlanned {
        tracks: argus_core::State,
        plan: PullPlan,
        errors: Vec<(String, String)>,
    },
    Suspended(Result<(), String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Filter,
    CommitMessage(Vec<PathBuf>, bool),
    /// For amending the repo's last commit: its message, and whether
    /// it's on a remote.
    AmendMessage(PathBuf, String, bool),
    /// For a stash of that kind.
    StashMessage(StashKind),
    BranchName(Vec<PathBuf>),
    /// The message a squash commit of the commit adds (`true`: instant).
    SquashMessage(PathBuf, String, bool),
    /// The new branch's name for a spin-off (`true`: spin-out).
    SpinOffName(Vec<PathBuf>, bool),
    RemoteUrl(PathBuf),
    SnoozeDuration(Vec<String>),
    /// How long to snooze one issue (of the repo with that key) for.
    SnoozeIssue(String, Issue),
}

pub enum Mode {
    Normal,
    Menu {
        menu: Menu,
        cursor: usize,
    },
    Input {
        prompt: Prompt,
        editor: LineEditor,
    },
    Help {
        scroll: u16,
    },
    /// The message history, scrolled `back` lines from its newest.
    History {
        back: usize,
    },
    /// Picking from a list by typing: `names` matched fuzzily against the
    /// query, `shown` their indices best first, `cursor` into `shown`.
    Pick {
        title: String,
        editor: LineEditor,
        labels: Vec<String>,
        names: Vec<String>,
        shown: Vec<usize>,
        cursor: usize,
    },
    /// A permanently destructive action waiting for `y`; any other key
    /// cancels it.
    Confirm {
        question: String,
        then: Confirmed,
    },
}

/// Actions that need confirmation, with what they apply to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    ClearTags(Vec<String>),
    /// Open the edited patch that didn't apply again (no drops it).
    EditAgain,
    /// Operations to run, by repo path.
    Ops(Vec<(PathBuf, Op)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
// (a detail line is far bigger than a repo's index, but nearly every row
// is one: boxing them would cost an allocation each for nothing saved)
#[allow(clippy::large_enum_variant)]
pub enum Row {
    Repo(usize),
    Detail { repo: usize, line: DetailLine },
}

/// Something that happened, for the message history (`m`).
#[derive(Debug, Clone)]
pub struct Event {
    pub at: jiff::Timestamp,
    pub what: Happened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    /// What the status line said.
    Said(String),
    /// A failure the status line reported.
    Failed(String),
    /// A job finished.
    Job {
        repo: String,
        label: String,
        ok: bool,
        output: String,
    },
    /// A step taken (with `-v`).
    Step(argus_core::trace::Step),
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: jiff::Timestamp,
    pub repo: String,
    pub label: String,
    pub ok: bool,
    pub output: String,
}

pub struct App {
    pub ws: Workspace,
    pub mode: Mode,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// Top row of the viewport, maintained by the renderer.
    pub scroll: usize,
    /// The places of the open menu's items last drawn, maintained by the
    /// renderer: a key for an item out of sight only scrolls to it.
    pub menu_shown: Option<std::ops::Range<usize>>,
    pub page: usize,
    pub filter: String,
    pub view: ViewFilter,
    pub expanded: BTreeSet<PathBuf>,
    /// Repos whose full describe is running.
    pub describing: BTreeSet<PathBuf>,
    pub marked: BTreeSet<PathBuf>,
    /// Files marked in the details view, by repo path.
    pub marked_files: BTreeMap<PathBuf, BTreeSet<(Section, String)>>,
    /// Added or removed lines marked in diffs: repo, file, side, hunk and
    /// line index. They go when the repo's status (so its diff) changes.
    pub marked_lines: BTreeSet<MarkedLine>,
    /// Where a region (`C-space`) starts: a diff row's repo, file, side,
    /// hunk and line. Its lines run to the cursor.
    pub region: Option<RegionStart>,
    /// The patch being edited (`e`), until it's applied or dropped.
    pub editing: Option<PatchEdit>,
    /// Lines of a listed repo's details folded or unfolded (Tab) the
    /// other way from how their kind starts (see
    /// [`Seg::open_by_default`]), by their place ([`DetailLine::id`]).
    pub toggled: BTreeMap<PathBuf, BTreeSet<Vec<Seg>>>,
    /// Diffs read so far; a repo's go when its status changes.
    diffs: HashMap<(PathBuf, String, Side), Result<FileDiff, String>>,
    /// What details lines unfolded into, by indent, read once until the
    /// repo's status changes.
    unfold_cache: HashMap<(PathBuf, Expand, usize), Vec<DetailLine>>,
    /// The stash switches for this session.
    pub stash: StashConfig,
    pub running: HashMap<PathBuf, String>,
    /// Auto-fixes still running from the last auto-fix action, and what
    /// the first did, to say what's done.
    auto_fixes: (usize, String),
    /// Repos a fix-auto action is working through, with the fixes it has
    /// applied to each so far: when one's status is next read, its next
    /// fix starts.
    chains: HashMap<PathBuf, Chain>,
    /// How many times the rows have been rebuilt (for tests of batching).
    #[doc(hidden)]
    pub rebuilds: usize,
    /// Questions from the background, waiting until nothing else is being
    /// asked ([`Self::ask_when_free`]).
    waiting: std::collections::VecDeque<(String, Confirmed)>,
    /// Operations agreed to while their repo was busy, to start when it's
    /// free ([`Self::start_queued`]).
    queued: Vec<(PathBuf, Op)>,
    /// What each repo's forge was asked and has said (see [`ForgeLook`]).
    forge: HashMap<PathBuf, ForgeLook>,
    /// The repo the open ignore menu is for.
    ignore_menu: Option<PathBuf>,
    /// The untracked files the open gitignore menu is for, by repo, and
    /// whether rules are anchored.
    gitignore: Option<GitignoreTargets>,
    /// The repo, mode and commits the open reset menu offers.
    pick: Option<CommitPick>,
    /// What each line of an open pick list (branches to check out,
    /// stashes to pop or drop) does, and where.
    picks: Option<(PathBuf, Vec<Op>)>,
    pub log: Vec<LogEntry>,
    pub show_log: bool,
    /// Everything said and done this session (see [`Happened`]), oldest
    /// first.
    pub history: Vec<Event>,
    pub message: Option<(String, bool)>,
    pub scanning: bool,
    /// Shown with each scan result, e.g. "no roots configured…".
    pub notices: Vec<String>,
    /// Where argus was started (a real path): the first scan puts the
    /// cursor on the repo holding it.
    pub start_in: Option<PathBuf>,
    pub quit: bool,
    /// Pending "g" for vi-style `gg`.
    pending_g: bool,
}

impl App {
    pub fn new(ws: Workspace) -> Self {
        let view = ViewFilter {
            attention_only: ws.config.ui.attention_only,
            show_hidden: false,
        };
        let stash = ws.config.stash;
        let mut a = Self {
            ws,
            mode: Mode::Normal,
            rows: vec![],
            cursor: 0,
            scroll: 0,
            menu_shown: None,
            page: 20,
            filter: String::new(),
            view,
            expanded: BTreeSet::new(),
            describing: BTreeSet::new(),
            marked: BTreeSet::new(),
            marked_files: BTreeMap::new(),
            marked_lines: BTreeSet::new(),
            region: None,
            editing: None,
            toggled: BTreeMap::new(),
            diffs: HashMap::new(),
            unfold_cache: HashMap::new(),
            stash,
            running: HashMap::new(),
            auto_fixes: (0, String::new()),
            chains: HashMap::new(),
            forge: HashMap::new(),
            rebuilds: 0,
            waiting: Default::default(),
            queued: vec![],
            ignore_menu: None,
            gitignore: None,
            pick: None,
            picks: None,
            log: vec![],
            show_log: false,
            history: vec![],
            message: None,
            scanning: false,
            notices: vec![],
            start_in: None,
            quit: false,
            pending_g: false,
        };
        a.rebuild();
        a
    }

    // ---------- rows ----------

    /// Recompute rows, keeping the cursor on the same row where possible,
    /// else on what's nearest: what took its place, the next beside it,
    /// the one before, the line they were under.
    pub fn rebuild(&mut self) {
        let place = self.place(self.cursor);
        self.build_rows();
        let at = place.and_then(|p| self.find(p));
        self.cursor = at
            .unwrap_or(self.cursor)
            .min(self.rows.len().saturating_sub(1));
    }

    /// The cursor to the deepest listed repo holding `dir`, if one does.
    fn start_on_repo_holding(&mut self, dir: &Path) {
        let holding = self.rows.iter().enumerate().filter_map(|(n, r)| match r {
            Row::Repo(i) => Some((n, &self.ws.entries[*i].path)),
            _ => None,
        });
        if let Some((n, _)) = holding
            .filter(|(_, p)| dir.starts_with(p))
            .max_by_key(|(_, p)| p.components().count())
        {
            self.cursor = n;
        }
    }

    /// The rows of the listed repo that row `n` is of or under: its own
    /// row's place, and its details' (the rows after it).
    fn rows_of(&self, n: usize) -> Option<(usize, std::ops::Range<usize>)> {
        let is_repo = |i: &usize| matches!(self.rows.get(*i), Some(Row::Repo(_)));
        let top = (0..=n.min(self.rows.len().checked_sub(1)?))
            .rev()
            .find(is_repo)?;
        let first = top + 1;
        let end = (first..self.rows.len())
            .find(is_repo)
            .unwrap_or(self.rows.len());
        Some((top, first..end))
    }

    /// Row `n`'s place among its repo's details.
    fn id(&self, n: usize) -> &[Seg] {
        match self.rows.get(n) {
            Some(Row::Detail { line, .. }) => &line.id,
            _ => &[],
        }
    }

    /// The last steps of the rows under the row at `parent` (blank lines
    /// aside), with where each is, among the rows in `range`.
    fn under(&self, range: std::ops::Range<usize>, parent: &[Seg]) -> Vec<(usize, &Seg)> {
        range
            .filter_map(|n| match self.id(n).split_last() {
                Some((last, rest)) if rest == parent && *last != Seg::Blank => Some((n, last)),
                _ => None,
            })
            .collect()
    }

    /// Where row `n` is, to come back to after the rows change.
    fn place(&self, n: usize) -> Option<Place> {
        let (top, range) = self.rows_of(n)?;
        let Row::Repo(i) = self.rows[top] else {
            return None;
        };
        let id = self.id(n).to_vec();
        let beside = (1..=id.len())
            .map(|d| {
                let all = self.under(range.clone(), &id[..d - 1]);
                let nth = all.iter().position(|(_, s)| **s == id[d - 1]);
                let segs: Vec<Seg> = all.into_iter().map(|(_, s)| s.clone()).collect();
                let nth = nth.unwrap_or(segs.len());
                (segs, nth)
            })
            .collect();
        Some(Place {
            root: self.ws.entries[i].path.clone(),
            id,
            beside,
        })
    }

    /// The row at `place`, or the nearest there is to it. For a line
    /// that's gone, as magit moves on: the one that took its place (a
    /// hunk partly staged is another hunk where it was), else the next of
    /// those it was beside, else the one before, else the line they were
    /// all under, and so on outwards to the repo's own row.
    fn find(&self, place: Place) -> Option<usize> {
        let top = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Repo(i) if self.ws.entries[*i].path == place.root))?;
        let (_, range) = self.rows_of(top)?;
        let mut id = place.id;
        let mut d = id.len();
        while d > 0 {
            if let Some(n) = range.clone().find(|&n| self.id(n) == &id[..d]) {
                return Some(n);
            }
            let now = self.under(range.clone(), &id[..d - 1]);
            let (before, nth) = &place.beside[d - 1];
            // something known by what's in it (a hunk, a commit), with
            // another where it was that wasn't there: it changed. That
            // one, and under it what the cursor was under
            if let Some((_, seg)) = now.get(*nth)
                && id[d - 1].by_content()
                && std::mem::discriminant(*seg) == std::mem::discriminant(&id[d - 1])
                && !before.contains(seg)
            {
                id[d - 1] = (*seg).clone();
                d = id.len();
                continue;
            }
            // else the line now where it was: the one after the nearest
            // before it that's still there (the first, with none), or
            // that one itself when nothing follows it
            let still = |seg: &Seg| now.iter().position(|(_, s)| *s == seg);
            let from = before.iter().take(*nth).rev().find_map(still);
            let at = from.map_or(0, |p| (p + 1).min(now.len().saturating_sub(1)));
            if let Some((n, _)) = now.get(at) {
                return Some(*n);
            }
            d -= 1;
        }
        Some(top)
    }

    fn build_rows(&mut self) {
        self.rebuilds += 1;
        // nested repos unfolded: entries of their own (again, after a scan),
        // each inside the one before
        let nested: Vec<(PathBuf, Vec<String>)> = self
            .toggled
            .iter()
            .flat_map(|(root, ids)| ids.iter().map(move |id| (root, id)))
            .filter(|(_, id)| matches!(id.last(), Some(Seg::Repo(_))))
            .map(|(root, id)| {
                let inside = id.iter().filter_map(|s| match s {
                    Seg::Repo(rel) => Some(rel.clone()),
                    _ => None,
                });
                (root.clone(), inside.collect())
            })
            .collect();
        for (mut p, inside) in nested {
            if self.ws.index_of(&p).is_none() {
                continue;
            }
            for rel in inside {
                match self.ws.nested(&p, &rel) {
                    Some(n) => p = self.ws.entries[n].path.clone(),
                    None => break,
                }
            }
        }
        // `#work` / `!#work` terms by tags, the rest fuzzily
        let (tags, text) = argus_core::rules::TagFilter::parse(&self.filter);
        let mut fuzzy = (!text.is_empty()).then(|| crate::fuzzy::Fuzzy::new(&text));
        let (icons, _) = self.ws.config.icons();
        let mut rows = vec![];
        for i in self.ws.ordered(self.view) {
            let e = &self.ws.entries[i];
            if !tags.matches(&e.flags) {
                continue;
            }
            if let Some(f) = &mut fuzzy
                && f.score(&e.display_name()).is_none()
                && f.score(&e.key).is_none()
            {
                continue;
            }
            rows.push(Row::Repo(i));
            if !self.expanded.contains(&e.path) {
                continue;
            }
            let root = e.path.clone();
            // depth first: each line, then what's under it if that's
            // shown; a nested repo's lines are its own, indented `pad`
            let mut lines = details(
                e,
                &self.ws.config.fix.auto,
                &self.ws.config.stash,
                &icons,
                &self.ws.state,
                crate::details::Hints::Keys,
                self.forge_view(&e.path),
            );
            if self.stale_main(i).is_some() {
                lines.extend(crate::details::gone_lines(true));
            } else if self.droppable(i) {
                lines.extend(crate::details::gone_lines(false));
            }
            let mut stack: Vec<(usize, usize, DetailLine)> =
                lines.into_iter().rev().map(|l| (i, 0, l)).collect();
            // (nothing of this repo's folded or unfolded: every line is as
            // its kind starts, and so is all it's under)
            let toggled = self.toggled.get(&root);
            let is_open = |id: &[Seg]| {
                id.last().is_some_and(Seg::open_by_default)
                    != toggled.is_some_and(|t| t.contains(id))
            };
            while let Some((r, pad, line)) = stack.pop() {
                // (a group's lines come with its heading, folded or not)
                if toggled.is_some() && (1..line.id.len()).any(|d| !is_open(&line.id[..d])) {
                    continue;
                }
                let e = &self.ws.entries[r];
                let mut below: Vec<(usize, usize, DetailLine)> = vec![];
                let mut diff = false;
                let open = line.folds() && is_open(&line.id);
                if let Some(s) = e.status().filter(|_| open) {
                    let changed = line.file.as_ref().zip(line.section);
                    match (&line.expand, changed) {
                        (Some(Expand::Repo(rel)), _) => {
                            let inner = self.ws.nested_index(&e.path, rel);
                            let inner_pad = unfold_indent(&line);
                            if let Some(n) = inner {
                                let lines = details(
                                    &self.ws.entries[n],
                                    &self.ws.config.fix.auto,
                                    &self.ws.config.stash,
                                    &icons,
                                    &self.ws.state,
                                    crate::details::Hints::Keys,
                                    None,
                                );
                                let inside = |l| (n, inner_pad, padded(l, inner_pad));
                                below.extend(lines.into_iter().map(inside));
                            }
                        }
                        (Some(x), _) => {
                            let indent = unfold_indent(&line);
                            let rows = self
                                .unfold_cache
                                .entry((e.path.clone(), x.clone(), indent))
                                .or_insert_with(|| unfold(&e.path, s, x, indent));
                            // (the sections' files have diffs of their
                            // own to unfold, as far in as they are)
                            // (a diff is two in from its file, as under
                            // a section: six, with the file at four)
                            let pad = match x {
                                Expand::Changes => indent,
                                Expand::Dir(_) => indent.saturating_sub(4),
                                _ => pad,
                            };
                            below.extend(rows.iter().map(|l| (r, pad, l.clone())));
                            diff = matches!(x, Expand::CommitFile(..));
                        }
                        (None, Some((f, section))) if line.diff.is_none() => {
                            let rows = diff_rows(&mut self.diffs, &e.path, f, section);
                            below.extend(rows.into_iter().map(|l| (r, pad, padded(l, pad))));
                            diff = true;
                        }
                        _ => {}
                    }
                }
                // a blank line after a diff sets what follows apart from
                // it; it's the diff's, so Tab there folds it
                if let Some((_, _, last)) = below.last().filter(|_| diff && !stack.is_empty()) {
                    let blank = DetailLine {
                        file: last.file.clone(),
                        section: last.section,
                        diff: last.diff.as_ref().map(|d| {
                            Box::new(DiffRef {
                                side: d.side,
                                hunk: None,
                                line: None,
                                at: None,
                            })
                        }),
                        ..crate::details::blank()
                    };
                    below.push((r, pad, blank));
                }
                stack.extend(
                    below
                        .into_iter()
                        .rev()
                        .map(|(r, pad, l)| (r, pad, l.under(&line.id))),
                );
                rows.push(Row::Detail { repo: r, line });
            }
        }
        self.rows = rows;
        // a file's mark goes when it's no longer in the section it was
        // marked in
        let ws = &self.ws;
        self.marked_files.retain(|repo, files| {
            let changed = ws.get(repo).and_then(|e| e.status()).map(|s| &s.changes);
            files.retain(|(section, f)| {
                let change = changed.and_then(|c| c.entry(f));
                // (a file in an untracked folder is listed as the folder:
                // it's there while it's on disk)
                let there = |c: &argus_core::status::FileChange| {
                    c.path == *f || repo.join(f).symlink_metadata().is_ok()
                };
                change.is_some_and(|c| section.code(c).is_some() && there(c))
            });
            !files.is_empty()
        });
    }

    pub fn current_repo(&self) -> Option<usize> {
        match self.rows.get(self.cursor)? {
            Row::Repo(i) | Row::Detail { repo: i, .. } => Some(*i),
        }
    }

    pub fn current(&self) -> Option<&Entry> {
        self.current_repo().map(|i| &self.ws.entries[i])
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|e| e.path.clone())
    }

    /// The listed repo the cursor is in: the current one, or for a nested
    /// repo's lines, the repo it's in.
    fn listed_path(&self) -> Option<PathBuf> {
        let mut e = self.current()?;
        while e.nested {
            e = self.ws.get(e.parent.as_ref()?)?;
        }
        Some(e.path.clone())
    }

    /// Marked repos that are still listed, else the current one.
    pub fn selected(&self) -> Vec<usize> {
        let marked: Vec<usize> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Repo(i) if self.marked.contains(&self.ws.entries[*i].path) => Some(*i),
                _ => None,
            })
            .collect();
        if marked.is_empty() {
            self.current_repo().into_iter().collect()
        } else {
            marked
        }
    }

    fn visible_repos(&self) -> Vec<usize> {
        self.rows
            .iter()
            .filter_map(|r| if let Row::Repo(i) = r { Some(*i) } else { None })
            .collect()
    }

    /// What a bulk action ("all") acts on: the visible repos, less hidden
    /// ones even while they're shown. Choosing one explicitly still works.
    fn bulk_repos(&self) -> Vec<usize> {
        self.visible_repos()
            .into_iter()
            .filter(|&i| !self.ws.entries[i].flags.hidden())
            .collect()
    }

    fn info(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.remember_said(Happened::Said(s.clone()));
        self.message = Some((s, false));
    }

    /// Report a failure in the status line (e.g. a save that failed).
    pub fn report_error(&mut self, s: impl Into<String>) {
        self.error(s);
    }

    fn error(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.remember_said(Happened::Failed(s.clone()));
        self.message = Some((s, true));
    }

    /// The status line's message, for the history: unless it says again
    /// what it said last.
    fn remember_said(&mut self, said: Happened) {
        let last = self
            .history
            .iter()
            .rev()
            .find(|e| matches!(e.what, Happened::Said(_) | Happened::Failed(_)));
        if last.is_none_or(|e| e.what != said) {
            self.remember(said);
        }
    }

    pub fn remember(&mut self, what: Happened) {
        self.history.push(Event {
            at: jiff::Timestamp::now(),
            what,
        });
        if self.history.len() > 5000 {
            self.history.drain(..500);
        }
    }

    // ---------- keys ----------

    pub fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        let fx = self.key(k);
        self.show_waiting();
        fx
    }

    /// Ask `question` once nothing else is being asked, typed or chosen:
    /// at once from a key, but a question from the background (a forge's
    /// answer, a job's end) never replaces what the user is in.
    /// A pop (from the Stash menu or a fix) whose files could be restored
    /// but not its staging, which conflicts with what's staged now:
    /// popping the files alone is another thing (what the stash had
    /// staged, then changed again, isn't brought back), so it's asked.
    fn ask_to_pop_plain(&mut self, path: &Path, op: &Op, outcome: &OpOutcome) {
        let stash = match op {
            Op::StashPopAt { index, id } => Some((*index, Some(id.clone()))),
            Op::StashPop => {
                let ids = self.ws.get(path).and_then(Entry::status);
                Some((0, ids.and_then(|s| s.stash_ids.first().cloned())))
            }
            _ => None,
        };
        if let Some((index, Some(id))) = stash
            && !outcome.success
            && outcome.output.starts_with(ops::POP_STAGING_CONFLICTS)
        {
            self.ask_when_free(
                "Pop without what the stash had staged (it conflicts with what's \
                 staged now)? Anything staged, then changed again before stashing, is lost."
                    .into(),
                Confirmed::Ops(vec![(path.to_path_buf(), Op::StashPopPlain { index, id })]),
            );
        }
    }

    fn ask_when_free(&mut self, question: String, then: Confirmed) {
        self.waiting.push_back((question, then));
        self.show_waiting();
    }

    fn show_waiting(&mut self) {
        if matches!(self.mode, Mode::Normal)
            && let Some((question, then)) = self.waiting.pop_front()
        {
            self.mode = Mode::Confirm { question, then };
        }
    }

    /// Start what was agreed to while its repo was busy, now that it isn't.
    fn start_queued(&mut self) -> Vec<Effect> {
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queued)
            .into_iter()
            .partition(|(p, _)| !self.running.contains_key(p));
        self.queued = waiting;
        let jobs: Vec<Job> = ready
            .into_iter()
            .filter_map(|(p, op)| self.job(&p, op))
            .collect();
        match jobs.is_empty() {
            true => vec![],
            false => self.start_jobs(jobs),
        }
    }

    fn key(&mut self, k: KeyEvent) -> Vec<Effect> {
        if matches!(self.mode, Mode::Normal) {
            self.message = None;
        }
        match &mut self.mode {
            Mode::Normal => match self.normal_key(k) {
                Some(a) => self.act(a),
                None => vec![],
            },
            Mode::Pick {
                editor,
                names,
                shown,
                cursor,
                ..
            } => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let last = shown.len().saturating_sub(1);
                match k.code {
                    KeyCode::Down => *cursor = (*cursor + 1).min(last),
                    KeyCode::Char('n') if ctrl => *cursor = (*cursor + 1).min(last),
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Char('p') if ctrl => *cursor = cursor.saturating_sub(1),
                    _ => match editor.handle(k) {
                        Edit::Cancel => {
                            self.picks = None;
                            self.mode = Mode::Normal;
                        }
                        Edit::Submit => {
                            let picked = shown.get(*cursor).copied();
                            self.mode = Mode::Normal;
                            if let Some(i) = picked {
                                return self.act(Action::PickBranch(i));
                            }
                            self.picks = None;
                        }
                        Edit::Changed => {
                            let mut f = crate::fuzzy::Fuzzy::new(&editor.text());
                            *shown = f.rank(names.iter().map(String::as_str));
                            *cursor = 0;
                        }
                        _ => {}
                    },
                }
                vec![]
            }
            Mode::History { back } => {
                let page = self.page.max(1);
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let alt = k.modifiers.contains(KeyModifiers::ALT);
                match k.code {
                    // a page, as C-v and M-v move the list
                    KeyCode::Char('v') if ctrl => *back = back.saturating_sub(page),
                    KeyCode::Char('v') if alt => *back = back.saturating_add(page),
                    KeyCode::Up | KeyCode::Char('k' | 'p') => *back = back.saturating_add(1),
                    KeyCode::Down | KeyCode::Char('j' | 'n') => *back = back.saturating_sub(1),
                    KeyCode::PageUp => *back = back.saturating_add(page),
                    KeyCode::PageDown => *back = back.saturating_sub(page),
                    _ => self.mode = Mode::Normal,
                }
                vec![]
            }
            Mode::Help { scroll } => {
                match k.code {
                    // n/p with or without Ctrl, as Emacs's C-n/C-p
                    KeyCode::Down | KeyCode::Char('j' | 'n') => *scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k' | 'p') => *scroll = scroll.saturating_sub(1),
                    _ => self.mode = Mode::Normal,
                }
                vec![]
            }
            Mode::Menu { menu, cursor, .. } => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let n = menu.items.len();
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    KeyCode::Char('g') if ctrl => self.mode = Mode::Normal,
                    KeyCode::Down | KeyCode::Tab => *cursor = (*cursor + 1) % n,
                    KeyCode::Char('n') if ctrl => *cursor = (*cursor + 1) % n,
                    KeyCode::Up | KeyCode::BackTab => *cursor = (*cursor + n - 1) % n,
                    KeyCode::Char('p') if ctrl => *cursor = (*cursor + n - 1) % n,
                    KeyCode::Enter => {
                        let it = &menu.items[*cursor];
                        let (a, stays) = (it.action, it.stays);
                        if !stays {
                            self.mode = Mode::Normal;
                        }
                        return self.act(a);
                    }
                    // moving through a list of things
                    KeyCode::Char('n' | 'j') if !ctrl && menu.id.is_list() => {
                        *cursor = (*cursor + 1) % n
                    }
                    KeyCode::Char('p' | 'k') if !ctrl && menu.id.is_list() => {
                        *cursor = (*cursor + n - 1) % n
                    }
                    KeyCode::Char(c) if !ctrl => {
                        let at = menu.items.iter().position(|i| i.key == c);
                        let hidden =
                            |n: usize| self.menu_shown.as_ref().is_some_and(|r| !r.contains(&n));
                        if let Some(n) = at.filter(|&n| hidden(n)) {
                            // never act on what can't be seen: show it first
                            *cursor = n;
                        } else if let Some(it) = at.map(|n| &menu.items[n]) {
                            // switches keep the menu open, as in magit
                            let (a, stays) = (it.action, it.stays);
                            if !stays {
                                self.mode = Mode::Normal;
                            }
                            return self.act(a);
                        }
                    }
                    _ => {}
                }
                vec![]
            }
            Mode::Confirm { then, .. } => {
                let then = then.clone();
                self.mode = Mode::Normal;
                if k.code == KeyCode::Char('y') && k.modifiers.is_empty() {
                    return self.confirmed(then);
                }
                if then == Confirmed::EditAgain {
                    self.drop_edit();
                    self.info("dropped the edited patch");
                    return vec![];
                }
                self.info("cancelled");
                vec![]
            }
            Mode::Input { prompt, editor } => {
                let r = editor.handle(k);
                let text = editor.text();
                let prompt = prompt.clone();
                match r {
                    Edit::Cancel => {
                        if prompt == Prompt::Filter {
                            self.filter.clear();
                            self.rebuild();
                        }
                        self.mode = Mode::Normal;
                        vec![]
                    }
                    Edit::Submit => {
                        self.mode = Mode::Normal;
                        self.submit(prompt, text)
                    }
                    Edit::Changed if prompt == Prompt::Filter => {
                        self.filter = text;
                        self.rebuild();
                        vec![]
                    }
                    _ => vec![],
                }
            }
        }
    }

    fn normal_key(&mut self, k: KeyEvent) -> Option<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let g = std::mem::take(&mut self.pending_g);
        use Action::*;
        Some(match k.code {
            KeyCode::Up => Up,
            KeyCode::Down => Down,
            KeyCode::PageUp => PageUp,
            KeyCode::PageDown => PageDown,
            KeyCode::Home => Top,
            KeyCode::End => Bottom,
            KeyCode::Tab | KeyCode::Enter => ToggleExpand,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    ClearFilter
                } else {
                    Cancel
                }
            }
            // C-space (C-@; some terminals send NUL)
            KeyCode::Null => ToggleRegion,
            KeyCode::Char(c) if ctrl => match c {
                ' ' | '@' => ToggleRegion,
                'n' => Down,
                'p' => Up,
                'v' => PageDown,
                'd' => PageDown,
                'u' => PageUp,
                's' => StartFilter,
                'g' => Cancel,
                'c' => Quit,
                'l' => Refresh,
                _ => return None,
            },
            KeyCode::Char(c) if alt => match c {
                'v' => PageUp,
                '<' => Top,
                '>' => Bottom,
                'n' => NextRepo,
                'p' => PrevRepo,
                _ => return None,
            },
            KeyCode::Char(c) => match c {
                'j' => Down,
                'k' => Up,
                'n' => NextSection,
                'p' => PrevSection,
                'g' if g => Top,
                'g' => {
                    // `g` alone refreshes (magit); `gg` goes to top (vi). The
                    // refresh is cheap, so do it immediately and let a second
                    // `g` also jump.
                    self.pending_g = true;
                    Refresh
                }
                'G' => Bottom,
                '<' => Top,
                '>' => Bottom,
                ' ' => ToggleMark,
                's' => StageFiles,
                'u' => UnstageFiles,
                'K' => DiscardFiles,
                'e' => EditPatch,
                'M' => MarkAll,
                'U' => UnmarkAll,
                '/' => StartFilter,
                '?' | 'h' => Help,
                'q' => Quit,
                '$' => ToggleLog,
                'L' => History,
                '!' => Shell,
                'a' => ToggleAttention,
                'H' => ToggleHidden,
                'R' => Rescan,
                'i' => GitignoreMenu,
                c => {
                    let m = MenuId::from_key(c)?;
                    OpenMenu(m)
                }
            },
            _ => return None,
        })
    }

    // ---------- actions ----------

    pub fn act(&mut self, a: Action) -> Vec<Effect> {
        use Action::*;
        let last = self.rows.len().saturating_sub(1);
        match a {
            Up => self.cursor = self.cursor.saturating_sub(1),
            Down => self.cursor = (self.cursor + 1).min(last),
            PageUp => self.cursor = self.cursor.saturating_sub(self.page.max(1)),
            PageDown => self.cursor = (self.cursor + self.page.max(1)).min(last),
            Top => self.cursor = 0,
            Bottom => self.cursor = last,
            NextSection => {
                if let Some(i) = (self.cursor + 1..self.rows.len()).find(|&i| self.is_section(i)) {
                    self.cursor = i;
                }
            }
            PrevSection => {
                if let Some(i) = (0..self.cursor).rev().find(|&i| self.is_section(i)) {
                    self.cursor = i;
                }
            }
            NextRepo => {
                if let Some(i) = (self.cursor + 1..self.rows.len())
                    .find(|&i| matches!(self.rows[i], Row::Repo(_)))
                {
                    self.cursor = i;
                }
            }
            PrevRepo => {
                // from a detail line, first go to its own repo
                if let Some(i) = (0..self.cursor)
                    .rev()
                    .find(|&i| matches!(self.rows[i], Row::Repo(_)))
                {
                    self.cursor = i;
                }
            }
            ToggleExpand if self.folding().is_some() => {
                // a line with lines under it, or one of those: folds or
                // unfolds them, the cursor going to the line they're under
                if let Some((root, id)) = self.folding() {
                    let set = self.toggled.entry(root.clone()).or_default();
                    if !set.remove(&id) {
                        set.insert(id);
                    }
                    self.toggled.retain(|_, set| !set.is_empty());
                    self.rebuild();
                }
            }
            ToggleExpand => {
                if let Some(p) = self.listed_path() {
                    if !self.expanded.remove(&p) {
                        self.expanded.insert(p.clone());
                    }
                    self.rebuild();
                    // stay on the repo row when collapsing from a detail line
                    if let Some(i) = self
                        .rows
                        .iter()
                        .position(|r| matches!(r, Row::Repo(i) if self.ws.entries[*i].path == p))
                    {
                        self.cursor = i;
                    }
                    return self.opened();
                }
            }
            ExpandAll => {
                self.expanded = self
                    .visible_repos()
                    .into_iter()
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                self.rebuild();
                return self.opened();
            }
            CollapseAll => {
                self.expanded.clear();
                self.rebuild();
            }
            ToggleMark => {
                let in_changes = matches!(self.rows.get(self.cursor),
                    Some(Row::Detail { line, .. }) if line.section.is_some());
                // (every row of an untracked file's diff is the file: going
                // down it marks the file, and only its own row unmarks it)
                let in_diff = matches!(self.rows.get(self.cursor),
                    Some(Row::Detail { line, .. }) if line.diff.is_some());
                let stays = |t: &Target| {
                    in_diff && matches!(t, Target::File { .. }) && self.marked(t) != Marked::No
                };
                if let Some(target) = self.target(self.cursor).filter(|t| !stays(t)) {
                    self.toggle(target);
                }
                if in_changes {
                    // (a row with nothing of its own to mark, an unchanged
                    // line say, is only stepped over)
                    self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
                } else if let Some(p) = self.listed_path() {
                    // (the repo listed: one nested in it has no row to mark)
                    if !self.marked.remove(&p) {
                        self.marked.insert(p);
                    }
                    self.act(NextRepo);
                }
            }
            MarkAll => {
                // (hidden ones shown are left out, as of every "all")
                let listed = self.bulk_repos();
                let paths = listed.into_iter().map(|i| self.ws.entries[i].path.clone());
                self.marked.extend(paths);
            }
            UnmarkAll => {
                for i in self.visible_repos() {
                    self.marked.remove(&self.ws.entries[i].path);
                }
            }
            Cancel => {
                // a region and what's marked in the details, wherever (the
                // repos marked are U's to unmark)
                self.region = None;
                self.marked_files.clear();
                self.marked_lines.clear();
            }
            ToggleRegion => {
                if self.region.is_some() {
                    // ended: its lines kept as marks
                    self.keep_region();
                } else if let Some(Row::Detail { line, .. }) = self.rows.get(self.cursor)
                    && line.diff.is_some()
                    && line.section == Some(Section::Untracked)
                {
                    self.info("an untracked file is marked whole: space marks it");
                } else if let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor)
                    && let (Some(f), Some(d)) = (&line.file, line.diff.as_deref())
                    && let Some(root) = self.listed_path()
                {
                    // (the file's own line: the last file in its place)
                    let file_line = line.id.iter().rposition(|s| matches!(s, Seg::File(_)));
                    self.region = Some(RegionStart {
                        repo: self.ws.entries[*repo].path.clone(),
                        file: f.clone(),
                        side: d.side,
                        at: Self::diff_pos(line),
                        root,
                        home: line.id[..file_line.map_or(0, |n| n + 1)].to_vec(),
                    });
                } else {
                    self.info("C-space starts a region in a file's diff");
                }
            }
            Toggle(Switch::Untracked) => self.stash.untracked = !self.stash.untracked,
            Toggle(Switch::KeepIndex) => self.stash.keep_index = !self.stash.keep_index,
            Toggle(Switch::Message) => self.stash.message = !self.stash.message,
            IgnoreStep(which) => return self.ignore_step(which),
            GitignoreMenu => return self.open_gitignore_menu(),
            Toggle(Switch::Anchored) => {
                if let Some(g) = &mut self.gitignore {
                    g.anchored = !g.anchored;
                }
                // the items show the rules
                if let (Some(menu), Mode::Menu { menu: open, .. }) =
                    (self.gitignore_menu_items(), &mut self.mode)
                {
                    *open = menu;
                }
            }
            Gitignore(file) => {
                let Some(GitignoreTargets { targets, anchored }) = self.gitignore.take() else {
                    return vec![];
                };
                let jobs = targets
                    .into_iter()
                    .filter_map(|(p, paths)| {
                        self.job(
                            &p,
                            Op::Ignore {
                                file,
                                paths,
                                anchored,
                            },
                        )
                    })
                    .collect();
                return self.start_jobs(jobs);
            }
            Reset(mode) => return self.open_commit_pick(Pick::Reset(mode)),
            Amend => return self.start_amend(),
            ApplyFix(kind) => {
                return match kind {
                    FixKind::Abort => self.act(AbortOperation),
                    FixKind::Commit => self.act(Commit { all: true }),
                    FixKind::Gitignore => self.act(GitignoreMenu),
                    FixKind::Branch => self.act(CreateBranch),
                    FixKind::AddRemote => self.act(AddRemote),
                    FixKind::DeleteBranches => self.act(DeleteMergedBranches),
                    FixKind::RemoveWorktree => self.remove_worktrees(),
                    k => self.fix_ops(k),
                };
            }
            PickCommit(n) => return self.pick_commit(n),
            Fixup { squash, instant } => {
                return self.open_commit_pick(Pick::Fixup { squash, instant });
            }
            Checkout => return self.open_branch_pick(),
            PickBranch(n) => {
                if let Some((path, mut ops)) = self.picks.take()
                    && n < ops.len()
                {
                    return self.run_or_ask(path, ops.swap_remove(n));
                }
            }
            CheckoutPrevious => {
                let [i] = self.selected()[..] else {
                    self.error("check out one repo at a time");
                    return vec![];
                };
                let path = self.ws.entries[i].path.clone();
                let op = Op::Switch { branch: "-".into() };
                let jobs = self.job(&path, op).into_iter().collect();
                return self.start_jobs(jobs);
            }
            Stash(kind) => return self.stash_action(kind),
            StageFiles => return self.take_or_ask(HunkVerb::Stage),
            UnstageFiles => return self.take_or_ask(HunkVerb::Unstage),
            EditPatch => return self.edit_patch(),
            DiscardFiles => return self.take_or_ask(HunkVerb::Discard),
            // (with no repo under the cursor there's no "this repo")
            TakeHere(verb) => match self.current_path() {
                Some(here) => return self.take(verb, Some(here)),
                None => self.info(FILE_HINT),
            },
            TakeAll(verb) => return self.take(verb, None),
            StashAll => return self.stash_asked(StashKind::MarkedFiles),
            GitignoreIn(all) => return self.gitignore_menu_in(all),
            OpenMenu(m) => {
                let mut menu = m.menu();
                self.add_tags(&mut menu);
                self.state_labels(&mut menu);
                self.drop_inapplicable(&mut menu);
                let mut cursor = 0;
                if m == MenuId::Fix {
                    self.add_fixes(&mut menu);
                    // on the fix under the cursor in the details
                    if let Some(Row::Detail { line, .. }) = self.rows.get(self.cursor)
                        && let Some(k) = line.fix
                    {
                        let key = menu::fix_key(k);
                        cursor = menu.items.iter().position(|i| i.key == key).unwrap_or(0);
                    }
                }
                if menu.items.is_empty() {
                    return self.not_applicable("nothing to fix in the selection");
                }
                self.mode = Mode::Menu { menu, cursor }
            }
            StartFilter => {
                self.mode = Mode::Input {
                    prompt: Prompt::Filter,
                    editor: LineEditor::new(&self.filter),
                }
            }
            ClearFilter => {
                self.filter.clear();
                self.rebuild();
            }
            Help => self.mode = Mode::Help { scroll: 0 },
            History => self.mode = Mode::History { back: 0 },
            Quit => {
                self.quit = true;
                return vec![Effect::SaveState, Effect::Quit];
            }
            ToggleLog => self.show_log = !self.show_log,
            ToggleAttention => {
                self.view.attention_only = !self.view.attention_only;
                self.rebuild();
                self.info(if self.view.attention_only {
                    "showing repos needing attention"
                } else {
                    "showing all repos"
                });
            }
            NextTheme => {
                let ui = &mut self.ws.config.ui;
                ui.theme = next(crate::theme::BUILTIN, &ui.theme).to_string();
                let msg = format!("theme {} (this session; set [ui] theme to keep)", ui.theme);
                self.info(msg);
            }
            NextIcons => {
                let ui = &mut self.ws.config.ui;
                ui.icons = next(argus_core::icons::SETS, &ui.icons).to_string();
                let msg = format!("icons {} (this session; set [ui] icons to keep)", ui.icons);
                self.rebuild();
                self.info(msg);
            }
            ToggleHidden => {
                self.view.show_hidden = !self.view.show_hidden;
                self.rebuild();
                self.info(if self.view.show_hidden {
                    "showing hidden repos"
                } else {
                    "hiding ignored/snoozed repos"
                });
            }
            Refresh => {
                let paths: Vec<PathBuf> = self.ws.entries.iter().map(|e| e.path.clone()).collect();
                // and ask the forges again, of the repos whose details are open
                self.forge.clear();
                self.rebuild();
                let mut fx = vec![Effect::Refresh(paths)];
                fx.extend(self.forge_opened());
                return fx;
            }
            DropGone => {
                let gone: Vec<PathBuf> = self
                    .selected()
                    .into_iter()
                    .filter(|&i| self.droppable(i))
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                if gone.is_empty() {
                    return self.not_applicable("nothing selected is gone from disk");
                }
                for p in &gone {
                    self.ws.forget(p);
                    self.marked.remove(p);
                    self.marked_files.remove(p);
                    self.marked_lines.retain(|k| k.0 != *p);
                }
                self.rebuild();
                let what = match gone.as_slice() {
                    [one] => paths::contract_tilde(one),
                    all => format!("{} repos", all.len()),
                };
                self.info(format!(
                    "dropped {what} from the list (R finds it if it's back)"
                ));
            }
            Rescan => {
                self.scanning = true;
                return vec![Effect::Scan];
            }
            Fetch(scope) => return self.jobs_for(scope, |_| Some(Op::Fetch)),
            Pull(Scope::All) => {
                let jobs = pull::plan_updates(&self.ws);
                return self.start_jobs(jobs);
            }
            Pull(Scope::Selected) => {
                return self.jobs_for(Scope::Selected, |e| {
                    let s = e.status()?;
                    Some(if s.upstream.as_ref().is_some_and(|t| !t.gone) {
                        Op::Pull
                    } else {
                        Op::Fetch
                    })
                });
            }
            Push => return self.fix_ops(FixKind::Push),
            Publish => return self.fix_ops(FixKind::Publish),
            PushBranches => return self.fix_ops(FixKind::PushBranches),
            SwitchDefault => return self.fix_ops(FixKind::SwitchDefault),
            Reattach => return self.fix_ops(FixKind::Reattach),
            CreateBranch => {
                let targets = self.applicable(FixKind::Branch);
                if targets.is_empty() {
                    self.info("no detached HEAD with commits on no branch selected");
                } else {
                    self.mode = Mode::Input {
                        prompt: Prompt::BranchName(targets),
                        editor: LineEditor::default(),
                    };
                }
            }
            SpinOff { stay } => {
                let targets: Vec<PathBuf> = self
                    .selected()
                    .into_iter()
                    .map(|i| &self.ws.entries[i])
                    .filter(|e| e.status().is_some_and(|s| s.head.branch().is_some()))
                    .map(|e| e.path.clone())
                    .collect();
                if targets.is_empty() {
                    return self.not_applicable("not on a branch: nothing to spin off");
                }
                self.mode = Mode::Input {
                    prompt: Prompt::SpinOffName(targets, stay),
                    editor: LineEditor::default(),
                };
            }
            NewBranch => {
                let targets: Vec<PathBuf> = self
                    .selected()
                    .into_iter()
                    .map(|i| &self.ws.entries[i])
                    .filter(|e| e.kind != argus_core::discover::RepoKind::Bare)
                    .map(|e| e.path.clone())
                    .collect();
                if targets.is_empty() {
                    return self.not_applicable("a bare repo has no HEAD to branch from");
                }
                self.mode = Mode::Input {
                    prompt: Prompt::BranchName(targets),
                    editor: LineEditor::default(),
                };
            }
            AbortOperation => {
                let p = self.planned(FixKind::Abort);
                let ops = p.ops.clone();
                let question = match ops.as_slice() {
                    [] => return self.not_applicable("nothing in progress"),
                    [(p, op)] => format!(
                        "{} in {}? What it did so far (resolved conflicts, commits) is lost.",
                        capitalised(&op.label()),
                        self.name_of(p)
                    ),
                    _ => format!(
                        "Abort what's in progress in {} repos? What each did so far is lost.",
                        ops.len()
                    ),
                } + &p.note();
                self.mode = Mode::Confirm {
                    question,
                    then: Confirmed::Ops(ops),
                };
            }
            DeleteMergedBranches => {
                let p = self.planned(FixKind::DeleteBranches);
                let ops = p.ops.clone();
                if ops.is_empty() {
                    return self.not_applicable("no merged branches to delete");
                }
                let names: Vec<String> = ops
                    .iter()
                    .filter_map(|(_, op)| match op {
                        Op::DeleteBranch { branch, .. } => Some(branch.clone()),
                        _ => None,
                    })
                    .collect();
                // by content only: git's own check can't see those
                let forced = ops
                    .iter()
                    .filter(|(_, op)| {
                        matches!(
                            op,
                            Op::DeleteBranch {
                                force_at: Some(_),
                                ..
                            }
                        )
                    })
                    .count();
                let how = match forced {
                    0 => String::new(),
                    n if n == names.len() => " (squashed or rebased: their commits go)".into(),
                    n => format!(" ({n} squashed or rebased: their commits go)"),
                };
                self.mode = Mode::Confirm {
                    question: format!(
                        "Delete {} merged branch(es){how}: {}?{}",
                        names.len(),
                        fix::list(&names, 3),
                        p.note()
                    ),
                    then: Confirmed::Ops(ops),
                };
            }
            DeleteBranch => return self.delete_branch(),
            PopStash => {
                return self.stash_pick("Pop", |index, id| Op::StashPopAt { index, id });
            }
            DropStash => {
                return self.stash_pick("Drop", |index, id| Op::StashDrop { index, id });
            }
            AddRemote => match self.applicable(FixKind::AddRemote).as_slice() {
                [] => self.info("the selection has remotes"),
                [one] => {
                    self.mode = Mode::Input {
                        prompt: Prompt::RemoteUrl(one.clone()),
                        editor: LineEditor::default(),
                    }
                }
                _ => self.error("add a remote to one repo at a time"),
            },
            FastForward => return self.fix_ops(FixKind::FastForward),
            Commit { all } => {
                let targets: Vec<PathBuf> = self
                    .selected()
                    .into_iter()
                    .map(|i| &self.ws.entries[i])
                    .filter(|e| {
                        e.status().is_some_and(|s| match all {
                            true => !s.changes.is_clean(),
                            false => s.changes.staged > 0,
                        })
                    })
                    .map(|e| e.path.clone())
                    .collect();
                match (targets.is_empty(), all) {
                    (true, true) => self.info("nothing to commit"),
                    (true, false) => self.info("nothing staged (c a commits all changes)"),
                    (false, _) => {
                        self.mode = Mode::Input {
                            prompt: Prompt::CommitMessage(targets, all),
                            editor: LineEditor::default(),
                        }
                    }
                }
            }
            a @ (AutoFix(scope) | FixAuto(scope)) => {
                let auto = matches!(a, FixAuto(_));
                let idx = if scope == Scope::All {
                    self.bulk_repos()
                } else {
                    self.selected()
                };
                let kinds = self.ws.config.fix.auto.clone();
                let total = idx.len();
                let fixable: Vec<PathBuf> = idx
                    .into_iter()
                    .filter(|&i| {
                        let e = &self.ws.entries[i];
                        e.status().is_some_and(|s| {
                            fix::choose(s, &e.fix_kinds(&kinds), &self.ws.config.stash).is_some()
                        })
                    })
                    .map(|i| self.ws.entries[i].path.clone())
                    .collect();
                let none = skipped(total - fixable.len(), "with nothing to apply");
                let (paths, busy): (Vec<PathBuf>, Vec<PathBuf>) = fixable
                    .into_iter()
                    .partition(|p| !self.running.contains_key(p));
                let busy = skipped(busy.len(), "busy");
                if paths.is_empty() {
                    match busy.is_empty() {
                        true => self.info(format!("no auto-fixes ({}) apply", kinds_label(&kinds))),
                        false => self.info(format!("nothing started{busy}")),
                    }
                    return vec![];
                }
                let what = self.auto_fix_summary(&paths, auto).unwrap_or_default();
                for p in &paths {
                    self.running.insert(p.clone(), "fix".into());
                    match auto {
                        true => self.chains.insert(p.clone(), Chain::default()),
                        false => self.chains.remove(p),
                    };
                }
                // (with another fix action still going, they finish as one)
                self.auto_fixes = match self.auto_fixes.0 {
                    0 => (paths.len(), what.clone()),
                    n => (n + paths.len(), "fixes".into()),
                };
                self.info(format!("{what}…{none}{busy}"));
                return vec![Effect::Fix { paths, kinds }];
            }
            PullAndClone => {
                self.info("listing forges…");
                return vec![Effect::PlanPullAndClone];
            }
            ToggleLabel(i) => return self.toggle_tag(Tag::Label(i)),
            TogglePull => return self.toggle_tag(Tag::Pull),
            ToggleIgnore => return self.toggle_tag(Tag::Ignore),
            ToggleOwnerTag(m) => return self.toggle_owner_tag(m),
            IgnoreIssues => return self.open_ignore_menu(),
            ClearTags => {
                let keys = self.selected_keys();
                let n = self.ws.tagged_count(&self.selected());
                if n > 1 {
                    self.mode = Mode::Confirm {
                        question: format!("Clear tags of {n} repos?"),
                        then: Confirmed::ClearTags(keys),
                    };
                    return vec![];
                }
                return self.confirmed(Confirmed::ClearTags(keys));
            }
            Snooze if let Some((key, issue, ignored)) = self.issue_under_cursor() => {
                if ignored {
                    self.info("that issue is ignored: t i stops ignoring it");
                } else {
                    self.mode = Mode::Input {
                        prompt: Prompt::SnoozeIssue(key, issue),
                        editor: LineEditor::new("3d"),
                    };
                }
            }
            Unsnooze if let Some((key, issue, _)) = self.issue_under_cursor() => {
                self.ws.state.unsnooze_issue(&key, issue);
                return self.flags_changed();
            }
            Snooze => {
                let keys = self.selected_keys();
                if !keys.is_empty() {
                    self.mode = Mode::Input {
                        prompt: Prompt::SnoozeDuration(keys),
                        editor: LineEditor::new("3d"),
                    };
                }
            }
            Unsnooze => {
                for k in self.selected_keys() {
                    self.ws.state.unsnooze_all(&k);
                }
                return self.flags_changed();
            }
            Ack => {
                for k in self.selected_keys() {
                    self.ws.state.new.remove(&k);
                }
                return self.flags_changed();
            }
            Shell => {
                if let Some(e) = self.current() {
                    let shell = self
                        .ws
                        .config
                        .tools
                        .shell
                        .clone()
                        .or_else(|| std::env::var("SHELL").ok())
                        .unwrap_or_else(|| "/bin/sh".into());
                    return vec![Effect::Suspend {
                        program: shell,
                        args: vec![],
                        cwd: e.path.clone(),
                        login_shell: true,
                    }];
                }
            }
            GitUi => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                match self.ws.config.tools.git_ui.clone() {
                    Some(cmd) => return vec![shell_command(&cmd, e.path.clone())],
                    None => self.error("set tools.git_ui in config, e.g. \"lazygit\""),
                }
            }
            Editor => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                let ed = self
                    .ws
                    .config
                    .tools
                    .editor
                    .clone()
                    .or_else(|| std::env::var("VISUAL").ok())
                    .or_else(|| std::env::var("EDITOR").ok())
                    .unwrap_or_else(|| "vi".into());
                return vec![shell_command(&format!("{ed} ."), e.path.clone())];
            }
            Browse => {
                let Some(e) = self.current() else {
                    return vec![];
                };
                if let Some(p) = self.pull_under_cursor() {
                    return vec![Effect::Open(p.url.clone())];
                }
                match &e.id {
                    Some(id) => {
                        return vec![Effect::Open(format!(
                            "https://{}/{}/{}",
                            id.host, id.owner, id.name
                        ))];
                    }
                    None => self.error("no forge identity for this repo"),
                }
            }
        }
        vec![]
    }

    fn selected_keys(&self) -> Vec<String> {
        self.selected()
            .into_iter()
            .map(|i| self.ws.entries[i].key.clone())
            .collect()
    }

    /// The first selected repo's owner (`github.com/acme`), if it has one.
    fn owner(&self) -> Option<String> {
        let id = self.ws.entries[*self.selected().first()?].id.as_ref()?;
        Some(format!("{}/{}", id.host, id.owner))
    }

    fn toggle_owner_tag(&mut self, m: Tag) -> Vec<Effect> {
        // relative to the first selected repo's owner, as toggle_tag goes
        let sel = self.selected();
        let Some(on) = sel.first().and_then(|&i| self.ws.owner_tagged(i, m)) else {
            return vec![];
        };
        let before = self.ws.config.clone();
        if let Err(e) = self.ws.set_owner_tag(&sel, m, !on) {
            self.error(e.to_string());
            return vec![];
        }
        self.rebuild();
        vec![Effect::SaveTags(Box::new(before))]
    }

    fn toggle_tag(&mut self, m: Tag) -> Vec<Effect> {
        // Toggle relative to the first selected repo so a mixed selection converges.
        let sel = self.selected();
        let Some(&first) = sel.first() else {
            return vec![];
        };
        let on = !m.of(&self.ws.entries[first].flags, &self.ws.config);
        let before = self.ws.config.clone();
        if let Err(e) = self.ws.set_tag(&sel, m, on) {
            self.error(e.to_string());
            return vec![];
        }
        self.rebuild();
        vec![Effect::SaveTags(Box::new(before))]
    }

    /// What opened details set going in the background.
    fn opened(&mut self) -> Vec<Effect> {
        let mut fx = self.describe_opened();
        fx.extend(self.forge_opened());
        fx
    }

    /// Questions for the forges of repos whose details are open, about
    /// each one not asked yet, or asked about other branches than it has
    /// now. What was known stays shown until the answer comes.
    fn forge_opened(&mut self) -> Vec<Effect> {
        if !self.ws.config.ui.forge_info {
            return vec![];
        }
        let mut fx = vec![];
        for p in &self.expanded {
            let Some(e) = self.ws.get(p) else { continue };
            let (Some(id), Some(s)) = (&e.id, e.status()) else {
                continue;
            };
            let Some(kind) = self.ws.config.forge_kind(&id.host) else {
                continue;
            };
            let branches = argus_core::forge::branches_to_ask(s);
            let look = self.forge.entry(p.clone()).or_default();
            if look.asked.as_ref() == Some(&branches) {
                continue;
            }
            look.asked = Some(branches.clone());
            fx.push(Effect::Forge {
                path: p.clone(),
                kind: kind.to_string(),
                id: id.clone(),
                branches,
                owners: argus_core::forge::head_owners(id, s),
            });
        }
        if !fx.is_empty() {
            // the details say it's being looked up
            self.rebuild();
        }
        fx
    }

    /// What to show of the forge of the repo at `path`.
    fn forge_view(&self, path: &std::path::Path) -> Option<crate::details::ForgeView<'_>> {
        use crate::details::ForgeView;
        let look = self.forge.get(path)?;
        match &look.answer {
            Some(Ok(Some(info))) => Some(ForgeView::Info(info)),
            Some(Ok(None)) => None,
            Some(Err(e)) => Some(ForgeView::Failed(e)),
            None => look.asked.is_some().then_some(ForgeView::Loading),
        }
    }

    /// The pull request of the branch under the cursor, as far as known.
    fn pull_under_cursor(&self) -> Option<&argus_core::forge::PullRequest> {
        let Some(Row::Detail { line, .. }) = self.rows.get(self.cursor) else {
            return None;
        };
        let look = self.forge.get(&self.current()?.path)?;
        match &look.answer {
            Some(Ok(Some(info))) => info
                .pull(line.branch.as_deref()?)
                .filter(|p| !p.url.is_empty()),
            _ => None,
        }
    }

    /// Full describes for repos whose details are open and whose walk for
    /// a tag gave up at `describe_limit`, unless one is already running.
    fn describe_opened(&mut self) -> Vec<Effect> {
        let wanted: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|p| !self.describing.contains(*p))
            .filter(|p| {
                self.ws
                    .get(p)
                    .and_then(|e| e.status())
                    .is_some_and(|s| s.describe_gave_up)
            })
            .cloned()
            .collect();
        wanted
            .into_iter()
            .map(|p| {
                self.describing.insert(p.clone());
                Effect::Describe(p)
            })
            .collect()
    }

    /// `other_pin`: pin the other way from the list's default.
    /// The ignore menu for the repo under the cursor, on the issue under
    /// it if that's one.
    fn open_ignore_menu(&mut self) -> Vec<Effect> {
        let (i, at) = match self.rows.get(self.cursor) {
            Some(Row::Repo(i)) => (*i, None),
            Some(Row::Detail { repo, line }) => (*repo, line.issue.and_then(IssueList::of)),
            None => return vec![],
        };
        let path = self.ws.entries[i].path.clone();
        self.ignore_menu = Some(path);
        let Some(menu) = self.ignore_menu_items() else {
            self.ignore_menu = None;
            return self.not_applicable("no issue of this repo can be ignored");
        };
        let lists = self.ignorable(i);
        let cursor = at
            .and_then(|l| lists.iter().position(|(m, _)| *m == l))
            .unwrap_or(0);
        self.mode = Mode::Menu { menu, cursor };
        vec![]
    }

    /// The gitignore menu for the untracked files marked, or under the
    /// cursor (tracked ones left out: rules don't apply to them).
    fn open_gitignore_menu(&mut self) -> Vec<Effect> {
        // (untracked files marked in a repo the cursor isn't in: which?)
        let untracked = |files: &BTreeSet<(Section, String)>| {
            files.iter().any(|(s, _)| *s == Section::Untracked)
        };
        // (only those are marks to it: its repos are the ones with some)
        let marks = self
            .marked_files
            .iter()
            .filter(|(_, files)| untracked(files));
        let marks: BTreeSet<PathBuf> = marks.map(|(repo, _)| repo.clone()).collect();
        let this = Some(('i', Action::GitignoreIn(false)));
        match self.asks_which("Gitignore", marks, this, Action::GitignoreIn(true)) {
            true => vec![],
            false => self.gitignore_menu_in(true),
        }
    }

    /// The gitignore menu for what's marked in every repo (`all`) or in
    /// the cursor's alone, else for what the cursor is on.
    fn gitignore_menu_in(&mut self, all: bool) -> Vec<Effect> {
        // the untracked files marked; else the file whose row (any of its
        // rows) the cursor is on; else, outside the changes, all of them
        let here = self.current_path();
        let marked: Vec<(usize, Vec<String>)> = self
            .marked_files
            .iter()
            .filter(|(repo, _)| all || here.as_ref() == Some(*repo))
            .filter_map(|(repo, files)| {
                let untracked = files.iter().filter(|(s, _)| *s == Section::Untracked);
                let files: Vec<String> = untracked.map(|(_, f)| f.clone()).collect();
                Some((self.ws.index_of(repo)?, files)).filter(|(_, f)| !f.is_empty())
            })
            .collect();
        let files = match (marked.is_empty(), self.rows.get(self.cursor)) {
            (false, _) => marked,
            (true, Some(Row::Detail { repo, line })) if line.section.is_some() => {
                match (&line.file, Section::heads(line)) {
                    (Some(file), _) => vec![(*repo, vec![file.clone()])],
                    (None, Some(Section::Untracked)) => {
                        return self.open_gitignore_menu_for_repos();
                    }
                    _ => vec![(*repo, vec![])],
                }
            }
            (true, _) => return self.open_gitignore_menu_for_repos(),
        };
        let targets: Vec<(PathBuf, Vec<String>)> = files
            .into_iter()
            .filter_map(|(i, files)| {
                let e = &self.ws.entries[i];
                let s = e.status()?;
                let untracked: Vec<String> = files
                    .into_iter()
                    .filter(|f| s.changes.entry(f).is_some_and(|c| c.index == '?'))
                    .collect();
                (!untracked.is_empty()).then(|| (e.path.clone(), untracked))
            })
            .collect();
        if targets.is_empty() {
            return self.not_applicable(
                "no untracked file selected: put the cursor on one in the details, or mark some",
            );
        }
        self.show_gitignore_menu(targets)
    }

    /// The current repo's recent commits to reset to: HEAD first, each
    /// marked when it's on a remote. The cursor starts on the commit before
    /// HEAD, or before the commit under the cursor in the details.
    /// The branches to check out: local ones but the current, then remote
    /// ones with no local copy (switching to one creates a local branch
    /// tracking it).
    fn open_branch_pick(&mut self) -> Vec<Effect> {
        let [i] = self.selected()[..] else {
            self.error("check out one repo at a time");
            return vec![];
        };
        let e = &self.ws.entries[i];
        let Some(s) = e.status() else {
            return vec![];
        };
        // (name, label, what checking it out runs)
        let mut lines: Vec<(String, String, Op)> = s
            .branches
            .iter()
            .filter(|b| !b.is_head)
            .map(|b| {
                let op = Op::Switch {
                    branch: b.name.clone(),
                };
                (b.name.clone(), b.name.clone(), op)
            })
            .collect();
        match argus_core::status::remote_only_branches(&e.path) {
            Ok(remote) => lines.extend(remote.into_iter().map(|r| {
                let label = format!("{r} (creates a local branch tracking it)");
                (r.clone(), label, Op::SwitchTrack { remote_branch: r })
            })),
            Err(err) => {
                self.error(err.to_string());
                return vec![];
            }
        }
        if lines.is_empty() {
            return self.not_applicable("no other branch to check out");
        }
        let title = format!("Checkout · {}", e.display_name());
        let path = e.path.clone();
        let (mut names, mut labels, mut ops) = (vec![], vec![], vec![]);
        for (n, l, op) in lines {
            names.push(n);
            labels.push(l);
            ops.push(op);
        }
        self.picks = Some((path, ops));
        self.mode = Mode::Pick {
            title,
            editor: LineEditor::new(""),
            shown: (0..names.len()).collect(),
            labels,
            names,
            cursor: 0,
        };
        vec![]
    }

    /// The list of HEAD's recent commits to reset to or fix up: from the
    /// commit under the cursor in the details if there is one (for a
    /// reset, its parent: what's left without it), else HEAD's parent for
    /// a reset and HEAD for a fixup.
    fn open_commit_pick(&mut self, purpose: Pick) -> Vec<Effect> {
        use argus_core::history::{self, Range};
        let verb = match purpose {
            Pick::Reset(_) => "reset",
            Pick::Fixup { squash: false, .. } => "fix up",
            Pick::Fixup { squash: true, .. } => "squash into",
        };
        let [i] = self.selected()[..] else {
            self.error(format!("{verb} one repo at a time"));
            return vec![];
        };
        let e = &self.ws.entries[i];
        if e.status()
            .is_some_and(|s| matches!(s.head, argus_core::status::Head::Unborn(_)))
        {
            return self.not_applicable(match purpose {
                Pick::Reset(_) => "no commits to reset to",
                Pick::Fixup { .. } => "no commits to fix up",
            });
        }
        let head = Range::Ancestors { tip: "HEAD".into() };
        // HEAD's history: never empty
        let commits = match history::commits(&e.path, &head, menu::LIST_KEYS.len()) {
            Ok((v, _)) => v,
            Err(err) => {
                self.error(err);
                return vec![];
            }
        };
        let local = history::commits(
            &e.path,
            &Range::NotOnRemote { tip: "HEAD".into() },
            menu::LIST_KEYS.len(),
        )
        .map(|(v, _)| v.into_iter().map(|c| c.id).collect::<Vec<_>>())
        .unwrap_or_default();
        let under = match self.rows.get(self.cursor) {
            Some(Row::Detail { line, .. }) => match &line.expand {
                Some(Expand::Commit(id)) => commits.iter().position(|c| c.id == *id),
                _ => None,
            },
            _ => None,
        };
        let after = usize::from(matches!(purpose, Pick::Reset(_)));
        let cursor = under
            .map_or(after, |n| n + after)
            .min(commits.len().saturating_sub(1));
        let items = commits
            .iter()
            .zip(menu::LIST_KEYS.chars())
            .enumerate()
            .map(|(n, (c, key))| {
                let mut label = format!("{} {}", &c.id[..7], c.summary);
                if n == 0 {
                    label.push_str(" (HEAD)");
                }
                if !local.contains(&c.id) {
                    label.push_str(" · on a remote");
                }
                menu::item(key, label, Action::PickCommit(n))
            })
            .collect();
        let title = match purpose {
            Pick::Reset(mode) => format!(
                "Reset {} · {} · to which commit?",
                mode.flag(),
                e.display_name()
            ),
            Pick::Fixup { squash, instant } => format!(
                "{} · {} · of which commit?",
                match (instant, squash) {
                    (false, false) => "Fixup",
                    (true, false) => "Instant fixup",
                    (false, true) => "Squash",
                    (true, true) => "Instant squash",
                },
                e.display_name()
            ),
        };
        let menu = Menu {
            id: MenuId::PickCommit,
            title,
            items,
        };
        self.pick = Some(CommitPick {
            path: e.path.clone(),
            name: e.display_name(),
            purpose,
            commits,
        });
        self.mode = Mode::Menu { menu, cursor };
        vec![]
    }

    /// Ask for the message to amend the current repo's last commit with,
    /// starting from its own summary line.
    fn start_amend(&mut self) -> Vec<Effect> {
        use argus_core::history::{self, Range};
        let [i] = self.selected()[..] else {
            self.error("amend one repo at a time");
            return vec![];
        };
        let path = self.ws.entries[i].path.clone();
        let message = match history::commit_detail(&path, "HEAD") {
            Ok(d) => d.message,
            Err(e) => {
                self.error(e);
                return vec![];
            }
        };
        let local = history::commits(&path, &Range::NotOnRemote { tip: "HEAD".into() }, 1)
            .is_ok_and(|(v, _)| !v.is_empty());
        let summary = message.lines().next().unwrap_or_default().to_string();
        self.mode = Mode::Input {
            prompt: Prompt::AmendMessage(path, message, !local),
            editor: LineEditor::new(&summary),
        };
        vec![]
    }

    /// Reset to the reset menu's commit `n`; a hard reset asks first.
    fn pick_commit(&mut self, n: usize) -> Vec<Effect> {
        let Some(CommitPick {
            path,
            name,
            purpose,
            commits,
        }) = self.pick.take()
        else {
            return vec![];
        };
        let Some(c) = commits.get(n) else {
            return vec![];
        };
        let mode = match purpose {
            Pick::Reset(mode) => mode,
            // a squash commit's message is asked for first
            Pick::Fixup {
                squash: true,
                instant,
            } => {
                self.mode = Mode::Input {
                    prompt: Prompt::SquashMessage(path, c.id.clone(), instant),
                    editor: LineEditor::default(),
                };
                return vec![];
            }
            Pick::Fixup { instant, .. } => {
                let op = Op::Fixup {
                    commit: c.id.clone(),
                    squash: None,
                    instant,
                };
                let jobs = self.job(&path, op).into_iter().collect();
                return self.start_jobs(jobs);
            }
        };
        let op = Op::Reset {
            mode,
            commit: c.id.clone(),
        };
        // what can't be had back: the work tree's changes (hard), or what's
        // staged of a file changed again since (mixed: the index goes)
        let restaged: Vec<String> = match self.ws.get(&path).and_then(|e| e.status()) {
            Some(s) if mode == ResetMode::Mixed => {
                // (staged for deletion and still there: nothing is lost)
                let both = |f: &&argus_core::status::FileChange| {
                    !matches!(f.index, ' ' | '?') && f.worktree != ' ' && !f.untracked()
                };
                let files = s.changes.files.iter().filter(both);
                files.map(|f| f.path.clone()).collect()
            }
            _ => vec![],
        };
        let lost = match mode {
            ResetMode::Hard => Some("Uncommitted changes are lost.".to_string()),
            _ if !restaged.is_empty() => Some(format!(
                "What's staged of {} differs from the work tree and is lost.",
                fix::list(&restaged, 3)
            )),
            _ => None,
        };
        if let Some(lost) = lost {
            self.mode = Mode::Confirm {
                question: format!(
                    "Reset {name} {} to {} ({})? {lost}",
                    mode.flag(),
                    &c.id[..7],
                    c.summary
                ),
                then: Confirmed::Ops(vec![(path, op)]),
            };
            return vec![];
        }
        let jobs = self.job(&path, op).into_iter().collect();
        self.start_jobs(jobs)
    }

    /// The gitignore menu for every untracked path of the selected repos
    /// (the gitignore fix), when no file is marked or under the cursor.
    fn open_gitignore_menu_for_repos(&mut self) -> Vec<Effect> {
        let targets: Vec<(PathBuf, Vec<String>)> = self
            .selected()
            .into_iter()
            .filter_map(|i| {
                let e = &self.ws.entries[i];
                let f = fix::plan(e.status()?, &self.ws.config.stash)
                    .into_iter()
                    .find(|f| f.kind == FixKind::Gitignore)?;
                match f.op {
                    Op::Ignore { paths, .. } => Some((e.path.clone(), paths)),
                    _ => None,
                }
            })
            .collect();
        if targets.is_empty() {
            return self.not_applicable("nothing untracked to ignore");
        }
        self.show_gitignore_menu(targets)
    }

    fn show_gitignore_menu(&mut self, targets: Vec<(PathBuf, Vec<String>)>) -> Vec<Effect> {
        self.gitignore = Some(GitignoreTargets {
            targets,
            anchored: true,
        });
        if let Some(menu) = self.gitignore_menu_items() {
            self.mode = Mode::Menu { menu, cursor: 0 };
        }
        vec![]
    }

    /// The gitignore menu: each place, with the rules it would get.
    fn gitignore_menu_items(&self) -> Option<Menu> {
        use argus_core::gitignore::{dir_of, rule};
        let GitignoreTargets { targets, anchored } = self.gitignore.as_ref()?;
        let paths: Vec<&String> = targets.iter().flat_map(|(_, p)| p).collect();
        let rules = |base: &dyn Fn(&str) -> String| -> String {
            let r: Vec<String> = paths.iter().map(|p| rule(p, &base(p), *anchored)).collect();
            fix::list(&r, 2)
        };
        let top = rules(&|_| String::new());
        let mut dirs: Vec<&str> = paths.iter().map(|p| dir_of(p)).collect();
        dirs.dedup();
        let own = rules(&|p| dir_of(p).to_string());
        let dir_label = match dirs.as_slice() {
            [d] if targets.len() == 1 => {
                let at = if d.is_empty() {
                    String::new()
                } else {
                    format!("{d}/")
                };
                format!("add {own} to {at}.gitignore")
            }
            _ => format!("add {own} to each file's directory's .gitignore"),
        };
        let n = paths.len();
        Some(Menu {
            id: MenuId::Gitignore,
            title: format!("Gitignore · {n} untracked file(s)"),
            items: vec![
                menu::item(
                    't',
                    format!("add {top} to .gitignore"),
                    Action::Gitignore(IgnoreFile::TopLevel),
                ),
                menu::item('d', dir_label, Action::Gitignore(IgnoreFile::Directory)),
                menu::item(
                    'p',
                    format!("add {top} to .git/info/exclude (this clone only)"),
                    Action::Gitignore(IgnoreFile::Private),
                ),
                menu::item(
                    'g',
                    format!("add {top} to the global excludes file (every repo)"),
                    Action::Gitignore(IgnoreFile::Global),
                ),
                menu::switch(
                    'a',
                    "anchored: only this path, not the name anywhere",
                    Switch::Anchored,
                ),
            ],
        })
    }

    /// Repo `i`'s issues that can be ignored, counted or not, one per
    /// list (stashes may be both), in the order the details show them.
    fn ignorable(&self, i: usize) -> Vec<(IssueList, Issue)> {
        let e = &self.ws.entries[i];
        let mut v: Vec<(IssueList, Issue)> = vec![];
        for &issue in e.issues.iter().chain(&e.ignored_issues) {
            let Some(list) = IssueList::of(issue) else {
                continue;
            };
            if v.iter().all(|(l, _)| *l != list) {
                v.push((list, issue));
            }
        }
        // all of them, ignored or not
        if let Some(s) = e.status() {
            for (list, issue) in &mut v {
                if *list == IssueList::Stashes {
                    *issue = Issue::Stashes(s.stashes.len());
                }
            }
        }
        // a fixed order, so stepping an issue never renumbers the menu
        v.sort_by_key(|(l, _)| IssueList::ALL.iter().position(|a| a == l));
        v
    }

    /// The ignore menu for [`Self::ignore_menu`]: each issue with a symbol
    /// and words for its state, and what `a` does. `None` if it has none.
    fn ignore_menu_items(&self) -> Option<Menu> {
        let i = self.ws.index_of(self.ignore_menu.as_ref()?)?;
        let e = &self.ws.entries[i];
        let s = e.status()?;
        let lists = self.ignorable(i);
        if lists.is_empty() {
            return None;
        }
        let (icons, _) = self.ws.config.icons();
        let (mut counted, mut ignored) = (0, 0);
        let mut items: Vec<Item> = vec![];
        for (n, (list, issue)) in lists.iter().enumerate() {
            let state = self.ws.issue_state(i, *list).ok()?;
            match state {
                IgnoreState::Counted => counted += 1,
                _ => ignored += 1,
            }
            let key = char::from_digit(n as u32 + 1, 10).unwrap_or('?');
            // stashes the patterns ignore while the issue still counts
            // (read only in that state)
            let partly = match list {
                IssueList::Stashes => e.stash_ignored.iter().filter(|&&b| b).count(),
                _ => 0,
            };
            let label = state_label(*issue, *list, state, partly, s, &icons);
            items.push(menu::stepper(
                key,
                label,
                Action::IgnoreStep(IgnoreWhich::One(n)),
            ));
        }
        if counted > 0 {
            items.push(menu::stepper(
                'a',
                format!("all: ignore {counted} remaining issue(s)"),
                Action::IgnoreStep(IgnoreWhich::All),
            ));
        }
        if ignored > 0 {
            items.push(menu::stepper(
                'n',
                format!("none: stop ignoring {ignored} issue(s)"),
                Action::IgnoreStep(IgnoreWhich::None),
            ));
        }
        Some(Menu {
            id: MenuId::Ignore,
            title: format!("Ignore issues · {}", e.display_name()),
            items,
        })
    }

    /// Move the ignore menu's issues as `which` says; the menu stays open,
    /// showing the result.
    fn ignore_step(&mut self, which: IgnoreWhich) -> Vec<Effect> {
        let Some(i) = self.ignore_menu.as_ref().and_then(|p| self.ws.index_of(p)) else {
            return vec![];
        };
        let lists = self.ignorable(i);
        let mut changes = vec![];
        for (n, (list, _)) in lists.iter().enumerate() {
            let Ok(now) = self.ws.issue_state(i, *list) else {
                continue;
            };
            let counted = now == IgnoreState::Counted;
            match which {
                IgnoreWhich::One(w) if w == n => changes.push((*list, now.next(*list))),
                IgnoreWhich::All if counted => {
                    changes.push((*list, IgnoreState::default_for(*list)))
                }
                IgnoreWhich::None if !counted => changes.push((*list, IgnoreState::Counted)),
                _ => {}
            }
        }
        if changes.is_empty() {
            return vec![];
        }
        let before = self.ws.config.clone();
        for (list, to) in &changes {
            if let Err(e) = self.ws.set_issue_state(&[i], *list, *to) {
                self.error(e.to_string());
                return vec![];
            }
        }
        self.rebuild();
        if let (Some(menu), Mode::Menu { menu: open, .. }) =
            (self.ignore_menu_items(), &mut self.mode)
        {
            *open = menu;
        }
        vec![Effect::SaveTags(Box::new(before))]
    }

    /// What fixing the repos at `paths` would do, once or (`auto`) until
    /// nothing more applies: `fix once acme/w: push`. `None` for nothing.
    fn auto_fix_summary(&self, paths: &[PathBuf], auto: bool) -> Option<String> {
        let (_, t) = self.auto_fix_tally(paths, auto)?;
        Some(format!("fix {} {t}", if auto { "auto" } else { "once" }))
    }

    /// How many of the repos at `paths` auto-fix would fix, and how:
    /// `acme/w: push` (with `auto`, every fix it's expected to get
    /// through: `acme/w: push, then switch main`), or how many of each
    /// kind for several (`3 repo(s): push (2), stash (1)`). `None` for none.
    fn auto_fix_tally(&self, paths: &[PathBuf], auto: bool) -> Option<(usize, String)> {
        let kinds = &self.ws.config.fix.auto;
        let fixes: Vec<(String, Vec<fix::Fix>)> = paths
            .iter()
            .filter_map(|p| {
                let e = self.ws.get(p)?;
                let (s, kinds, stash) = (e.status()?, e.fix_kinds(kinds), &self.ws.config.stash);
                let v = match auto {
                    true => fix::chain(s, &kinds, stash),
                    false => fix::choose(s, &kinds, stash).into_iter().collect(),
                };
                (!v.is_empty()).then(|| (e.display_name(), v))
            })
            .collect();
        match fixes.as_slice() {
            [] => None,
            [(name, v)] => {
                let labels: Vec<String> = v.iter().map(|f| f.op.label()).collect();
                Some((1, format!("{name}: {}", labels.join(", then "))))
            }
            _ => {
                // how many of each kind, in priority order
                let mut counts: Vec<(FixKind, usize)> = vec![];
                for f in fixes.iter().flat_map(|(_, v)| v) {
                    match counts.iter_mut().find(|c| c.0 == f.kind) {
                        Some(c) => c.1 += 1,
                        None => counts.push((f.kind, 1)),
                    }
                }
                let tally: Vec<String> = counts
                    .iter()
                    .map(|(k, n)| format!("{} ({n})", k.name()))
                    .collect();
                Some((
                    fixes.len(),
                    format!("{} repo(s): {}", fixes.len(), tally.join(", ")),
                ))
            }
        }
    }

    /// One of the last fix action's repos has nothing more to run: say so
    /// once they all have.
    fn fix_finished(&mut self) {
        self.auto_fixes.0 = self.auto_fixes.0.saturating_sub(1);
        if self.auto_fixes.0 == 0 && self.message.as_ref().is_none_or(|m| !m.1) {
            let what = std::mem::take(&mut self.auto_fixes.1);
            self.info(format!("{what}: done"));
        }
    }

    /// The next fix for the repo at `path`, if a fix-auto action is
    /// working through it and its last fix has finished: chosen from its
    /// status as just read. With none (or one it has applied already),
    /// the repo is finished.
    fn continue_chain(&mut self, path: &std::path::Path) -> Vec<Effect> {
        let Some(chain) = self.chains.get(path) else {
            return vec![];
        };
        if self.running.contains_key(path) {
            return vec![];
        }
        let kinds = self.ws.config.fix.auto.clone();
        let next = self
            .ws
            .get(path)
            .and_then(|e| fix::choose(e.status()?, &e.fix_kinds(&kinds), &self.ws.config.stash));
        // the fix just applied, wanted again: most likely a status read
        // before it finished and delivered after. Read once more before
        // taking it as "no progress".
        let stale = next
            .as_ref()
            .zip(chain.done.last())
            .is_some_and(|(n, last)| n.op == last.op);
        if stale && !chain.read_again {
            self.chains
                .entry(path.to_path_buf())
                .or_default()
                .read_again = true;
            return vec![Effect::Refresh(vec![path.to_path_buf()])];
        }
        if next.is_none_or(|f| fix::repeats(&chain.done, &f)) {
            self.chains.remove(path);
            self.fix_finished();
            return vec![];
        }
        self.running.insert(path.to_path_buf(), "fix".into());
        vec![Effect::Fix {
            paths: vec![path.to_path_buf()],
            kinds,
        }]
    }

    /// The Fix menu's items: auto-fix for the selection and for all shown,
    /// saying what each would do (when it would do something); then each
    /// fix that applies to the selection, with its reason for one repo or
    /// how many it applies to.
    fn add_fixes(&self, menu: &mut Menu) {
        let paths = |idx: Vec<usize>| -> Vec<PathBuf> {
            idx.into_iter()
                .map(|i| self.ws.entries[i].path.clone())
                .collect()
        };
        let (selected, all) = (paths(self.selected()), paths(self.bulk_repos()));
        for (auto, how, keys) in [(false, "once", ['x', 'a']), (true, "auto", ['X', 'A'])] {
            let (one, every) = match auto {
                true => (
                    Action::FixAuto(Scope::Selected),
                    Action::FixAuto(Scope::All),
                ),
                false => (
                    Action::AutoFix(Scope::Selected),
                    Action::AutoFix(Scope::All),
                ),
            };
            if let Some((_, t)) = self.auto_fix_tally(&selected, auto) {
                menu.items
                    .push(menu::item(keys[0], format!("fix {how} {t}"), one));
            }
            if let Some((_, t)) = self.auto_fix_tally(&all, auto) {
                menu.items
                    .push(menu::item(keys[1], format!("fix all {how} · {t}"), every));
            }
        }
        let selected = self.selected();
        let plans: Vec<Vec<fix::Fix>> = selected
            .iter()
            .map(|&i| self.fixes_for(i).into_iter().map(|(_, f)| f).collect())
            .collect();
        for kind in FixKind::ALL {
            let of_kind: Vec<&fix::Fix> = plans
                .iter()
                .filter_map(|p| p.iter().find(|f| f.kind == kind))
                .collect();
            let Some(f) = of_kind.first() else { continue };
            let name = match kind.manual() {
                true => format!("{}…", kind.name()),
                false => kind.name().to_string(),
            };
            let label = match selected.len() {
                1 => format!("{name}: {}", f.reason),
                n => format!("{name} ({} of {n} repos)", of_kind.len()),
            };
            menu.items.push(menu::item(
                menu::fix_key(kind),
                label,
                Action::ApplyFix(kind),
            ));
        }
        // gone with nothing to prune: acknowledged by dropping or rescanning
        if selected.iter().any(|&i| self.droppable(i)) {
            menu.items.push(menu::item(
                'g',
                "drop from the list (its folder is gone)",
                Action::DropGone,
            ));
            menu.items
                .push(menu::item('G', "rescan roots", Action::Rescan));
        }
    }

    /// After `op` succeeded in the repo at `path`: drop the rows of the
    /// worktrees it removed or pruned, which nothing need acknowledge.
    /// Returns whether any went.
    fn drop_removed(&mut self, path: &std::path::Path, op: &Op) -> bool {
        let gone: Vec<PathBuf> = match op {
            Op::RemoveWorktree => vec![path.to_path_buf()],
            Op::PruneWorktree { path } => vec![path.clone()],
            // its status from before still names them
            Op::PruneWorktrees => self
                .ws
                .get(path)
                .and_then(Entry::status)
                .map(|s| s.stale_worktrees.clone())
                .unwrap_or_default(),
            _ => return false,
        };
        for p in &gone {
            self.ws.forget(p);
            self.marked.remove(p);
        }
        !gone.is_empty()
    }

    /// The fixes for the repo at `i`, with where each runs: its status's;
    /// for a worktree whose folder is gone, pruning its record, in its
    /// main repo.
    fn fixes_for(&self, i: usize) -> Vec<(PathBuf, fix::Fix)> {
        let e = &self.ws.entries[i];
        if let Some(s) = e.status() {
            return fix::plan(s, &self.ws.config.stash)
                .into_iter()
                .map(|f| (e.path.clone(), f))
                .collect();
        }
        let Some(main) = self.stale_main(i) else {
            return vec![];
        };
        let fix = fix::Fix {
            kind: FixKind::PruneWorktrees,
            op: Op::PruneWorktree {
                path: e.path.clone(),
            },
            reason: "its folder is gone".into(),
            blocked: None,
        };
        vec![(main, fix)]
    }

    /// The main repo whose stale record names the repo at `i`.
    fn stale_main(&self, i: usize) -> Option<PathBuf> {
        let e = &self.ws.entries[i];
        let main = e.parent.as_ref()?;
        let s = self.ws.get(main)?.status()?;
        s.stale_worktrees.contains(&e.path).then(|| main.clone())
    }

    /// Gone (unreadable, its folder missing) with no record to prune: one
    /// to drop from the list.
    fn droppable(&self, i: usize) -> bool {
        let e = &self.ws.entries[i];
        e.error().is_some() && e.path.symlink_metadata().is_err() && self.stale_main(i).is_none()
    }

    /// Leave out items with nothing to act on: `c c` when nothing in the
    /// selection is staged.
    fn drop_inapplicable(&self, menu: &mut Menu) {
        let staged = self.selected().iter().any(|&i| {
            self.ws.entries[i]
                .status()
                .is_some_and(|s| s.changes.staged > 0)
        });
        if !staged {
            menu.items.retain(|it| {
                it.action != Action::Commit { all: false }
                    && !matches!(it.action, Action::Fixup { .. })
            });
        }
        // stashing: what there's something to stash of, or a stash for
        // (pop and drop: the stash under the cursor, else one of the one
        // repo selected, as `stash_pick` takes them)
        let on_stash = matches!(
            self.rows.get(self.cursor),
            Some(Row::Detail { line, .. }) if line.stash.is_some()
        );
        let stashes = on_stash
            || matches!(self.selected()[..], [i]
                if self.ws.entries[i].status().is_some_and(|s| !s.stashes.is_empty()));
        menu.items.retain(|it| match it.action {
            Action::Stash(StashKind::Staged) => staged,
            Action::Stash(StashKind::MarkedFiles) => {
                !self.marked_files.is_empty() || !self.lines().is_empty()
            }
            Action::PopStash | Action::DropStash => stashes,
            _ => true,
        });
        // with the cursor on a stash, or one stash in all, there's nothing
        // to ask before popping it (dropping always asks)
        let only = matches!(self.selected()[..], [i]
            if self.ws.entries[i].status().is_some_and(|s| s.stashes.len() == 1));
        if let Some(pop) = menu
            .items
            .iter_mut()
            .find(|it| it.action == Action::PopStash)
            && (on_stash || only)
        {
            pop.label = "pop".into();
        }
        // a repo without a forge identity has no owner to tag
        if self.owner().is_none() {
            menu.items
                .retain(|it| it.action != Action::OpenMenu(MenuId::Owner));
        }
    }

    /// The Tag and Owner menus' items for each tag with a key, first, in
    /// config order: "tag work" or "untag work" by the first selected
    /// repo (the Owner menu's are labelled by [`Self::state_labels`]). A
    /// key the menu has already is left to it (warned about at startup).
    fn add_tags(&self, menu: &mut Menu) {
        if !matches!(menu.id, MenuId::Tag | MenuId::Owner) {
            return;
        }
        let first = self.selected().first().map(|&i| &self.ws.entries[i].flags);
        let mut items = vec![];
        for (i, (name, def)) in self.ws.config.tags.iter().enumerate() {
            let Some(key) = def.key else { continue };
            if menu.items.iter().any(|it| it.key == key) {
                continue;
            }
            // pull and ignore have their items already: they take the key
            if let Some(m) = Some(name.as_str())
                .filter(|n| argus_core::config::acts(n))
                .and_then(|n| Tag::label(&self.ws.config, n))
            {
                let theirs = |a: &Action| match (m, a) {
                    (Tag::Pull, Action::TogglePull) | (Tag::Ignore, Action::ToggleIgnore) => true,
                    (_, Action::ToggleOwnerTag(o)) => *o == m,
                    _ => false,
                };
                if let Some(it) = menu.items.iter_mut().find(|it| theirs(&it.action)) {
                    it.key = key;
                }
                continue;
            }
            let (label, action) = match menu.id {
                MenuId::Owner => (String::new(), Action::ToggleOwnerTag(Tag::Label(i))),
                _ if first.is_some_and(|f| f.has(name)) => {
                    (format!("untag {name}"), Action::ToggleLabel(i))
                }
                _ => (format!("tag {name}"), Action::ToggleLabel(i)),
            };
            items.push(menu::item(key, label, action));
        }
        menu.items.splice(0..0, items);
    }

    /// Labels for toggles saying what choosing them does now: tag or
    /// untag (by the first selected repo, as toggling goes), show or hide.
    fn state_labels(&self, menu: &mut Menu) {
        let first = self.selected().first().map(|&i| &self.ws.entries[i].flags);
        let label = |action: &Action| -> Option<&'static str> {
            use Action::*;
            Some(match (menu.id, action) {
                (MenuId::Tag, TogglePull) if first?.pull => "stop pulling",
                (MenuId::Tag, TogglePull) => "pull (with F a and argus pull)",
                (MenuId::Tag, ToggleIgnore) if first?.ignored => "unignore repo",
                (MenuId::Tag, ToggleIgnore) => "ignore repo",
                (MenuId::View, ToggleAttention) if self.view.attention_only => "show all repos",
                (MenuId::View, ToggleAttention) => "show only repos needing attention",
                (MenuId::View, ToggleHidden) if self.view.show_hidden => {
                    "hide ignored and snoozed repos"
                }
                (MenuId::View, ToggleHidden) => "show ignored and snoozed repos",
                (MenuId::View, ToggleLog) if self.show_log => "hide process log",
                (MenuId::View, ToggleLog) => "show process log",
                _ => return None,
            })
        };
        let labels: Vec<Option<&str>> = menu.items.iter().map(|it| label(&it.action)).collect();
        for (it, l) in menu.items.iter_mut().zip(labels) {
            if let Some(l) = l {
                it.label = l.to_string();
            }
        }
        if menu.id == MenuId::Owner
            && let (Some(owner), Some(&i)) = (self.owner(), self.selected().first())
        {
            menu.title = format!("Owner {owner} (all its repos)");
            for it in &mut menu.items {
                let Action::ToggleOwnerTag(m) = it.action else {
                    continue;
                };
                let on = self.ws.owner_tagged(i, m) == Some(true);
                it.label = match (m, on) {
                    (Tag::Label(_), true) => format!("untag {}", m.label_text(&self.ws.config)),
                    (Tag::Label(_), false) => format!("tag {}", m.label_text(&self.ws.config)),
                    (Tag::Pull, true) => "stop pulling".into(),
                    (Tag::Pull, false) => "pull".into(),
                    (Tag::Ignore, true) => "unignore".into(),
                    (Tag::Ignore, false) => "ignore".into(),
                };
            }
        }
    }

    /// Carry out a confirmed (or harmless enough) destructive action.
    fn confirmed(&mut self, c: Confirmed) -> Vec<Effect> {
        match c {
            Confirmed::EditAgain => self.editing.iter().map(PatchEdit::open).collect(),
            Confirmed::Ops(ops) => {
                // a remote to point elsewhere waits for its repo's running
                // job (the pull that found it renamed) rather than be
                // dropped as busy
                let waits = |p: &PathBuf, op: &Op| {
                    matches!(op, Op::SetRemoteUrl { .. }) && self.running.contains_key(p)
                };
                let (later, now): (Vec<_>, Vec<_>) =
                    ops.into_iter().partition(|(p, op)| waits(p, op));
                let jobs: Vec<Job> = now
                    .into_iter()
                    .filter_map(|(p, op)| self.job(&p, op))
                    .collect();
                let fx = match jobs.is_empty() {
                    true => vec![],
                    false => self.start_jobs(jobs),
                };
                // (marked lines a discard took are acted on, as when staged)
                self.unmark_lines_started(&fx);
                if !later.is_empty() {
                    self.add_note(&format!(
                        "{} to follow once its repo's job is done",
                        later.len()
                    ));
                    self.queued.extend(later);
                }
                fx
            }
            Confirmed::ClearTags(keys) => {
                // by key: a scan may have arrived while asking
                let indices: Vec<usize> = (0..self.ws.entries.len())
                    .filter(|&i| keys.contains(&self.ws.entries[i].key))
                    .collect();
                let before = self.ws.config.clone();
                if let Err(e) = self.ws.clear_tags(&indices) {
                    self.error(e.to_string());
                    return vec![];
                }
                for k in &keys {
                    self.ws.state.snoozed.remove(k);
                }
                // what patterns (or tracking) still tag
                let left: Vec<String> = indices
                    .iter()
                    .filter_map(|&i| {
                        let tags = self.ws.remaining_tags(i);
                        (!tags.is_empty()).then(|| {
                            format!("{} {}", self.ws.entries[i].display_name(), tags.join(", "))
                        })
                    })
                    .collect();
                let what = match indices.len() {
                    1 => "tags cleared".to_string(),
                    n => format!("tags cleared for {n} repos"),
                };
                self.info(match left.is_empty() {
                    true => what,
                    false => format!("{what}; still: {}", left.join("; ")),
                });
                let mut fx = vec![Effect::SaveTags(Box::new(before))];
                fx.extend(self.flags_changed());
                fx
            }
        }
    }

    fn flags_changed(&mut self) -> Vec<Effect> {
        self.ws.resolve_flags();
        self.rebuild();
        vec![Effect::SaveState]
    }

    /// Every fix of `kind` the selected repos have and can run, and what
    /// was left out.
    fn planned(&self, kind: FixKind) -> Planned {
        let mut p = Planned::default();
        for i in self.selected() {
            let e = &self.ws.entries[i];
            // unreadable: nothing to say, unless it's a stale worktree
            if e.status().is_none() && self.stale_main(i).is_none() {
                continue;
            }
            let fixes: Vec<_> = self
                .fixes_for(i)
                .into_iter()
                .filter(|(_, f)| f.kind == kind)
                .collect();
            if fixes.is_empty() {
                p.none += 1;
            }
            for (at, f) in fixes {
                match f.blocked {
                    Some(why) => {
                        p.blocked += 1;
                        p.why.get_or_insert(format!("{}: {why}", e.display_name()));
                    }
                    None => p.ops.push((at, f.op)),
                }
            }
        }
        p
    }

    /// The issue on the details line under the cursor: its repo's key, the
    /// issue, and whether it's ignored.
    fn issue_under_cursor(&self) -> Option<(String, Issue, bool)> {
        let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor) else {
            return None;
        };
        Some((
            self.ws.entries[*repo].key.clone(),
            line.issue?,
            line.ignored,
        ))
    }

    fn not_applicable(&mut self, why: &str) -> Vec<Effect> {
        self.info(why.to_string());
        vec![]
    }

    fn name_of(&self, path: &std::path::Path) -> String {
        self.ws
            .get(path)
            .map_or_else(|| path.display().to_string(), |e| e.display_name())
    }

    /// The selected repos a fix of `kind` applies to.
    fn applicable(&self, kind: FixKind) -> Vec<PathBuf> {
        let stash = self.ws.config.stash;
        self.selected()
            .into_iter()
            .map(|i| &self.ws.entries[i])
            .filter(|e| {
                e.status()
                    .is_some_and(|s| fix::plan(s, &stash).iter().any(|f| f.kind == kind))
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Run every fix of a kind (one per branch, for kinds that have
    /// several) on the selected repos where it applies and isn't blocked.
    /// `b k`: delete the branch under the cursor in the details, asked
    /// first, saying what's known of its commits: merged, on a remote, or
    /// on none and not found merged (which they may still be). Forced
    /// (`-D`: you chose it), while it's at the commit seen.
    fn delete_branch(&mut self) -> Vec<Effect> {
        let Some(Row::Detail { repo, line }) = self.rows.get(self.cursor) else {
            return self.not_applicable("put the cursor on a branch in the details (Tab)");
        };
        let (Some(name), e) = (line.branch.clone(), &self.ws.entries[*repo]) else {
            return self.not_applicable("put the cursor on a branch in the details (Tab)");
        };
        let Some((s, b)) = e
            .status()
            .and_then(|s| Some((s, s.branches.iter().find(|b| b.name == name)?)))
        else {
            return vec![];
        };
        if b.is_head {
            return self.not_applicable("that's the branch you're on: switch to another first");
        }
        let default = s.default_branch.as_deref().unwrap_or("the default branch");
        let known = match (b.merged, b.absorbed, b.unpushed) {
            (true, ..) => format!("It's merged into {default}."),
            (_, true, _) => {
                format!("Its changes are in {default} (squashed or rebased); its commits go.")
            }
            (_, _, Some(0)) => {
                // its upstream, when that's what holds them
                let on = b
                    .upstream
                    .as_ref()
                    .filter(|t| !t.gone && t.ahead == 0)
                    .map(|t| format!(" ({})", t.name))
                    .unwrap_or_default();
                format!("Its commits are on a remote{on}: nothing is lost.")
            }
            (_, _, Some(n)) => format!(
                "{n} {} on no remote, and argus couldn't find them merged into {default} \
                 (they may be, in a way it can't see). They'd be lost.",
                plural(n, "commit is", "commits are")
            ),
            (_, _, None) => format!(
                "{} has no remote: its commits that aren't in {default} are only here. \
                 They'd be lost.",
                e.display_name()
            ),
        };
        let op = Op::DeleteBranch {
            branch: name.clone(),
            force_at: Some(b.tip.clone()),
        };
        self.mode = Mode::Confirm {
            question: format!("Delete branch {name}? {known}"),
            then: Confirmed::Ops(vec![(e.path.clone(), op)]),
        };
        vec![]
    }

    /// `x W`: remove the finished worktrees selected, asked first.
    fn remove_worktrees(&mut self) -> Vec<Effect> {
        let p = self.planned(FixKind::RemoveWorktree);
        let ops = p.ops.clone();
        let question = match ops.as_slice() {
            [] => {
                return match p.why {
                    Some(why) => {
                        self.error(why);
                        vec![]
                    }
                    None => self.not_applicable("no finished worktree to remove"),
                };
            }
            [(path, _)] => format!(
                "Remove the worktree {} ({})? Its folder is deleted, ignored files in it too; \
                 the branch stays.",
                self.name_of(path),
                paths::contract_tilde(path)
            ),
            _ => format!(
                "Remove {} finished worktrees? Their folders are deleted, ignored files in \
                 them too; the branches stay.",
                ops.len()
            ),
        } + &p.note();
        self.mode = Mode::Confirm {
            question,
            then: Confirmed::Ops(ops),
        };
        vec![]
    }

    fn fix_ops(&mut self, kind: FixKind) -> Vec<Effect> {
        let p = self.planned(kind);
        if p.ops.is_empty() {
            match p.why {
                Some(why) => self.error(why),
                None => self.info("not applicable to the selection (see details with Tab)"),
            }
            return vec![];
        }
        let note = p.note();
        let jobs = p
            .ops
            .into_iter()
            .filter_map(|(path, op)| self.job(&path, op))
            .collect();
        let fx = self.start_jobs(jobs);
        self.add_note(&note);
        fx
    }

    fn jobs_for(&mut self, scope: Scope, op: impl Fn(&Entry) -> Option<Op>) -> Vec<Effect> {
        let idx = if scope == Scope::All {
            self.bulk_repos()
        } else {
            self.selected()
        };
        let mut jobs = vec![];
        for i in idx {
            let e = &self.ws.entries[i];
            if let Some(op) = op(e) {
                jobs.push(Job {
                    label: format!("{} {}", op.label(), e.display_name()),
                    family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                    path: e.path.clone(),
                    op,
                });
            }
        }
        self.start_jobs(jobs)
    }

    /// `file`'s diff on `side` when it's text, read if it hasn't been.
    fn text_diff(&mut self, repo: &Path, file: &str, side: Side) -> Option<&Vec<diff::Hunk>> {
        let key = (repo.to_path_buf(), file.to_string(), side);
        let read = || diff::file_diff(repo, file, side).map_err(|e| e.to_string());
        match self.diffs.entry(key).or_insert_with(read) {
            Ok(FileDiff::Text(hunks)) => Some(hunks),
            _ => None,
        }
    }

    /// The lines of `file`'s diff that are added or removed, as (hunk,
    /// line in it); `None` when the diff hasn't been read as text.
    fn changed_lines(&self, repo: &Path, file: &str, side: Side) -> Option<Vec<(usize, usize)>> {
        let key = (repo.to_path_buf(), file.to_string(), side);
        let Some(Ok(FileDiff::Text(hunks))) = self.diffs.get(&key) else {
            return None;
        };
        let changed = |l: &&argus_core::diff::Line| l.kind != argus_core::diff::LineKind::Context;
        let mut lines = vec![];
        for (h, hunk) in hunks.iter().enumerate() {
            let at = hunk.lines.iter().enumerate();
            lines.extend(at.filter(|(_, l)| changed(l)).map(|(at, _)| (h, at)));
        }
        Some(lines)
    }

    /// Whether `file` is marked whole where `section` lists it: itself, or
    /// (untracked) the folder it's in.
    pub fn file_marked(&self, repo: &Path, section: Section, file: &str) -> bool {
        let files = self.marked_files.get(repo);
        files.is_some_and(|m| m.iter().any(|(s, f)| *s == section && f == file))
            || self.in_marked_folder(repo, section, file)
    }

    /// Whether `file` is in an untracked folder that's marked (`build/`,
    /// listed as one path and unfolding into what's in it).
    fn in_marked_folder(&self, repo: &Path, section: Section, file: &str) -> bool {
        let files = self.marked_files.get(repo);
        files.is_some_and(|m| m.iter().any(|(s, f)| *s == section && within(f, file)))
    }

    /// The diffs read for the repos `of` are forgotten, to be read again
    /// as they're shown. Marked lines and a region point into a diff as it
    /// was read, by hunk and line: those of a file whose diff reads the
    /// same again are kept with it (a refresh that found nothing new);
    /// the others go, and what went is returned to be said.
    fn reread_diffs(&mut self, of: impl Fn(&PathBuf) -> bool) -> Option<String> {
        type Key = (PathBuf, String, Side);
        let mut pointed: BTreeSet<Key> = BTreeSet::new();
        let lines = self.marked_lines.iter().filter(|k| of(&k.0));
        pointed.extend(lines.map(|k| (k.0.clone(), k.1.clone(), k.2)));
        let region = self.region.as_ref().filter(|r| of(&r.repo));
        pointed.extend(region.map(|r| (r.repo.clone(), r.file.clone(), r.side)));
        let same = |key: &Key| {
            let now = diff::file_diff(&key.0, &key.1, key.2).map_err(|e| e.to_string());
            self.diffs.get(key) == Some(&now)
        };
        let kept: BTreeSet<Key> = pointed.into_iter().filter(same).collect();
        let key = |repo: &PathBuf, file: &String, side: Side| (repo.clone(), file.clone(), side);
        self.diffs.retain(|k, _| !of(&k.0) || kept.contains(k));
        let gone = |k: &MarkedLine| of(&k.0) && !kept.contains(&key(&k.0, &k.1, k.2));
        let lost: Vec<&MarkedLine> = self.marked_lines.iter().filter(|k| gone(k)).collect();
        let mut files: Vec<&str> = lost.iter().map(|k| k.1.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let region = self.region.as_ref();
        let region =
            region.is_some_and(|r| of(&r.repo) && !kept.contains(&key(&r.repo, &r.file, r.side)));
        let said = match (lost.len(), region) {
            (0, false) => None,
            (0, true) => Some("the region went with its file's changes".to_string()),
            (n, region) => Some(format!(
                "{n} marked {} of {} went with {} changes{}",
                plural(n, "line", "lines"),
                files.join(", "),
                if files.len() == 1 { "its" } else { "their" },
                if region { " (and the region)" } else { "" },
            )),
        };
        self.marked_lines.retain(|k| !gone(k));
        if region {
            self.region = None;
        }
        said
    }

    /// The lines that are marked: those marked one by one, and those of a
    /// region while there is one (but for a file marked whole, which has
    /// them already). The one place that says so, for what's drawn as
    /// marked and for every key that takes what's marked.
    pub fn lines(&self) -> std::borrow::Cow<'_, BTreeSet<MarkedLine>> {
        if self.region.is_none() {
            return std::borrow::Cow::Borrowed(&self.marked_lines);
        }
        let section = |side: Side| match side {
            Side::Unstaged => Section::Unstaged,
            Side::Staged => Section::Staged,
        };
        let region = self.region_lines().into_iter();
        let region = region.filter(|k| !self.file_marked(&k.0, section(k.2), &k.1));
        let mut lines = self.marked_lines.clone();
        lines.extend(region);
        std::borrow::Cow::Owned(lines)
    }

    /// A region ends: its lines are kept as marks.
    fn keep_region(&mut self) {
        self.marked_lines = self.lines().into_owned();
        self.region = None;
    }

    /// How much of `file` as `section` lists it is marked, or with `hunk`
    /// how much of that hunk of its diff: by the file's own mark, else by
    /// its lines'.
    pub fn marked_of(
        &self,
        repo: &Path,
        section: Section,
        file: &str,
        hunk: Option<usize>,
    ) -> Marked {
        if self.file_marked(repo, section, file) {
            return Marked::Wholly;
        }
        // (an untracked folder with something marked in it)
        let files = self.marked_files.get(repo).into_iter().flatten();
        if files
            .into_iter()
            .any(|(s, f)| *s == section && within(file, f))
        {
            return Marked::Partly;
        }
        let side = section.side();
        let within = |h: usize| hunk.is_none_or(|of| of == h);
        let lines = self.lines();
        let marked = lines.iter();
        let marked = marked
            .filter(|k| k.0 == repo && k.1 == file && k.2 == side && within(k.3))
            .count();
        if marked == 0 {
            return Marked::No;
        }
        // (its diff not at hand to count: some of it, for all that's known)
        match self.changed_lines(repo, file, side) {
            Some(all) if all.iter().filter(|l| within(l.0)).count() <= marked => Marked::Wholly,
            _ => Marked::Partly,
        }
    }

    /// How much of what `section` lists of the repo is marked: all its
    /// files, or some file or line of one.
    pub fn section_marked(&self, repo: &Path, section: Section) -> Marked {
        let files = self.ws.get(repo).and_then(Entry::status);
        let files = files.map(|s| section.files(s)).unwrap_or_default();
        let of = |f: &String| self.marked_of(repo, section, f, None);
        let marked: Vec<Marked> = files.iter().map(of).collect();
        if !marked.is_empty() && marked.iter().all(|m| *m == Marked::Wholly) {
            Marked::Wholly
        } else if marked.iter().any(|m| *m != Marked::No) {
            Marked::Partly
        } else {
            Marked::No
        }
    }

    /// The files marked whole and the lines marked in the details of the
    /// repo at `path` and of the repos nested in it (all repos' with
    /// `None`).
    pub fn marked_in(&self, path: Option<&Path>) -> (usize, usize) {
        let within = |repo: &PathBuf| path.is_none_or(|p| repo.starts_with(p));
        let files = self.marked_files.iter().filter(|(repo, _)| within(repo));
        let lines = self.lines();
        let lines = lines.iter().filter(|k| within(&k.0));
        (files.map(|(_, f)| f.len()).sum(), lines.count())
    }

    /// How much of the changes of the repo at `path` (and of the repos
    /// nested in it) is marked in details: every file of every section,
    /// or something.
    pub fn repo_marked(&self, path: &Path) -> Marked {
        if self.marked_in(Some(path)) == (0, 0) {
            return Marked::No;
        }
        let status = self.ws.get(path).and_then(Entry::status);
        let lists = |s: &Section| status.is_some_and(|st| !s.files(st).is_empty());
        // (a section with no files says nothing either way)
        let whole = Section::ALL
            .iter()
            .filter(|s| lists(s))
            .all(|s| self.section_marked(path, *s) == Marked::Wholly);
        let marks = self.repos_with_marks();
        let nested = marks
            .iter()
            .any(|repo| *repo != path && repo.starts_with(path));
        match whole && !nested {
            true => Marked::Wholly,
            false => Marked::Partly,
        }
    }

    /// The repos with something marked in their details.
    pub fn repos_with_marks(&self) -> BTreeSet<PathBuf> {
        let lines = self.lines();
        let lines = lines.iter().map(|k| &k.0);
        self.marked_files.keys().chain(lines).cloned().collect()
    }

    /// Where `repos` are, when that's not just the repo the cursor is in:
    /// the one's name, or how many. What's marked in details is taken from
    /// every repo, so what's taken elsewhere is said.
    fn elsewhere(&self, repos: &[&Path]) -> Option<String> {
        let here = self.current_path();
        let name = |p: &Path| {
            self.ws
                .get(p)
                .map_or_else(String::new, |e| e.display_name())
        };
        match repos {
            [] => None,
            [one] if here.as_deref() == Some(*one) => None,
            [one] => Some(name(one)),
            many => Some(format!("{} repos", many.len())),
        }
    }

    /// For a question about marked files or lines: which repo they're in,
    /// when it isn't the one the cursor is in (`In acme/app: `), and the
    /// question's first letter to go with it.
    fn asked_where(&self, repos: &[&Path], question: String) -> String {
        let Some(whereabouts) = self.elsewhere(repos) else {
            return question;
        };
        let mut letters = question.chars();
        let first = letters.next().map(|c| c.to_lowercase().to_string());
        format!(
            "In {whereabouts}: {}{}",
            first.unwrap_or_default(),
            letters.as_str()
        )
    }

    /// What row `n` is of a repo's changes, to mark or to act on: the one
    /// place that says so, for drawing its mark, for space and for the
    /// keys that take what's under the cursor. `None` for a row that's
    /// nothing by itself: one outside the changes, an unchanged line, the
    /// blank line after a diff.
    pub fn target(&self, n: usize) -> Option<Target> {
        let Some(Row::Detail { repo, line }) = self.rows.get(n) else {
            return None;
        };
        let repo = self.ws.entries[*repo].path.clone();
        let section = line.section?;
        let Some(file) = line.file.clone() else {
            return Section::heads(line).map(|section| Target::Section { repo, section });
        };
        if line.id.last() == Some(&Seg::Blank) {
            return None;
        }
        let d = line.diff.as_deref();
        let unchanged = |l: &diff::Line| l.kind == diff::LineKind::Context;
        Some(match d.map(|d| (d.hunk, d.at, &d.line)) {
            Some((_, _, Some(l))) if unchanged(l) => return None,
            // (an untracked file's lines aren't staged by themselves; a
            // row of a diff that isn't text stands for its file)
            Some((Some(hunk), at, _)) if section != Section::Untracked => Target::Hunk {
                repo,
                section,
                file,
                hunk,
                at,
            },
            _ => Target::File {
                repo,
                section,
                file,
            },
        })
    }

    /// How much of `target` is marked: what its row shows.
    pub fn marked(&self, target: &Target) -> Marked {
        match target {
            Target::Repo { repo } => self.repo_marked(repo),
            Target::Section { repo, section } => self.section_marked(repo, *section),
            Target::File {
                repo,
                section,
                file,
            } => self.marked_of(repo, *section, file, None),
            Target::Hunk {
                repo,
                section,
                file,
                hunk,
                at: None,
            } => self.marked_of(repo, *section, file, Some(*hunk)),
            Target::Hunk {
                repo,
                section,
                file,
                hunk,
                at: Some(at),
            } => {
                let key = (repo.clone(), file.clone(), section.side(), *hunk, *at);
                match self.lines().contains(&key) || self.file_marked(repo, *section, file) {
                    true => Marked::Wholly,
                    false => Marked::No,
                }
            }
        }
    }

    /// Space on `target`: marked, wholly or partly, it's unmarked; not
    /// marked, it's marked, all of it.
    fn toggle(&mut self, target: Target) {
        match target {
            // (not a row's target: see [`Self::target`])
            Target::Repo { .. } => {}
            Target::Section { repo, section } => {
                let was = self.section_marked(&repo, section);
                let files = self.ws.get(&repo).and_then(Entry::status);
                let files = files.map(|s| section.files(s)).unwrap_or_default();
                if let Some(marked) = self.marked_files.get_mut(&repo) {
                    marked.retain(|(s, _)| *s != section);
                }
                let side = section.side();
                if section != Section::Untracked {
                    self.marked_lines.retain(|k| !(k.0 == repo && k.2 == side));
                }
                if was == Marked::No {
                    let marked = self.marked_files.entry(repo.clone()).or_default();
                    marked.extend(files.into_iter().map(|f| (section, f)));
                }
                if self.marked_files.get(&repo).is_some_and(BTreeSet::is_empty) {
                    self.marked_files.remove(&repo);
                }
            }
            Target::File {
                repo,
                section,
                file,
            } => self.toggle_in_file(repo, section, file, None),
            Target::Hunk {
                repo,
                section,
                file,
                hunk,
                at,
            } => self.toggle_in_file(repo, section, file, Some((hunk, at))),
        }
    }

    fn toggle_in_file(
        &mut self,
        repo: PathBuf,
        section: Section,
        file: String,
        part: Option<(usize, Option<usize>)>,
    ) {
        if self.in_marked_folder(&repo, section, &file) {
            self.info("its folder is marked: unmark that to mark files in it");
        } else {
            self.toggle_marked(repo, section, file, part);
        }
    }

    /// Space on a file of the changes (`part` none), on a hunk's own line
    /// (its number) or on a line of one (and the line's): what's wholly or
    /// partly marked is unmarked, what isn't marked is marked. A hunk or
    /// line taken out of a file marked whole leaves the file's other lines
    /// marked, each by itself.
    fn toggle_marked(
        &mut self,
        repo: PathBuf,
        section: Section,
        file: String,
        part: Option<(usize, Option<usize>)>,
    ) {
        let side = section.side();
        let was = self.marked_of(&repo, section, &file, part.map(|(h, _)| h));
        let all = self.changed_lines(&repo, &file, side).unwrap_or_default();
        let whole = self.file_marked(&repo, section, &file);
        if let Some(files) = self.marked_files.get_mut(&repo) {
            files.remove(&(section, file.clone()));
            // (a folder's own line: what's marked in it, too)
            if part.is_none() {
                files.retain(|(s, f)| !(*s == section && within(&file, f)));
            }
            if files.is_empty() {
                self.marked_files.remove(&repo);
            }
        }
        let key = |(h, at): (usize, usize)| (repo.clone(), file.clone(), side, h, at);
        let Some((h, at)) = part else {
            self.marked_lines
                .retain(|k| !(k.0 == repo && k.1 == file && k.2 == side));
            if was == Marked::No {
                let files = self.marked_files.entry(repo).or_default();
                files.insert((section, file));
            }
            return;
        };
        if whole {
            self.marked_lines.extend(all.iter().copied().map(key));
        }
        let (of, on) = match at {
            Some(at) => (vec![(h, at)], !self.marked_lines.contains(&key((h, at)))),
            None => (
                all.into_iter().filter(|l| l.0 == h).collect(),
                was == Marked::No,
            ),
        };
        for line in of {
            if on {
                self.marked_lines.insert(key(line));
            } else {
                self.marked_lines.remove(&key(line));
            }
        }
    }

    /// What `s`, `u` and `K` take, and whether that's by its marks: what's
    /// marked in details, in every repo; with nothing marked, what the
    /// cursor is on (a hunk, whichever of its lines that's on). The one
    /// place that says so: each thing with where it's listed, so nothing
    /// is taken by its other side.
    pub fn taken(&self) -> (Vec<Target>, bool) {
        self.taken_in(None)
    }

    /// [`Self::taken`], or with `only` what's taken in that repo alone:
    /// what's marked in it, and with nothing marked in it what the cursor
    /// is on.
    fn taken_in(&self, only: Option<&Path>) -> (Vec<Target>, bool) {
        let lines = self.lines();
        let within = |repo: &PathBuf| only.is_none_or(|o| repo == o);
        let marked = self.marked_files.keys().any(within) || lines.iter().any(|k| within(&k.0));
        if !marked {
            return (self.under_cursor().into_iter().collect(), false);
        }
        let mut taken = vec![];
        for (repo, files) in self.marked_files.iter().filter(|(r, _)| within(r)) {
            for (section, file) in files {
                taken.push(Target::File {
                    repo: repo.clone(),
                    section: *section,
                    file: file.clone(),
                });
            }
        }
        for (repo, file, side, hunk, at) in lines.iter().filter(|k| within(&k.0)) {
            taken.push(Target::Hunk {
                repo: repo.clone(),
                section: match side {
                    Side::Unstaged => Section::Unstaged,
                    Side::Staged => Section::Staged,
                },
                file: file.clone(),
                hunk: *hunk,
                at: Some(*at),
            });
        }
        (taken, true)
    }

    /// What the cursor is on, to act on with nothing marked: its row's
    /// target, a line standing for its hunk (an unchanged one too, which
    /// is nothing to mark but is in its hunk); on the repo's own row or a
    /// row of its details that's none of its changes, the repo. Nothing on
    /// the blank line after a diff.
    fn under_cursor(&self) -> Option<Target> {
        let hunk = |repo, section, file, hunk| Target::Hunk {
            repo,
            section,
            file,
            hunk,
            at: None,
        };
        match self.target(self.cursor) {
            Some(Target::Hunk {
                repo,
                section,
                file,
                hunk: h,
                ..
            }) => return Some(hunk(repo, section, file, h)),
            Some(other) => return Some(other),
            None => {}
        }
        let (repo, line) = match self.rows.get(self.cursor)? {
            Row::Repo(i) => (*i, None),
            Row::Detail { repo, line } => (*repo, Some(line)),
        };
        let repo = self.ws.entries[repo].path.clone();
        match line.and_then(|l| Some((l, l.section?))) {
            None => Some(Target::Repo { repo }),
            Some((line, section)) => {
                let section = Some(section).filter(|s| *s != Section::Untracked)?;
                let h = line.diff.as_deref()?.hunk?;
                Some(hunk(repo, section, line.file.clone()?, h))
            }
        }
    }

    /// The files of what's `taken`, by repo index: a section's are those
    /// it lists.
    fn files_of(&self, taken: Vec<Target>, verb: HunkVerb) -> Vec<(usize, Vec<String>)> {
        let mut by: Vec<(usize, Vec<String>)> = vec![];
        let listed = |repo: &Path, sections: &[Section]| -> Vec<String> {
            let s = self.ws.get(repo).and_then(Entry::status);
            let files = sections
                .iter()
                .flat_map(|section| s.map(|s| section.files(s)));
            files.flatten().collect()
        };
        for target in taken {
            let (repo, files) = match target {
                // (a repo: what it lists on the side the key is for)
                Target::Repo { repo } => {
                    let sections: &[Section] = match verb {
                        HunkVerb::Unstage => &[Section::Staged],
                        _ => &[Section::Unstaged, Section::Untracked],
                    };
                    let files = listed(&repo, sections);
                    (repo, files)
                }
                Target::Section { repo, section } => {
                    let files = listed(&repo, &[section]);
                    (repo, files)
                }
                Target::File { repo, file, .. } => (repo, vec![file]),
                Target::Hunk { .. } => continue,
            };
            let Some(i) = self.ws.index_of(&repo) else {
                continue;
            };
            let at = by.iter().position(|(n, _)| *n == i);
            let at = at.unwrap_or_else(|| {
                by.push((i, vec![]));
                by.len() - 1
            });
            for f in files {
                if !by[at].1.contains(&f) {
                    by[at].1.push(f);
                }
            }
        }
        by
    }

    /// Why `verb` isn't for one of `targets`, if it isn't: staging what's
    /// staged, unstaging what isn't, discarding what's staged. Said of
    /// what's under the cursor, or (`marked`) of the marks to take off.
    fn not_for(verb: HunkVerb, targets: &[Target], marked: bool) -> Option<&'static str> {
        let wrong = |t: &&Target| {
            let (Target::Section { section, .. }
            | Target::File { section, .. }
            | Target::Hunk { section, .. }) = t
            else {
                // (a repo: what it has on the key's side, whichever that is)
                return false;
            };
            (*section == Section::Staged) != (verb == HunkVerb::Unstage)
        };
        let wrong: Vec<&Target> = targets.iter().filter(wrong).collect();
        let lines = wrong.iter().any(|t| matches!(t, Target::Hunk { .. }));
        if wrong.is_empty() {
            return None;
        }
        Some(match (verb, marked, lines) {
            (HunkVerb::Stage, false, _) => "that's staged already: u unstages it",
            (HunkVerb::Unstage, false, _) => "that's not staged: s stages it",
            (HunkVerb::Discard, false, _) => "that's staged: u unstages it, then K discards it",
            (HunkVerb::Unstage, true, true) => {
                "some marked lines aren't staged: unmark them (space) first"
            }
            (_, true, true) => "some marked lines are staged: unmark them (space) first",
            (HunkVerb::Stage, true, false) => {
                "some marked files are staged already: unmark them (space) first"
            }
            (HunkVerb::Unstage, true, false) => {
                "some marked files aren't staged: unmark them (space) first"
            }
            (HunkVerb::Discard, true, false) => {
                "some marked files are staged (u unstages them, then K discards them): \
                 unmark them (space) first"
            }
        })
    }

    /// `s`, `u` or `K`: on what's [taken](Self::taken), each thing only
    /// where it is. Lines and hunks go as patches (one per file and side,
    /// built from the diff as it was read), files whole. Discarding asks
    /// first, once, saying what goes.
    /// Whether something is marked (in the repos `marks`, those with marks
    /// the key is for) in a repo the cursor isn't in, and so which marks
    /// are meant has to be asked: opens the menu for it if so. `this` is the key's own letter, for the cursor's repo alone
    /// (left out when that has nothing for it), `A` for all repos.
    fn asks_which(
        &mut self,
        word: &str,
        marks: BTreeSet<PathBuf>,
        this: Option<(char, Action)>,
        all: Action,
    ) -> bool {
        let here = self.current_path();
        if !marks.iter().any(|r| here.as_ref() != Some(r)) {
            return false;
        }
        let name = |p: &PathBuf| {
            self.ws
                .get(p)
                .map_or_else(String::new, |e| e.display_name())
        };
        let names: Vec<String> = marks.iter().map(name).collect();
        let mut items = vec![];
        items.extend(this.map(|(key, action)| menu::item(key, "this repo", action)));
        items.push(menu::item('A', "all repos", all));
        let menu = Menu {
            id: MenuId::Across,
            title: format!("{word} · marked repos: {}", names.join(", ")),
            items,
        };
        self.mode = Mode::Menu { menu, cursor: 0 };
        true
    }

    /// `s`, `u` or `K` as pressed. What's marked in details is marked
    /// across repos: with some of it in a repo the cursor isn't in, it's
    /// asked which is meant.
    fn take_or_ask(&mut self, verb: HunkVerb) -> Vec<Effect> {
        let (key, word) = match verb {
            HunkVerb::Stage => ('s', "Stage"),
            HunkVerb::Unstage => ('u', "Unstage"),
            HunkVerb::Discard => ('K', "Discard"),
        };
        let this = Some((key, Action::TakeHere(verb)));
        let marks = self.repos_with_marks();
        match self.asks_which(word, marks, this, Action::TakeAll(verb)) {
            true => vec![],
            false => self.take(verb, None),
        }
    }

    fn take(&mut self, verb: HunkVerb, only: Option<PathBuf>) -> Vec<Effect> {
        let (targets, marked) = self.taken_in(only.as_deref());
        if targets.is_empty() {
            self.info(FILE_HINT);
            return vec![];
        }
        if let Some(why) = Self::not_for(verb, &targets, marked) {
            self.info(why);
            return vec![];
        }
        // (taken: a region is over, its lines marks to unmark when done)
        self.keep_region();
        let (index, reverse, word) = match verb {
            HunkVerb::Stage => (true, false, "stage"),
            HunkVerb::Unstage => (true, true, "unstage"),
            HunkVerb::Discard => (false, true, "discard"),
        };
        // by repo, file and side: hunk -> its lines taken (all if None)
        type Chosen = BTreeMap<(PathBuf, String, Side), BTreeMap<usize, Option<Vec<usize>>>>;
        let mut chosen: Chosen = BTreeMap::new();
        for target in &targets {
            if let Target::Hunk {
                repo,
                section,
                file,
                hunk,
                at,
            } = target
            {
                let key = (repo.clone(), file.clone(), section.side());
                let hunks = chosen.entry(key).or_default();
                match at {
                    None => drop(hunks.insert(*hunk, None)),
                    Some(at) => {
                        let lines = hunks.entry(*hunk).or_insert_with(|| Some(vec![]));
                        lines.get_or_insert_default().push(*at);
                    }
                }
            }
        }
        let mut ops: Vec<(PathBuf, Op)> = vec![];
        let (mut lines, mut hunk) = (0, None);
        for ((repo, file, side), hunks) in &chosen {
            let key = (repo.clone(), file.clone(), *side);
            let Some(Ok(FileDiff::Text(all))) = self.diffs.get(&key) else {
                continue;
            };
            let parts: Vec<(&diff::Hunk, Option<&[usize]>)> = hunks
                .iter()
                .filter_map(|(h, at)| Some((all.get(*h)?, at.as_deref())))
                .collect();
            let n: usize = hunks.values().flatten().map(Vec::len).sum();
            let whole = hunks.values().any(Option::is_none);
            let what = match whole {
                true => format!("{word} a hunk of {file}"),
                false => format!("{word} {n} {} of {file}", plural(n, "line", "lines")),
            };
            lines += n;
            hunk = whole.then(|| what.clone());
            let patch = diff::patch_hunks(file, &parts, reverse);
            ops.push((
                repo.clone(),
                Op::Apply {
                    patch,
                    index,
                    reverse,
                    what,
                },
            ));
        }
        // then the files, whole
        // (a whole repo, taken from its own row with its details folded
        // away, is asked about first: nothing shows what "everything" is)
        let whole = match targets.as_slice() {
            [Target::Repo { repo }] => Some(repo.clone()),
            _ => None,
        };
        let folded = matches!(self.rows.get(self.cursor), Some(Row::Repo(_)))
            && !matches!(self.rows.get(self.cursor + 1), Some(Row::Detail { .. }));
        let files = self.files_of(targets, verb);
        let plan = match verb {
            HunkVerb::Stage => ops::stage_ops,
            HunkVerb::Unstage => ops::unstage_ops,
            HunkVerb::Discard => ops::discard_ops,
        };
        let patched = ops.len();
        let mut nested: Vec<String> = vec![];
        let mut file_jobs = vec![];
        for (i, names) in &files {
            let e = &self.ws.entries[*i];
            let Some(s) = e.status() else { continue };
            // (what staging leaves out, to say why when that's everything)
            let inside: Vec<String> = ops::nested_repos_in(s, names)
                .into_iter()
                .cloned()
                .collect();
            // (nor does discarding delete one: git wouldn't)
            let names: Vec<String> = names
                .iter()
                .filter(|n| verb != HunkVerb::Discard || !inside.contains(n))
                .cloned()
                .collect();
            nested.extend(inside);
            file_jobs.extend(plan(s, &names).into_iter().map(|op| (*i, op)));
        }
        if verb != HunkVerb::Discard {
            let mut jobs: Vec<Job> = ops
                .into_iter()
                .filter_map(|(p, op)| self.job(&p, op))
                .collect();
            jobs.extend(file_jobs.into_iter().map(|(i, op)| self.file_job(i, op)));
            if jobs.is_empty() {
                match nested.first() {
                    // (only staging leaves them out)
                    Some(one) => self.info(format!(
                        "{one} is a repo of its own: staging would add it as an embedded repo \
                         (gitignore it, or make it a submodule)"
                    )),
                    None => self.info(format!("nothing to {word}")),
                }
                return vec![];
            }
            if let Some(repo) = whole.filter(|_| folded) {
                let name = self
                    .ws
                    .get(&repo)
                    .map_or_else(String::new, |e| e.display_name());
                let n: usize = files.iter().map(|(_, f)| f.len()).sum();
                let question = format!(
                    "{} everything in {name} ({n} {})?",
                    capitalised(word),
                    plural(n, "file", "files")
                );
                let ops = jobs.into_iter().map(|j| (j.path, j.op)).collect();
                self.mode = Mode::Confirm {
                    question,
                    then: Confirmed::Ops(ops),
                };
                return vec![];
            }
            let mut repos: Vec<PathBuf> = jobs.iter().map(|j| j.path.clone()).collect();
            repos.sort();
            repos.dedup();
            let fx = self.start_jobs(jobs);
            self.unmark_lines_started(&fx);
            self.note_elsewhere(&repos);
            return fx;
        }
        // what it would do, to say exactly that: unstaged changes dropped,
        // untracked files deleted, staged work left
        let (mut changed, mut untracked): (Vec<&String>, Vec<&String>) = (vec![], vec![]);
        for (_, op) in &file_jobs {
            match op {
                Op::Clean { paths } => untracked.extend(paths),
                Op::Restore { paths, .. } => changed.extend(paths),
                _ => {}
            }
        }
        // (files taken by their marks are said to be: the cursor may be on
        // something else, a section of more files say)
        let of_marks = if marked && patched == 0 {
            "marked "
        } else {
            ""
        };
        let files_question = match (changed.as_slice(), untracked.as_slice()) {
            ([], []) => None,
            ([one], []) => Some(format!("unstaged changes to {one}? They're lost.")),
            ([], [one]) => Some(format!("untracked {one}? It's lost.")),
            (c, []) => Some(format!(
                "unstaged changes to {} {of_marks}files? They're lost.",
                c.len()
            )),
            ([], u) => Some(format!(
                "{} {of_marks}untracked files? They're lost.",
                u.len()
            )),
            (c, u) => Some(format!(
                "unstaged changes to {} {of_marks}{} and delete {} {of_marks}untracked? \
                 They're lost.",
                c.len(),
                plural(c.len(), "file", "files"),
                u.len()
            )),
        };
        let delete_only = changed.is_empty() && !untracked.is_empty();
        let question = match (patched, &hunk, files_question) {
            (0, _, None) => {
                match nested.first() {
                    Some(one) => self.info(format!(
                        "{one} is a repo of its own: K doesn't delete it from here"
                    )),
                    None => self.info("nothing unstaged to discard (u unstages what's staged)"),
                }
                return vec![];
            }
            (0, _, Some(files)) if delete_only => format!("Delete {files}"),
            (0, _, Some(files)) => format!("Discard {files}"),
            (_, Some(hunk), None) => format!(
                "{}? It's lost.",
                capitalised(&hunk.replacen("a ", "this ", 1))
            ),
            (n, _, None) => format!(
                "Discard the marked lines ({n} file{})? They're lost.",
                if n == 1 { "" } else { "s" }
            ),
            (_, _, Some(files)) => {
                let files = files.replace("It's lost.", "They're lost.");
                let of = format!("{lines} marked {}", plural(lines, "line", "lines"));
                match (delete_only, untracked.is_empty()) {
                    (true, _) => format!("Discard {of} and delete {files}"),
                    (false, true) => format!("Discard {of} and {files}"),
                    (false, false) => format!("Discard {of}, {files}"),
                }
            }
        };
        let path = |i: usize| self.ws.entries[i].path.clone();
        let mut repos: Vec<PathBuf> = ops.iter().map(|(p, _)| p.clone()).collect();
        repos.extend(files.iter().map(|(i, _)| path(*i)));
        repos.sort();
        repos.dedup();
        let repos: Vec<&Path> = repos.iter().map(PathBuf::as_path).collect();
        let question = match &whole {
            Some(repo) => {
                let name = self
                    .ws
                    .get(repo)
                    .map_or_else(String::new, |e| e.display_name());
                let mut letters = question.chars();
                let first = letters.next().map(|c| c.to_lowercase().to_string());
                format!(
                    "Everything in {name}: {}{}",
                    first.unwrap_or_default(),
                    letters.as_str()
                )
            }
            None => self.asked_where(&repos, question),
        };
        ops.extend(file_jobs.into_iter().map(|(i, op)| (path(i), op)));
        let then = Confirmed::Ops(ops);
        self.mode = Mode::Confirm { question, then };
        vec![]
    }

    /// Add to the status line where jobs were started, when that's not
    /// just the repo the cursor is in.
    fn note_elsewhere(&mut self, repos: &[PathBuf]) {
        let repos: Vec<&Path> = repos.iter().map(PathBuf::as_path).collect();
        if let Some(whereabouts) = self.elsewhere(&repos) {
            self.add_note(&format!(" · in {whereabouts}"));
        }
    }

    /// A job running a file operation in the repo at index `i`.
    fn file_job(&self, i: usize, op: Op) -> Job {
        let e = &self.ws.entries[i];
        Job {
            label: format!("{} in {}", op.label(), e.display_name()),
            family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
            path: e.path.clone(),
            op,
        }
    }

    /// The added and removed lines of a region: those of its file and side
    /// between where it starts and the cursor (unchanged lines ignored).
    pub fn region_lines(&self) -> Vec<MarkedLine> {
        let Some(r) = &self.region else {
            return vec![];
        };
        // (a diff converted to text has no lines to take by themselves)
        let diff = self.diffs.get(&(r.repo.clone(), r.file.clone(), r.side));
        let Some(Ok(FileDiff::Text(hunks))) = diff else {
            return vec![];
        };
        // where the cursor is in the diff: in the copy the region is of,
        // its own place; else that copy's side of it
        let top = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Repo(i) if self.ws.entries[*i].path == r.root));
        let copy: Vec<usize> = top
            .and_then(|top| self.rows_of(top))
            .map(|(_, rows)| rows.filter(|n| self.id(*n).starts_with(&r.home)).collect())
            .unwrap_or_default();
        use std::cmp::Ordering::{Greater, Less};
        let sides = copy.first().zip(copy.last());
        let sides = sides.map(|(first, last)| (self.cursor.cmp(first), self.cursor.cmp(last)));
        let end = match (sides, self.rows.get(self.cursor)) {
            (Some((Less, _)), _) => DIFF_START,
            (Some((_, Greater)), _) => DIFF_END,
            (Some(_), Some(Row::Detail { line, .. })) => Self::diff_pos(line),
            // (the copy folded away: no further than where it starts)
            _ => r.at,
        };
        let (from, to) = (r.at.min(end), r.at.max(end));
        let mut lines = vec![];
        for (h, hunk) in hunks.iter().enumerate() {
            for (at, l) in hunk.lines.iter().enumerate() {
                let changed = l.kind != argus_core::diff::LineKind::Context;
                if changed && (from..=to).contains(&(h, 2 * at + 1)) {
                    lines.push((r.repo.clone(), r.file.clone(), r.side, h, at));
                }
            }
        }
        lines
    }

    /// Where a row of a file's diff (or its file's own line) is in the
    /// diff: see [`DiffPos`]. The file's line and a note under it are
    /// before all of it, the blank line that ends it after.
    fn diff_pos(line: &DetailLine) -> DiffPos {
        match line.diff.as_deref() {
            Some(DiffRef {
                hunk: Some(h), at, ..
            }) => (*h, at.map_or(0, |at| 2 * at + 1)),
            _ if line.id.last() == Some(&Seg::Blank) => DIFF_END,
            _ => DIFF_START,
        }
    }

    /// `e` in a diff: the marked lines (else the hunk or line under the
    /// cursor) as a patch, with how to edit it, in git's editor.
    fn edit_patch(&mut self) -> Vec<Effect> {
        if self.editing.as_ref().is_some_and(|e| e.applying) {
            self.info("the edited patch is still being applied");
            return vec![];
        }
        // by repo, file and side: hunk -> its chosen lines (all if None)
        type Chosen = BTreeMap<(PathBuf, String, Side), BTreeMap<usize, Option<Vec<usize>>>>;
        let mut chosen: Chosen = BTreeMap::new();
        // what's marked (a file marked whole: all its hunks; an untracked
        // one is said below to have no patch), else the hunk the cursor
        // is in
        let (taken, marked) = self.taken();
        for target in taken {
            match target {
                Target::Hunk {
                    repo,
                    section,
                    file,
                    hunk,
                    at,
                } => {
                    let hunks = chosen.entry((repo, file, section.side())).or_default();
                    match at {
                        None => drop(hunks.insert(hunk, None)),
                        Some(at) => {
                            let lines = hunks.entry(hunk).or_insert_with(|| Some(vec![]));
                            lines.get_or_insert_default().push(at);
                        }
                    }
                }
                Target::File {
                    section: Section::Untracked,
                    file,
                    ..
                } => {
                    self.info(format!(
                        "{file} is untracked: no patch to edit, s stages it whole"
                    ));
                    return vec![];
                }
                Target::File {
                    repo,
                    section,
                    file,
                } if marked => {
                    let hunks = match self.text_diff(&repo, &file, section.side()) {
                        Some(hunks) if !hunks.is_empty() => hunks.len(),
                        _ => {
                            self.info(format!("{file} has no lines to edit (it isn't text)"));
                            return vec![];
                        }
                    };
                    let key = (repo, file, section.side());
                    chosen
                        .entry(key)
                        .or_default()
                        .extend((0..hunks).map(|h| (h, None)));
                }
                Target::File { .. } | Target::Section { .. } | Target::Repo { .. } => {}
            }
        }
        let Some((repo, _, side)) = chosen.keys().next().cloned() else {
            self.info("e edits a hunk or lines of a file's diff (Tab on a file shows it)");
            return vec![];
        };
        if chosen.keys().any(|k| k.2 != side) {
            self.info("the marked lines are staged and unstaged: unmark one side's (space) first");
            return vec![];
        }
        if chosen.keys().any(|k| k.0 != repo) {
            self.info("the marked lines are in several repos: edit one repo's at a time");
            return vec![];
        }
        let reverse = side == Side::Staged;
        let mut patch = String::new();
        let mut files = vec![];
        for ((_, file, _), hunks) in &chosen {
            let key = (repo.clone(), file.clone(), side);
            let Some(Ok(FileDiff::Text(all))) = self.diffs.get(&key) else {
                continue;
            };
            let parts: Vec<(&diff::Hunk, Option<&[usize]>)> = hunks
                .iter()
                .filter_map(|(h, at)| Some((all.get(*h)?, at.as_deref())))
                .collect();
            patch.push_str(&diff::patch_hunks(file, &parts, reverse));
            files.push(file.clone());
        }
        // (taken: a region is over, and the marks with it)
        self.region = None;
        self.marked_lines.clear();
        // a new file, the user's alone: never one somebody else put there
        // (a shared temp directory), nor through a link. A name taken (one
        // left by an argus that had this process's number) is passed over.
        // (what's marked is taken wherever the cursor is: said, when it's
        // not in this repo)
        let elsewhere = self.elsewhere(&[repo.as_path()]);
        let elsewhere = elsewhere.map_or_else(String::new, |name| format!("# In {name}.\n"));
        let text = patch + edit_instructions(reverse) + &elsewhere;
        let create = |n: usize| {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let file = std::env::temp_dir().join(format!("argus-{}-{n}.diff", std::process::id()));
            let mut opts = std::fs::OpenOptions::new();
            let made = opts.write(true).create_new(true).mode(0o600).open(&file);
            let written = made.and_then(|mut f| {
                // (made here, so here's to take it away half-written)
                f.write_all(text.as_bytes())
                    .inspect_err(|_| drop(std::fs::remove_file(&file)))
            });
            (written, file)
        };
        let next = || EDITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let taken = |r: &std::io::Result<()>| matches!(r, Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists);
        let (written, file) = (0..1000)
            .map(|_| create(next()))
            .find(|(r, _)| !taken(r))
            .unwrap_or_else(|| create(next()));
        if let Err(e) = written {
            self.error(format!("{}: {e}", file.display()));
            return vec![];
        }
        let edit = PatchEdit {
            repo,
            file,
            reverse,
            files,
            applying: false,
        };
        let open = edit.open();
        self.editing = Some(edit);
        vec![open]
    }

    /// Back from editing the patch: apply what was saved (its `#` lines
    /// dropped), unless it's empty or touches more than the files' hunks.
    fn edited(&mut self, r: Result<(), String>) -> Vec<Effect> {
        let Some(edit) = self.editing.as_mut() else {
            return vec![];
        };
        let text = r.and_then(|()| {
            std::fs::read_to_string(&edit.file).map_err(|e| format!("{}: {e}", edit.file.display()))
        });
        let text = match text {
            Ok(t) => t,
            Err(e) => {
                self.drop_edit();
                self.error(format!("editing the patch: {e}"));
                return vec![];
            }
        };
        let mut patch: String = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .flat_map(|l| [l, "\n"])
            .collect();
        let verb = if edit.reverse { "unstage" } else { "stage" };
        if patch.trim().is_empty() {
            self.drop_edit();
            self.info(format!("the patch is empty: nothing to {verb}"));
            return vec![];
        }
        if let Err(why) = check_patch(&patch, &edit.files) {
            self.mode = Mode::Confirm {
                question: format!("The edited patch {why}: edit it again?"),
                then: Confirmed::EditAgain,
            };
            return vec![];
        }
        if !patch.ends_with('\n') {
            patch.push('\n');
        }
        edit.applying = true;
        let what = match edit.files.as_slice() {
            [one] => format!("{verb} an edited patch of {one}"),
            all => format!("{verb} an edited patch of {} files", all.len()),
        };
        let op = Op::Apply {
            patch,
            index: true,
            reverse: edit.reverse,
            what,
        };
        let repo = edit.repo.clone();
        let jobs = self.job(&repo, op).into_iter().collect();
        self.start_jobs(jobs)
    }

    fn drop_edit(&mut self) {
        if let Some(e) = self.editing.take() {
            let _ = std::fs::remove_file(e.file);
        }
    }

    /// The marked lines of the repos jobs were started in are acted on,
    /// so unmarked; those of a repo too busy to start in stay.
    fn unmark_lines_started(&mut self, fx: &[Effect]) {
        let started = |path: &PathBuf| {
            fx.iter()
                .any(|e| matches!(e, Effect::Jobs(jobs) if jobs.iter().any(|j| j.path == *path)))
        };
        self.marked_lines.retain(|k| !started(&k.0));
    }

    /// Whether row `i` is a section line (`n`/`p` stop there).
    fn is_section(&self, i: usize) -> bool {
        match &self.rows[i] {
            Row::Repo(_) => true,
            Row::Detail { line, .. } => !line.is_body(),
        }
    }

    /// What was folded or unfolded under lines the repo no longer has is
    /// forgotten: a file committed and changed again starts folded, as
    /// any other. `path` is the repo whose status is new: its listed
    /// repo's lines are looked over, a nested repo's among them.
    fn forget_gone(&mut self, path: &std::path::Path) {
        let mut root = self.ws.get(path);
        while let Some(e) = root.filter(|e| e.nested) {
            root = e.parent.as_ref().and_then(|p| self.ws.get(p));
        }
        let Some((root, i)) = root.and_then(|e| Some((e.path.clone(), self.ws.index_of(&e.path)?)))
        else {
            self.toggled.remove(path);
            return;
        };
        let Some(mut set) = self.toggled.remove(&root) else {
            return;
        };
        set.retain(|id| self.still_there(i, id));
        if !set.is_empty() {
            self.toggled.insert(root, set);
        }
    }

    /// Whether the line at `id` of entry `i`'s details can still be there,
    /// as far as its status says: its file still changed in that section,
    /// its branch, stash or issue still had. Lines read on demand (a
    /// commit's) are taken to be.
    fn still_there(&self, i: usize, id: &[Seg]) -> bool {
        let e = &self.ws.entries[i];
        let Some(s) = e.status() else { return true };
        let changed = |section: &Section, f: &str| {
            let file = s.changes.files.iter().find(|c| c.path == f);
            file.is_some_and(|c| section.code(c).is_some())
        };
        match id {
            [Seg::Section(section), Seg::File(f), ..] => changed(section, f),
            // a nested repo: there, and its own lines by its own status
            [Seg::Section(section), Seg::Repo(rel), rest @ ..] => {
                changed(section, rel)
                    && self
                        .ws
                        .nested_index(&e.path, rel)
                        .is_none_or(|n| self.still_there(n, rest))
            }
            [Seg::Branches, Seg::Branch(b), ..] => s.branches.iter().any(|x| x.name == *b),
            [Seg::Stashes, Seg::Stash(id), ..] => s.stash_ids.contains(id),
            // an issue, and under it whatever it unfolds into
            [Seg::Issue(name, _), rest @ ..] => {
                let shown = e.issues.iter().chain(&e.ignored_issues);
                shown.chain(&e.snoozed_issues).any(|i| i.name() == *name)
                    && self.still_there(i, rest)
            }
            _ => true,
        }
    }

    /// What Tab at the cursor folds or unfolds: the line there if lines
    /// can be under it, else the nearest it's under; by its listed repo
    /// and place. None for a line under nothing (the repo's own, `path`).
    fn folding(&self) -> Option<(PathBuf, Vec<Seg>)> {
        let (top, range) = self.rows_of(self.cursor)?;
        let Row::Repo(i) = self.rows[top] else {
            return None;
        };
        let id = self.id(self.cursor);
        let folds = |d: &usize| {
            let at = range.clone().find(|&n| self.id(n) == &id[..*d]);
            matches!(at.map(|n| &self.rows[n]), Some(Row::Detail { line, .. }) if line.folds())
        };
        let d = (1..=id.len()).rev().find(folds)?;
        Some((self.ws.entries[i].path.clone(), id[..d].to_vec()))
    }

    /// Whether a switch is on this session.
    pub fn switch_on(&self, s: Switch) -> bool {
        match s {
            Switch::Untracked => self.stash.untracked,
            Switch::KeepIndex => self.stash.keep_index,
            Switch::Message => self.stash.message,
            Switch::Anchored => self.gitignore.as_ref().is_some_and(|g| g.anchored),
        }
    }

    /// A stash of `kind`, now or (the message switch on) once its message
    /// has been asked for.
    fn stash_action(&mut self, kind: StashKind) -> Vec<Effect> {
        let marked = !self.marked_files.is_empty() || !self.lines().is_empty();
        if kind == StashKind::MarkedFiles && !marked {
            self.info("mark files in the details view first (Tab, then space on a file)");
            return vec![];
        }
        // (marks in a repo the cursor isn't in: which are meant?)
        if kind == StashKind::MarkedFiles {
            let here = self.current_path();
            let here = here.filter(|p| self.repos_with_marks().contains(p));
            let this = here.map(|_| ('m', Action::Stash(StashKind::MarkedHere)));
            let marks = self.repos_with_marks();
            if self.asks_which("Stash", marks, this, Action::StashAll) {
                return vec![];
            }
        }
        self.stash_asked(kind)
    }

    /// A stash of `kind`, its marks' repos settled: now or (the message
    /// switch on) once its message has been asked for.
    fn stash_asked(&mut self, kind: StashKind) -> Vec<Effect> {
        if !self.stash.message {
            return self.stash_now(kind, None);
        }
        self.mode = Mode::Input {
            prompt: Prompt::StashMessage(kind),
            editor: LineEditor::new(""),
        };
        vec![]
    }

    fn stash_now(&mut self, kind: StashKind, message: Option<String>) -> Vec<Effect> {
        let mut lines = vec![];
        let targets: Vec<(usize, Vec<String>)> = match kind {
            StashKind::All | StashKind::Staged => {
                self.selected().into_iter().map(|i| (i, vec![])).collect()
            }
            StashKind::MarkedFiles | StashKind::MarkedHere => {
                match self.marked_stashes(kind == StashKind::MarkedHere) {
                    Ok((by_lines, by_path)) => {
                        // (taken: a region is over, its lines marks)
                        self.keep_region();
                        lines = by_lines;
                        by_path
                    }
                    Err(why) => {
                        self.error(format!("not stashing: {why}"));
                        return vec![];
                    }
                }
            }
        };
        let fx = self.stash_jobs(targets, kind == StashKind::Staged, message, lines);
        if kind == StashKind::MarkedFiles {
            let jobs = fx.iter().filter_map(|e| match e {
                Effect::Jobs(jobs) => Some(jobs.iter().map(|j| j.path.clone())),
                _ => None,
            });
            let mut repos: Vec<PathBuf> = jobs.flatten().collect();
            repos.dedup();
            self.note_elsewhere(&repos);
        }
        fx
    }

    /// What stashes the marks, by repo: with lines marked, a stash of
    /// lines (its message to come), else of the marked files by their
    /// paths. Everything goes only where it's marked. A stash by path
    /// takes all of a file, so a file listed twice has to be marked twice
    /// for that; marked under unstaged only, its unstaged lines are
    /// stashed by themselves. An error says what can't go either way.
    #[allow(clippy::type_complexity)]
    fn marked_stashes(
        &mut self,
        here: bool,
    ) -> Result<(Vec<(PathBuf, Op)>, Vec<(usize, Vec<String>)>), String> {
        // (`here`: what's marked in the cursor's repo alone)
        let only = self.current_path().filter(|_| here);
        let (taken, marked) = self.taken_in(only.as_deref());
        let taken = if marked { taken } else { vec![] };
        // by repo: file -> where it's marked whole; file -> hunk -> lines
        type Lines = BTreeMap<String, BTreeMap<usize, Vec<usize>>>;
        let mut whole: BTreeMap<PathBuf, BTreeMap<String, Vec<Section>>> = BTreeMap::new();
        let mut lines: BTreeMap<PathBuf, Lines> = BTreeMap::new();
        for target in taken {
            match target {
                Target::File {
                    repo,
                    section,
                    file,
                } => whole
                    .entry(repo)
                    .or_default()
                    .entry(file)
                    .or_default()
                    .push(section),
                Target::Hunk { section, .. } if section != Section::Unstaged => {
                    return Err("some marked lines are staged (z g stashes what's staged): \
                         unmark them (space) first"
                        .into());
                }
                Target::Hunk {
                    repo,
                    file,
                    hunk,
                    at: Some(at),
                    ..
                } => {
                    let hunks = lines.entry(repo).or_default().entry(file).or_default();
                    hunks.entry(hunk).or_default().push(at);
                }
                Target::Hunk { .. } | Target::Section { .. } | Target::Repo { .. } => {}
            }
        }
        let repos: BTreeSet<PathBuf> = whole.keys().chain(lines.keys()).cloned().collect();
        let (mut by_lines, mut by_path) = (vec![], vec![]);
        for repo in repos {
            let files = whole.remove(&repo).unwrap_or_default();
            let mut chosen = lines.remove(&repo).unwrap_or_default();
            // where each is listed, to tell one marked everywhere it is
            // (stashed by its path) from one marked on one side
            let status = self.ws.get(&repo).and_then(Entry::status);
            let listed = |file: &str| -> Vec<Section> {
                let change = status.and_then(|s| s.changes.entry(file));
                let lists = |s: &Section| change.is_some_and(|c| s.code(c).is_some());
                Section::ALL.into_iter().filter(lists).collect()
            };
            let gone = |file: &str| {
                let change = status.and_then(|s| s.changes.entry(file));
                change.is_some_and(|c| c.worktree == 'D')
            };
            let mut paths: Vec<String> = vec![];
            let mut unstaged: Vec<String> = vec![];
            for (file, marked) in &files {
                let listed = listed(file);
                if listed.iter().all(|s| marked.contains(s)) {
                    paths.push(file.clone());
                } else if marked == &[Section::Unstaged] {
                    unstaged.push(file.clone());
                } else {
                    return Err(format!(
                        "{file} is listed {} too, and stashing a file takes all its changes: \
                         mark it there as well (or z g stashes what's staged)",
                        match listed.iter().find(|s| !marked.contains(s)) {
                            Some(Section::Staged) => "under staged",
                            Some(Section::Untracked) => "under untracked",
                            _ => "under unstaged",
                        }
                    ));
                }
            }
            if chosen.is_empty() && unstaged.is_empty() {
                if let Some(i) = self.ws.index_of(&repo) {
                    by_path.push((i, paths));
                }
                continue;
            }
            // a stash of lines: the marked ones, and all those of the
            // files marked whole, which have to be text lines to take
            let takable: Vec<(String, bool)> = paths
                .iter()
                .chain(&unstaged)
                .map(|file| {
                    let lines_only = listed(file) == [Section::Unstaged] || unstaged.contains(file);
                    (file.clone(), lines_only && !gone(file))
                })
                .collect();
            for (file, takable) in &takable {
                let text = *takable
                    && !ops::mode_changed(&repo, file)
                    && self
                        .text_diff(&repo, file, Side::Unstaged)
                        .is_some_and(|hunks| !hunks.is_empty());
                if !text {
                    return Err(format!(
                        "{file} can't go in a stash of lines, which takes unstaged changes \
                         to text (not a file untracked, staged, deleted, or its mode): \
                         unmark it, or the lines"
                    ));
                }
                let all = self.changed_lines(&repo, file, Side::Unstaged);
                let hunks = chosen.entry(file.clone()).or_default();
                for (h, at) in all.unwrap_or_default() {
                    hunks.entry(h).or_default().push(at);
                }
            }
            let (mut stage, mut discard, mut n) = (String::new(), String::new(), 0);
            for (file, hunks) in &chosen {
                let key = (repo.clone(), file.clone(), Side::Unstaged);
                let Some(Ok(FileDiff::Text(all))) = self.diffs.get(&key) else {
                    continue;
                };
                let parts: Vec<(&diff::Hunk, Option<&[usize]>)> = hunks
                    .iter()
                    .filter_map(|(h, at)| Some((all.get(*h)?, Some(at.as_slice()))))
                    .collect();
                stage.push_str(&diff::patch_hunks(file, &parts, false));
                discard.push_str(&diff::patch_hunks(file, &parts, true));
                n += hunks.values().map(Vec::len).sum::<usize>();
            }
            let op = Op::StashLines {
                message: String::new(),
                stage,
                discard,
                lines: n,
            };
            by_lines.push((repo, op));
        }
        Ok((by_lines, by_path))
    }

    /// Pop or drop a stash: the one under the cursor, else the repo's
    /// only one, else one picked from them (the newest first, so Enter
    /// takes it).
    fn stash_pick(&mut self, title: &str, op: fn(usize, String) -> Op) -> Vec<Effect> {
        if let Some(Row::Detail {
            repo,
            line: DetailLine { stash: Some(n), .. },
        }) = self.rows.get(self.cursor)
        {
            let (e, n) = (&self.ws.entries[*repo], *n);
            let id = e.status().and_then(|s| s.stash_ids.get(n)).cloned();
            let path = e.path.clone();
            return match id {
                Some(id) => self.run_or_ask(path, op(n, id)),
                None => vec![],
            };
        }
        let [i] = self.selected()[..] else {
            self.error("pick a stash of one repo at a time");
            return vec![];
        };
        let e = &self.ws.entries[i];
        let stashes = e.status().map(|s| s.stashes.iter().zip(&s.stash_ids));
        let (mut names, mut ops) = (vec![], vec![]);
        for (n, (message, id)) in stashes.into_iter().flatten().enumerate() {
            names.push(format!("stash@{{{n}}} {message}"));
            ops.push(op(n, id.clone()));
        }
        if names.is_empty() {
            return self.not_applicable("no stash");
        }
        // (the only one: nothing to pick)
        if ops.len() == 1 {
            let path = e.path.clone();
            return self.run_or_ask(path, ops.remove(0));
        }
        self.picks = Some((e.path.clone(), ops));
        self.mode = Mode::Pick {
            title: format!("{title} · {}", e.display_name()),
            editor: LineEditor::new(""),
            shown: (0..names.len()).collect(),
            labels: names.clone(),
            names,
            cursor: 0,
        };
        vec![]
    }

    /// Run `op` in the repo at `path`; one that can't be undone is asked
    /// about first.
    fn run_or_ask(&mut self, path: PathBuf, op: Op) -> Vec<Effect> {
        let Some(e) = self.ws.get(&path) else {
            return vec![];
        };
        if let Op::StashDrop { index, .. } = &op {
            let message = e.status().and_then(|s| s.stashes.get(*index).cloned());
            self.mode = Mode::Confirm {
                question: format!(
                    "Drop stash@{{{index}}} ({}) of {}? It can't be recovered.",
                    message.unwrap_or_default(),
                    e.display_name()
                ),
                then: Confirmed::Ops(vec![(path, op)]),
            };
            return vec![];
        }
        let jobs = self.job(&path, op).into_iter().collect();
        self.start_jobs(jobs)
    }

    /// Stash each target repo (with paths, if any) using this session's
    /// switches. A repo git can't stash cleanly refuses the lot.
    fn stash_jobs(
        &mut self,
        targets: Vec<(usize, Vec<String>)>,
        staged: bool,
        message: Option<String>,
        lines: Vec<(PathBuf, Op)>,
    ) -> Vec<Effect> {
        let Op::Stash {
            message: default_message,
            ..
        } = Op::stash(&self.stash)
        else {
            unreachable!()
        };
        let message = |default: &String| message.clone().unwrap_or_else(|| default.clone());
        // stashes of marked lines, as made by marked_lines_stashes
        let mut jobs = vec![];
        for (path, mut op) in lines {
            if let Op::StashLines { message: m, .. } = &mut op {
                *m = message(&default_message);
            }
            jobs.extend(self.job(&path, op));
        }
        let mut refused = vec![];
        for (i, paths) in targets {
            let e = &self.ws.entries[i];
            let Some(s) = e.status() else { continue };
            // (an untracked file that's marked is asked for: the switch is
            // for what isn't named)
            let named = |p: &String| s.changes.entry(p).is_some_and(|c| c.untracked());
            let untracked = self.stash.untracked || paths.iter().any(named);
            match ops::stash_problem(&e.path, s, untracked, staged, &paths) {
                Some(why) if why == ops::NOTHING_TO_STASH => continue,
                Some(why) => refused.push(format!("{}: {why}", e.display_name())),
                None => {
                    let op = Op::Stash {
                        message: message(&default_message),
                        untracked,
                        keep_index: self.stash.keep_index,
                        staged,
                        paths,
                    };
                    jobs.push(Job {
                        label: format!("{} {}", op.label(), e.display_name()),
                        family: e.parent.clone().unwrap_or_else(|| e.path.clone()),
                        path: e.path.clone(),
                        op,
                    });
                }
            }
        }
        if !refused.is_empty() {
            self.error(format!("not stashing: {}", refused.join("; ")));
            return vec![];
        }
        if jobs.is_empty() {
            self.info("nothing to stash");
            return vec![];
        }
        let fx = self.start_jobs(jobs);
        self.unmark_lines_started(&fx);
        fx
    }

    /// Start `jobs`, except in repos already running something (said so).
    fn start_jobs(&mut self, mut jobs: Vec<Job>) -> Vec<Effect> {
        let busy: BTreeSet<PathBuf> = jobs
            .iter()
            .filter(|j| self.running.contains_key(&j.path))
            .map(|j| j.path.clone())
            .collect();
        jobs.retain(|j| !busy.contains(&j.path));
        let busy = skipped(busy.len(), "busy");
        if jobs.is_empty() {
            if !busy.is_empty() {
                self.info(format!("nothing started{busy}"));
            }
            return vec![];
        }
        for j in &jobs {
            self.running.insert(j.path.clone(), j.op.label());
        }
        self.info(format!("running {} job(s)…{busy}", jobs.len()));
        vec![Effect::Jobs(jobs)]
    }

    /// Add `note` to the status line's message.
    fn add_note(&mut self, note: &str) {
        if let Some((m, _)) = &mut self.message {
            m.push_str(note);
        }
    }

    /// A job running `op` in the repo at `path`, if it's still there.
    fn job(&self, path: &std::path::Path, op: Op) -> Option<Job> {
        let e = self.ws.get(path)?;
        Some(Job {
            label: format!("{} {}", op.label(), e.display_name()),
            family: e.parent.clone().unwrap_or_else(|| path.to_path_buf()),
            path: path.to_path_buf(),
            op,
        })
    }

    fn submit(&mut self, prompt: Prompt, text: String) -> Vec<Effect> {
        match prompt {
            Prompt::Filter => {
                self.filter = text;
                self.rebuild();
                vec![]
            }
            Prompt::AmendMessage(path, message, _) => {
                if text.trim().is_empty() {
                    self.error("empty commit message: aborted");
                    return vec![];
                }
                // a new summary line; the rest of the message stays
                let summary = message.lines().next().unwrap_or_default();
                let message =
                    (text != summary).then(|| format!("{text}{}", &message[summary.len()..]));
                let jobs = self.job(&path, Op::Amend { message }).into_iter().collect();
                self.start_jobs(jobs)
            }
            Prompt::CommitMessage(paths, all) => {
                if text.trim().is_empty() {
                    self.error("empty commit message: aborted");
                    return vec![];
                }
                let jobs = paths
                    .into_iter()
                    .filter_map(|p| {
                        self.ws
                            .get(&p)
                            .map(|e| (p, e.display_name(), e.parent.clone()))
                    })
                    .map(|(path, name, parent)| Job {
                        label: format!("commit {name}"),
                        family: parent.unwrap_or_else(|| path.clone()),
                        path,
                        op: match all {
                            true => Op::CommitAll {
                                message: text.clone(),
                            },
                            false => Op::Commit {
                                message: text.clone(),
                            },
                        },
                    })
                    .collect();
                self.start_jobs(jobs)
            }
            Prompt::BranchName(_) | Prompt::RemoteUrl(_) if text.trim().is_empty() => {
                self.error("nothing entered: aborted");
                vec![]
            }
            Prompt::BranchName(paths) => {
                let name = text.trim().to_string();
                let jobs = paths
                    .iter()
                    .filter_map(|p| self.job(p, Op::CreateBranch { name: name.clone() }))
                    .collect();
                self.start_jobs(jobs)
            }
            Prompt::SquashMessage(path, commit, instant) => {
                let op = Op::Fixup {
                    commit,
                    squash: Some(text),
                    instant,
                };
                let jobs = self.job(&path, op).into_iter().collect();
                self.start_jobs(jobs)
            }
            Prompt::SpinOffName(paths, stay) => {
                let name = text.trim().to_string();
                let jobs = paths
                    .iter()
                    .filter_map(|p| {
                        let op = Op::SpinOff {
                            name: name.clone(),
                            stay,
                        };
                        self.job(p, op)
                    })
                    .collect();
                self.start_jobs(jobs)
            }
            Prompt::RemoteUrl(path) => {
                let op = Op::AddRemote {
                    name: "origin".into(),
                    url: text.trim().to_string(),
                };
                let jobs = self.job(&path, op).into_iter().collect();
                self.start_jobs(jobs)
            }
            Prompt::StashMessage(kind) => {
                let message = (!text.trim().is_empty()).then_some(text);
                self.stash_now(kind, message)
            }
            Prompt::SnoozeIssue(key, issue) => match argus_core::state::parse_duration(&text) {
                Some(secs) => {
                    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(secs);
                    self.ws.state.snooze_issue(&key, issue, until);
                    self.info(format!(
                        "snoozed {} for {text}",
                        issue.name().replace('_', " ")
                    ));
                    self.flags_changed()
                }
                None => {
                    self.error(format!("bad duration {text:?} (try 90m, 12h, 3d, 2w)"));
                    vec![]
                }
            },
            Prompt::SnoozeDuration(keys) => match argus_core::state::parse_duration(&text) {
                Some(secs) => {
                    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(secs);
                    for k in keys {
                        self.ws.state.snoozed.insert(k, until);
                    }
                    self.info(format!("snoozed for {text}"));
                    self.flags_changed()
                }
                None => {
                    self.error(format!("bad duration {text:?} (try 90m, 12h, 3d, 2w)"));
                    vec![]
                }
            },
        }
    }

    // ---------- messages ----------

    pub fn on_msg(&mut self, m: Msg) -> Vec<Effect> {
        let mut fx = self.msg(m);
        fx.extend(self.start_queued());
        self.show_waiting();
        fx
    }

    /// Statuses that arrived together (a refresh of every repo brings one
    /// each): all applied, then the rows rebuilt once for the lot.
    pub fn on_statuses(
        &mut self,
        batch: Vec<(PathBuf, Result<RepoStatus, String>)>,
    ) -> Vec<Effect> {
        if batch.is_empty() {
            return vec![];
        }
        let mut fx = self.statuses(batch);
        fx.extend(self.start_queued());
        self.show_waiting();
        fx
    }

    fn statuses(&mut self, batch: Vec<(PathBuf, Result<RepoStatus, String>)>) -> Vec<Effect> {
        let paths: Vec<PathBuf> = batch.iter().map(|(p, _)| p.clone()).collect();
        self.ws.update_statuses(batch);
        let went = self.reread_diffs(|repo| paths.contains(repo));
        self.unfold_cache.retain(|k, _| !paths.contains(&k.0));
        for path in &paths {
            self.forget_gone(path);
        }
        self.rebuild();
        let mut fx = self.opened();
        for path in &paths {
            fx.extend(self.continue_chain(path));
        }
        if let Some(went) = went {
            // (said beside a failure, not over it)
            match &self.message {
                Some((_, true)) => self.add_note(&format!(" · {went}")),
                _ => self.info(went),
            }
        }
        fx
    }

    fn msg(&mut self, m: Msg) -> Vec<Effect> {
        match m {
            Msg::Scanned { entries, truncated } => {
                self.scanning = false;
                let went = self.reread_diffs(|_| true);
                self.unfold_cache.clear();
                self.ws.entries = entries;
                self.ws.truncated = truncated;
                self.ws.resolve_flags();
                let hidden = self.ws.entries.iter().filter(|e| e.flags.hidden()).count();
                let n = self.ws.entries.len();
                let mut parts = vec![match hidden {
                    0 => format!("{n} {}", plural(n, "repo", "repos")),
                    h => format!("{n} {}, {h} hidden", plural(n, "repo", "repos")),
                }];
                // (what's marked in a repo no longer listed goes with it; the
                // rows are the old ones still, so nothing here reads them)
                let listed = |repo: &PathBuf| self.ws.index_of(repo).is_some();
                self.marked_files.retain(|repo, _| listed(repo));
                self.marked_lines.retain(|k| listed(&k.0));
                self.diffs.retain(|k, _| listed(&k.0));
                if self.region.as_ref().is_some_and(|r| !listed(&r.repo)) {
                    self.region = None;
                }
                parts.extend(self.notices.iter().cloned());
                parts.extend(self.ws.truncation_warning());
                parts.extend(went);
                self.info(parts.join(" · "));
                self.rebuild();
                if let Some(here) = self.start_in.take() {
                    self.start_on_repo_holding(&here);
                }
            }
            Msg::Status(path, st) => {
                return self.statuses(vec![(path, *st)]);
            }
            Msg::Forge {
                path,
                branches,
                info,
            } => {
                // an answer to an earlier question than the last is stale
                if let Some(look) = self.forge.get_mut(&path)
                    && look.asked.as_ref() == Some(&branches)
                {
                    look.answer = Some(info);
                    self.rebuild();
                }
            }
            Msg::Step(step) => self.remember(Happened::Step(step)),
            Msg::Described(path) => {
                self.describing.remove(&path);
                // status now has the remembered answer
                return vec![Effect::Refresh(vec![path])];
            }
            Msg::JobStarted { path, label } => {
                self.running.insert(path, label);
            }
            Msg::JobFinished {
                path,
                label,
                op,
                outcome,
            } => {
                self.running.remove(&path);
                self.push_log(
                    &path,
                    label.clone(),
                    outcome.success,
                    outcome.output.clone(),
                );
                if !outcome.success {
                    self.error(format!("{label} failed ($ for log)"));
                }
                self.ask_to_pop_plain(&path, &op, &outcome);
                if let Op::Apply { .. } = op
                    && let Some(e) = self.editing.as_mut()
                    && e.applying
                    && e.repo == path
                {
                    if outcome.success {
                        self.drop_edit();
                    } else {
                        e.applying = false;
                        self.ask_when_free(
                            "The edited patch doesn't apply ($ for why): edit it again?".into(),
                            Confirmed::EditAgain,
                        );
                    }
                }
                if let Op::Clone { .. } = op {
                    if outcome.success {
                        let jobs = [Job {
                            path: path.clone(),
                            op: op.clone(),
                            family: path.clone(),
                            label,
                        }];
                        pull::record_clones(&mut self.ws, &jobs, &[Some(outcome)]);
                        self.rebuild();
                        return vec![Effect::SaveState];
                    }
                    return vec![];
                }
                if self.running.is_empty() && self.message.as_ref().is_none_or(|m| !m.1) {
                    self.info("done");
                }
                // its main repo too, before a removed worktree's row goes
                let refresh = self.ws.with_parents(&path);
                if outcome.success && self.drop_removed(&path, &op) {
                    self.rebuild();
                }
                return vec![Effect::Refresh(refresh)];
            }
            Msg::FixDone { path, applied } => {
                self.running.remove(&path);
                let name = self
                    .ws
                    .get(&path)
                    .map(|e| e.display_name())
                    .unwrap_or_default();
                // a fix-auto action goes on from a fix that worked, once
                // the repo's status has been read again
                let mut more = false;
                match applied {
                    Ok(list) => {
                        if let Some(a) = list {
                            let ok = a.outcome.success;
                            self.ask_to_pop_plain(&path, &a.fix.op, &a.outcome);
                            self.push_log(&path, a.fix.op.label(), ok, a.outcome.output);
                            if let Some(chain) = self.chains.get_mut(&path).filter(|_| ok) {
                                chain.done.push(a.fix.clone());
                                chain.read_again = false;
                                more = true;
                            }
                            if !ok {
                                self.error(format!(
                                    "{name}: {} failed ($ for log)",
                                    a.fix.op.label()
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        self.push_log(&path, "fix".into(), false, e.clone());
                        self.error(format!("{name}: {e}"));
                    }
                }
                if !more {
                    self.chains.remove(&path);
                    self.fix_finished();
                }
                return vec![Effect::Refresh(self.ws.with_parents(&path))];
            }
            Msg::PullPlanned {
                tracks,
                plan,
                errors,
            } => {
                self.ws.state.tracks = tracks.tracks;
                self.ws.resolve_flags();
                for (k, e) in &errors {
                    self.push_log(&PathBuf::from(k), "list forge".into(), false, e.clone());
                }
                for r in &plan.missing {
                    self.push_log(
                        &PathBuf::new(),
                        format!("new on forge: {}", r.id.slug()),
                        true,
                        String::new(),
                    );
                }
                for o in &plan.orphans {
                    // the clone may be the only copy left: hiding it is
                    // all that's offered
                    let hint = match o.state {
                        pull::OrphanState::Gone => " (t I ignores it)",
                        _ => "",
                    };
                    let label = format!("orphan: {}{hint}", o.describe());
                    self.push_log(&o.path, label, false, String::new());
                }
                let renames = pull::follow_renames(&self.ws, &plan.orphans);
                let mut jobs = plan.jobs;
                jobs.extend(pull::plan_updates(&self.ws));
                self.ws.state.last_pull = Some(jiff::Timestamp::now());
                let mut fx = self.start_jobs(jobs);
                if !errors.is_empty() {
                    self.error(format!(
                        "{} forge listing(s) failed ($ for log)",
                        errors.len()
                    ));
                }
                fx.push(Effect::SaveState);
                self.rebuild();
                // a renamed repo's remote is pointed at its new name only
                // when asked
                let names: Vec<String> = renames
                    .iter()
                    .filter_map(|j| Some(self.ws.get(&j.path)?.display_name()))
                    .collect();
                let question = match (renames.as_slice(), names.as_slice()) {
                    ([], _) => None,
                    ([j], [name]) => Some(format!(
                        "{name} is now {} on the forge: point its remote there?",
                        j.label.rsplit(" to ").next().unwrap_or_default()
                    )),
                    _ => Some(format!(
                        "{} repos have new names on the forge ({}): point their remotes there?",
                        renames.len(),
                        fix::list(&names, 3)
                    )),
                };
                if let Some(question) = question {
                    let ops = renames.into_iter().map(|j| (j.path, j.op)).collect();
                    self.ask_when_free(question, Confirmed::Ops(ops));
                }
                return fx;
            }
            Msg::Suspended(r) if self.editing.as_ref().is_some_and(|e| !e.applying) => {
                return self.edited(r);
            }
            Msg::Suspended(r) => {
                if let Err(e) = r {
                    self.error(e);
                }
                if let Some(p) = self.current_path() {
                    return vec![Effect::Refresh(self.ws.with_parents(&p))];
                }
            }
        }
        vec![]
    }

    fn push_log(&mut self, path: &std::path::Path, label: String, ok: bool, output: String) {
        let repo = self
            .ws
            .get(path)
            .map(|e| e.display_name())
            .unwrap_or_else(|| paths::contract_tilde(path));
        self.remember(Happened::Job {
            repo: repo.clone(),
            label: label.clone(),
            ok,
            output: output.clone(),
        });
        self.log.push(LogEntry {
            at: jiff::Timestamp::now(),
            repo,
            label,
            ok,
            output,
        });
        if self.log.len() > 1000 {
            self.log.drain(..100);
        }
    }
}

/// What a commit list offers: the repo, what for, and its recent commits.
struct CommitPick {
    path: PathBuf,
    name: String,
    purpose: Pick,
    commits: Vec<argus_core::history::CommitLine>,
}

/// Whether `file` is in the untracked folder `folder` (`build/`).
fn within(folder: &str, file: &str) -> bool {
    folder.ends_with('/') && file != folder && file.starts_with(folder)
}

/// What a row of a repo's changes is (see [`App::target`]): where it's
/// listed is part of what it is, so a file under staged and the same file
/// under unstaged are two things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A repo itself: everything of its changes a key is for. What the
    /// cursor is on when it's on the repo's own row, or on a row of its
    /// details that's none of its changes; never a row to mark.
    Repo { repo: PathBuf },
    /// A section's heading: the files it lists.
    Section { repo: PathBuf, section: Section },
    File {
        repo: PathBuf,
        section: Section,
        file: String,
    },
    /// A hunk of a file's diff (its own line), or with `at` one added or
    /// removed line of it.
    Hunk {
        repo: PathBuf,
        section: Section,
        file: String,
        hunk: usize,
        at: Option<usize>,
    },
}

/// How much of a file, or of a hunk of its diff, is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marked {
    No,
    /// Some of its lines.
    Partly,
    /// The file itself, or every added and removed line.
    Wholly,
}

/// An added or removed line marked in a diff: repo, file, side, hunk and
/// the line's index in it.
pub type MarkedLine = (PathBuf, String, argus_core::diff::Side, usize, usize);

/// A patch being edited (`e`): in `file`, for the repo at `repo`, to stage
/// (or, `reverse`, unstage) and naming only `files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchEdit {
    pub repo: PathBuf,
    pub file: PathBuf,
    pub reverse: bool,
    pub files: Vec<String>,
    /// Saved, and being applied.
    pub applying: bool,
}

impl PatchEdit {
    /// git's editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`, as
    /// that repo's git picks it) on the patch, run as git runs it.
    fn open(&self) -> Effect {
        Effect::Suspend {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                r#"e=$(git var GIT_EDITOR) && eval "$e \"\$1\"""#.into(),
                "argus".into(),
                self.file.display().to_string(),
            ],
            cwd: self.repo.clone(),
            login_shell: false,
        }
    }
}

/// Numbers the patch files of this process's edits.
static EDITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How to edit, after the patch (as `git add -p`'s `e` says it).
fn edit_instructions(reverse: bool) -> &'static str {
    if reverse {
        "# ---\n\
         # Edit the patch, then save and quit: what's left is unstaged.\n\
         # To keep a '+' line staged, make it a ' ' (context) line.\n\
         # To keep a '-' line's removal staged, delete it.\n\
         # Lines starting with # are dropped; delete everything to cancel.\n\
         # If it doesn't apply, you're asked whether to edit it again.\n"
    } else {
        "# ---\n\
         # Edit the patch, then save and quit: what's left is staged.\n\
         # To leave a '-' line out, make it a ' ' (context) line.\n\
         # To leave a '+' line out, delete it.\n\
         # Lines starting with # are dropped; delete everything to cancel.\n\
         # If it doesn't apply, you're asked whether to edit it again.\n"
    }
}

/// Why an edited patch can't be applied as it is, if it can't: it may
/// change only the hunks of `files`, each under its own header. (With
/// `--recount`, git ends a hunk only at an `@@` or `diff` line, so a
/// `---`/`+++` line inside one is a removed or added line.)
fn check_patch(patch: &str, files: &[String]) -> Result<(), String> {
    let mut lines = patch.lines();
    let mut started = false;
    while let Some(l) = lines.next() {
        if let Some(rest) = l.strip_prefix("diff ") {
            let Some(f) = files.iter().find(|f| rest == format!("--git a/{f} b/{f}")) else {
                return Err(format!("names another file ({l})"));
            };
            if lines.next() != Some(&format!("--- a/{f}"))
                || lines.next() != Some(&format!("+++ b/{f}"))
            {
                return Err(format!("has {f}'s header changed"));
            }
            started = true;
        } else if !started {
            return Err("lost its diff --git header".into());
        } else if !(l.is_empty() || l.starts_with([' ', '+', '-', '\\']) || l.starts_with("@@ ")) {
            return Err(format!("has a line that isn't a hunk's ({l})"));
        }
    }
    Ok(())
}

/// A place in a file's diff, in the diff's own order: its hunk, then
/// within it the header (0) or a line (twice its place, plus one).
/// [`DIFF_START`] is before all of it, [`DIFF_END`] after.
type DiffPos = (usize, usize);
const DIFF_START: DiffPos = (0, 0);
const DIFF_END: DiffPos = (usize::MAX, usize::MAX);

/// A region (`C-space`): where it starts in a file's diff. It runs from
/// there to the cursor, in the diff's own order: its lines are the diff's,
/// not the rows between two rows, so it's the same whatever is folded or
/// shown twice. The cursor is somewhere in the diff only while it's in
/// the copy the region was started in (`home`, under `root`); before that
/// copy it's the diff's start, after it the diff's end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionStart {
    /// The repo the diff is of (a nested repo's own path).
    pub repo: PathBuf,
    pub file: String,
    pub side: argus_core::diff::Side,
    at: DiffPos,
    /// The listed repo the rows are under, and the file's line there.
    root: PathBuf,
    home: Vec<Seg>,
}

/// What `s`/`u`/`K` do to a hunk or line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkVerb {
    Stage,
    Unstage,
    Discard,
}

/// What a picked commit is for.
#[derive(Clone, Copy)]
enum Pick {
    Reset(ResetMode),
    Fixup { squash: bool, instant: bool },
}

/// The untracked files a gitignore menu is for, by repo, and whether its
/// rules are anchored.
struct GitignoreTargets {
    targets: Vec<(PathBuf, Vec<String>)>,
    anchored: bool,
}

/// An ignore menu item: the state's symbol, then the issue and, when
/// ignored, how: `[ignored]`, or `[ignored@feat]` for just this value. For
/// counted stashes, how many config patterns ignore anyway (`partly`).
fn state_label(
    issue: Issue,
    list: IssueList,
    state: IgnoreState,
    partly: usize,
    s: &RepoStatus,
    icons: &argus_core::icons::Icons,
) -> String {
    let (_, text) = describe(&issue, s.default_branch.as_deref());
    let first = list.values(s).into_iter().next().unwrap_or_default();
    let (symbol, how) = match state {
        IgnoreState::Counted if partly > 0 => (
            &icons.issue_counted,
            format!(", {partly} ignored by patterns"),
        ),
        IgnoreState::Counted => (&icons.issue_counted, String::new()),
        IgnoreState::Pinned => (
            &icons.issue_pinned,
            match list.values(s).len() {
                n if n > 1 => format!(" [ignored@{n} messages]"),
                _ => format!(" [ignored@{first}]"),
            },
        ),
        IgnoreState::Ignored => (&icons.issue_ignored, " [ignored]".into()),
    };
    format!("{} {text}{how}", symbol.trim_end())
}

/// The fixes of one kind an action runs, and what it leaves out.
#[derive(Default)]
struct Planned {
    ops: Vec<(PathBuf, Op)>,
    /// Selected repos with no fix of the kind.
    none: usize,
    /// Fixes left out as blocked, and the first one's reason.
    blocked: usize,
    why: Option<String>,
}

impl Planned {
    /// ` · 2 not applicable · 1 blocked (acme/w: …)`, or nothing.
    fn note(&self) -> String {
        let mut s = skipped(self.none, "not applicable");
        if let Some(why) = &self.why {
            s.push_str(&format!("{} ({why})", skipped(self.blocked, "blocked")));
        }
        s
    }
}

/// ` · 2 busy`, or nothing for none.
fn skipped(n: usize, what: &str) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" · {n} {what}")
    }
}

/// `abort merge` -> `Abort merge`
fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn kinds_label(k: &[FixKind]) -> String {
    k.iter().map(|k| k.name()).collect::<Vec<_>>().join(", ")
}

/// Run a user-configured command line through `sh -c` so quoting works.
fn shell_command(cmd: &str, cwd: PathBuf) -> Effect {
    Effect::Suspend {
        program: "sh".into(),
        args: vec!["-c".into(), cmd.into()],
        cwd,
        login_shell: false,
    }
}

/// The name after `current` in `names`, wrapping; the first if unknown.
fn next<'a>(names: &[&'a str], current: &str) -> &'a str {
    let i = names.iter().position(|n| *n == current);
    names[i.map_or(0, |i| (i + 1) % names.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indenting_a_line_keeps_its_links_on_their_parts() {
        let mut l = crate::details::blank();
        l.parts = vec![(None, "see ".into()), (None, "#12".into())];
        l.links = vec![(1, "https://x/12".into())];
        let p = padded(l.clone(), 4);
        assert_eq!(p.parts[0].1, "    ");
        assert_eq!(p.links, [(2, "https://x/12".to_string())]);
        assert_eq!(p.parts[p.links[0].0].1, "#12");
        // inverse: not indented, as it was
        assert_eq!(padded(l.clone(), 0), l);
        // a line starting with an indent gets more of it, in the same part
        l.parts.insert(0, (None, "  ".into()));
        l.links = vec![(2, "https://x/12".into())];
        let p = padded(l.clone(), 4);
        assert_eq!(p.parts.len(), 3);
        assert_eq!(p.parts[0].1, "      ");
        assert_eq!(p.links, l.links);
        assert_eq!(padded(l.clone(), 0), l);
    }
}
