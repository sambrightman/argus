//! Counting commits reachable from one tip but from none of others: each
//! way to the answer gives git's (`git rev-list --count tip --not …`).

use std::path::Path;

use argus_core::reach::{self, Counted, How, exclusive};
use argus_core::remember::Remembered;
use argus_core::status::{StatusOptions, status};
use argus_core::testkit::{Sandbox, commit, git, write};
use gix::ObjectId;
use proptest::prelude::*;

fn id(r: &Path, rev: &str) -> ObjectId {
    ObjectId::from_hex(git(r, &["rev-parse", rev]).trim().as_bytes()).unwrap()
}

/// The remote branches' tips.
fn remote_tips(r: &Path) -> Vec<ObjectId> {
    let out = git(
        r,
        &["for-each-ref", "--format=%(objectname)", "refs/remotes"],
    );
    out.lines()
        .map(|l| ObjectId::from_hex(l.as_bytes()).unwrap())
        .collect()
}

/// The count, as a status would make it: with the repo's remembered
/// answers, kept after (so it's remembered for the next count).
fn count(r: &Path, tip: &str, likely: &[&str], others: &[ObjectId]) -> Counted {
    let repo = gix::open(r).unwrap();
    let likely: Vec<ObjectId> = likely.iter().map(|l| id(r, l)).collect();
    let mut remembered = Remembered::of(&repo);
    let counted = exclusive(&repo, id(r, tip), &likely, others, &mut remembered).unwrap();
    remembered.keep();
    counted
}

fn write_graph(r: &Path) {
    git(r, &["commit-graph", "write", "--reachable"]);
}

fn drop_graph(r: &Path) {
    let info = r.join(".git/objects/info");
    let _ = std::fs::remove_file(info.join("commit-graph"));
    let _ = std::fs::remove_dir_all(info.join("commit-graphs"));
}

/// A clone whose `main` has five commits, pushed, and twelve old remote
/// branches at its first three.
fn with_old_branches(sb: &Sandbox) -> std::path::PathBuf {
    sb.remote("acme", "r");
    let r = sb.clone_as_github("acme", "r", "r");
    for i in 1..=5 {
        write(&r, "f", &format!("{i}\n"));
        commit(&r, &format!("c{i}"));
    }
    git(&r, &["push", "-q"]);
    for i in 0..12 {
        let at = format!("HEAD~{}:refs/heads/old{i}", 2 + i % 3);
        git(&r, &["push", "-q", "origin", &at]);
    }
    git(&r, &["fetch", "-q"]);
    r
}

/// Two local commits on `feat`, from `main`.
fn two_more(r: &Path) {
    git(r, &["switch", "-q", "-c", "feat"]);
    for f in ["g", "h"] {
        write(r, f, "x\n");
        commit(r, f);
    }
}

#[test]
fn hiding_the_likely_tips_first_is_enough_when_it_leaves_nothing() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    let all = remote_tips(&r);
    let c = count(&r, "HEAD~1", &["origin/main"], &all);
    assert_eq!(
        c,
        Counted {
            n: 0,
            how: How::Likely
        }
    );
    // a tip that's one of the others needs no walk at all
    assert_eq!(count(&r, "origin/old0", &[], &all).how, How::IsAnother);
    // inverse: with no likely tip, the first walk leaves the whole history
    argus_core::remember::forget();
    let c = count(&r, "HEAD~1", &[], &all);
    assert_eq!(c.n, 0);
    assert_ne!(c.how, How::Likely);
}

#[test]
fn generations_rule_out_tips_that_cant_contain_whats_left() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    let all = remote_tips(&r);
    // the graph written before the new commits: they're bounded by their
    // parent, which it has; no old branch reaches that high
    write_graph(&r);
    two_more(&r);
    let pruned = Counted {
        n: 2,
        how: How::Pruned { hidden: 1 },
    };
    let walks = reach::walks();
    assert_eq!(count(&r, "feat", &["origin/main"], &all), pruned);
    assert_eq!(reach::walks(), walks + 1, "nothing more to hide: one walk");
    // and written after, with them in it
    argus_core::remember::forget();
    write_graph(&r);
    assert_eq!(count(&r, "feat", &["origin/main"], &all), pruned);
    // inverse: without a graph, every tip is hidden
    argus_core::remember::forget();
    drop_graph(&r);
    assert_eq!(
        count(&r, "feat", &["origin/main"], &all),
        Counted {
            n: 2,
            how: How::Full
        }
    );
}

/// A remote branch at one of the commits left (generation equal, not
/// greater) or past it holds it: it's hidden in the second walk.
#[test]
fn a_tip_that_could_contain_whats_left_is_hidden_too() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    let all = remote_tips(&r);
    let one = Counted {
        n: 1,
        how: How::Pruned { hidden: 2 },
    };
    write_graph(&r);
    assert_eq!(count(&r, "feat", &["origin/main"], &all), one);
}

/// A remote tip newer than the graph has no generation to compare: it
/// may contain what's left, so it's hidden.
#[test]
fn a_tip_the_graph_lacks_is_hidden_too() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    write_graph(&r);
    // a commit on g, made after the graph: it holds g, not h
    git(&r, &["switch", "-q", "-c", "other", "feat~1"]);
    write(&r, "j", "x\n");
    commit(&r, "j");
    git(&r, &["push", "-q", "origin", "other:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    let all = remote_tips(&r);
    let c = count(&r, "feat", &["origin/main"], &all);
    assert_eq!(
        c,
        Counted {
            n: 1,
            how: How::Pruned { hidden: 2 }
        }
    );
}

/// Commits the graph lacks can only be in tips it lacks too (it holds
/// every ancestor of what it has): with an old graph, every tip it has is
/// ruled out.
#[test]
fn tips_in_an_old_graph_cant_contain_newer_commits() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    let early = git(&r, &["rev-parse", "HEAD~2"]);
    let mut child = std::process::Command::new("git")
        .args(["commit-graph", "write", "--stdin-commits"])
        .current_dir(&r)
        .envs(argus_core::testkit::subprocess_env(&sb.root()))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), early.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    two_more(&r);
    let all = remote_tips(&r);
    // origin/main is newer than the graph: hidden as likely; the old
    // branches are in it and below the new commits
    let c = count(&r, "feat", &["origin/main"], &all);
    assert_eq!(
        c,
        Counted {
            n: 2,
            how: How::Pruned { hidden: 1 }
        }
    );
    // and with none likely, the tips the graph lacks are hidden
    argus_core::remember::forget();
    let c = count(&r, "feat", &[], &all);
    assert_eq!(c.n, 2);
    // inverse: every tip likely leaves nothing to prune
    argus_core::remember::forget();
    let every: Vec<String> = all.iter().map(ToString::to_string).collect();
    let every: Vec<&str> = every.iter().map(String::as_str).collect();
    assert_eq!(
        count(&r, "feat", &every, &all),
        Counted {
            n: 2,
            how: How::Full
        }
    );
}

#[test]
fn counts_are_remembered_for_the_same_tip_and_others() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    let all = remote_tips(&r);
    let first = count(&r, "feat", &["origin/main"], &all);
    let walks = reach::walks();
    let again = count(&r, "feat", &["origin/main"], &all);
    assert_eq!(
        again,
        Counted {
            n: first.n,
            how: How::Remembered
        }
    );
    assert_eq!(reach::walks(), walks, "no walk");
    // whatever the likely ones: they only make it faster
    assert_eq!(count(&r, "feat", &[], &all).how, How::Remembered);
    // beside another counted in the same status, both kept
    let repo = gix::open(&r).unwrap();
    let main = [id(&r, "origin/main")];
    let mut one = Remembered::of(&repo);
    let both = [id(&r, "feat~1"), id(&r, "feat")]
        .map(|tip| exclusive(&repo, tip, &main, &all, &mut one).unwrap().how);
    assert_ne!(both[0], How::Remembered);
    assert_eq!(both[1], How::Remembered);
    one.keep();
    assert_eq!(
        count(&r, "feat~1", &["origin/main"], &all).how,
        How::Remembered
    );
    // but only what the last status asked for: feat wasn't, so counted again
    assert_ne!(
        count(&r, "feat", &["origin/main"], &all).how,
        How::Remembered
    );
    // inverse: another set of others is counted afresh
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    let c = count(&r, "feat", &["origin/main"], &remote_tips(&r));
    assert_eq!(c.n, 1);
    assert_ne!(c.how, How::Remembered);
}

fn hiding(r: &Path, tip: &str, likely: &[&str], others: &[ObjectId], cap: usize) -> Vec<ObjectId> {
    let repo = gix::open(r).unwrap();
    let likely: Vec<ObjectId> = likely.iter().map(|l| id(r, l)).collect();
    let mut v = reach::hiding(&repo, id(r, tip), &likely, others, cap).unwrap();
    v.sort();
    v
}

/// For a list, the tips to hide: the likely ones and what could hold
/// what they leave; every one when that can't be told.
#[test]
fn a_list_hides_only_the_tips_that_could_hold_its_commits() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    let all = remote_tips(&r);
    let mut every = all.clone();
    every.sort();
    every.dedup();
    write_graph(&r);
    let mut two = vec![id(&r, "origin/main"), id(&r, "origin/side")];
    two.sort();
    assert_eq!(hiding(&r, "feat", &["origin/main"], &all, 100), two);
    // nothing left by the likely ones: they're enough
    assert_eq!(
        hiding(&r, "main~1", &["origin/main"], &all, 100),
        [id(&r, "origin/main")]
    );
    // a tip that's one of them: itself
    assert_eq!(
        hiding(&r, "origin/old0", &[], &all, 100),
        [id(&r, "origin/old0")]
    );
    // every one likely: every one, with no walk
    let names: Vec<String> = every.iter().map(ToString::to_string).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let walks = reach::walks();
    assert_eq!(hiding(&r, "feat", &names, &all, 100), every);
    assert_eq!(reach::walks(), walks);
    // inverse: more left than the cap, or no graph: every one
    assert_eq!(hiding(&r, "feat", &["origin/main"], &all, 1), every);
    drop_graph(&r);
    assert_eq!(hiding(&r, "feat", &["origin/main"], &all, 100), every);
}

/// The steps taken while `f` runs, those starting with `prefix`.
fn steps(prefix: &'static str, f: impl FnOnce()) -> Vec<String> {
    use std::sync::{Arc, Mutex};
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let into = got.clone();
    argus_core::trace::set_sink(Some((
        argus_core::trace::Form::Each,
        Arc::new(move |s: argus_core::trace::Step| {
            if s.what.starts_with(prefix) {
                into.lock().unwrap().push(s.what);
            }
        }),
    )));
    f();
    argus_core::trace::set_sink(None);
    got.lock().unwrap().clone()
}

/// The details' lists hide only what could hold their commits, and list
/// what git lists.
#[test]
fn commit_lists_hide_only_what_could_hold_them() {
    use argus_core::history::{Range, commits};
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    write_graph(&r);
    let ids = |range: &Range| -> Vec<String> {
        commits(&r, range, 50)
            .unwrap()
            .0
            .into_iter()
            .map(|c| c.id)
            .collect()
    };
    let rev_list = |args: &[&str]| -> Vec<String> {
        git(&r, &[&["rev-list"], args].concat())
            .lines()
            .map(String::from)
            .collect()
    };
    let not_on_remote = Range::NotOnRemote { tip: "feat".into() };
    let n = all_remote(&r);
    let said = steps("hiding ", || {
        assert_eq!(
            ids(&not_on_remote),
            rev_list(&["feat", "--not", "--remotes"])
        );
    });
    // origin/HEAD and origin/main are one tip; side could hold them
    assert_eq!(said, [format!("hiding 2 of {n} tips")]);
    // HEAD detached on a commit of its own
    git(&r, &["switch", "-q", "--detach", "feat"]);
    write(&r, "k", "x\n");
    commit(&r, "k");
    let orphans = rev_list(&["HEAD", "--not", "--branches", "--tags", "--remotes"]);
    assert_eq!(orphans.len(), 1);
    let said = steps("hiding ", || assert_eq!(ids(&Range::Orphans), orphans));
    // the branches, likely; nothing else could hold a commit the graph lacks
    assert_eq!(said, [format!("hiding 2 of {} tips", n + 2)]);
}

/// A branch's upstream is among the likely tips, named or as HEAD: what
/// it leaves is newer than every other tip.
#[test]
fn a_list_hides_the_branchs_upstream_first() {
    use argus_core::history::{Range, commits};
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    write_graph(&r);
    write(&r, "i", "x\n");
    commit(&r, "i");
    git(&r, &["fetch", "-q"]);
    let n = all_remote(&r);
    for tip in ["HEAD", "feat"] {
        let range = Range::NotOnRemote { tip: tip.into() };
        let said = steps("hiding ", || {
            assert_eq!(commits(&r, &range, 50).unwrap().0.len(), 1)
        });
        // origin/feat and origin/main: side, below what they leave, isn't
        assert_eq!(said, [format!("hiding 2 of {n} tips")], "{tip}");
    }
}

/// The remote tips, as many as a list is told of (origin/HEAD among them).
fn all_remote(r: &Path) -> usize {
    git(
        r,
        &["for-each-ref", "--format=%(objectname)", "refs/remotes"],
    )
    .lines()
    .count()
}

/// A branch with nothing ahead of its upstream, a remote branch, has
/// nothing unpushed: known without a walk. (Here not merged into the
/// default branch, which would say so too.)
#[test]
fn a_branch_level_with_its_upstream_needs_no_walk() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    git(&r, &["push", "-q", "-u", "origin", "feat"]);
    git(&r, &["reset", "-q", "--hard", "HEAD~1"]);
    let unpushed = |r: &Path| {
        let s = status(r, StatusOptions::default()).unwrap();
        s.branches
            .iter()
            .find(|b| b.name == "feat")
            .unwrap()
            .unpushed
    };
    let walks = reach::walks();
    assert_eq!(unpushed(&r), Some(0));
    assert_eq!(reach::walks(), walks);
    // inverse: a commit ahead is looked for on every remote branch
    write(&r, "new", "x\n");
    commit(&r, "new");
    assert_eq!(unpushed(&r), Some(1));
    assert!(reach::walks() > walks);
}

/// Merged into the remote's default branch: on a remote, without a walk.
#[test]
fn a_branch_merged_into_the_remote_default_needs_no_walk() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    // (not at a remote branch's tip, which needs no walk either)
    git(&r, &["branch", "-q", "--no-track", "old", "origin/main~1"]);
    let walks = reach::walks();
    let s = status(&r, StatusOptions::default()).unwrap();
    let old = s.branches.iter().find(|b| b.name == "old").unwrap();
    assert_eq!(old.unpushed, Some(0));
    assert_eq!(reach::walks(), walks);
}

/// Merged into a default branch that's only local says nothing of
/// remotes: the commits are counted.
#[test]
fn a_branch_merged_into_a_local_default_is_counted() {
    let sb = Sandbox::new();
    let r = sb.init("r");
    let bare = sb.path("bare.git");
    git(
        &sb.root(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    git(&r, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&r, &["push", "-q", "origin", "HEAD:refs/heads/other"]);
    git(&r, &["fetch", "-q"]);
    for f in ["g", "h"] {
        write(&r, f, "x\n");
        commit(&r, f);
    }
    git(&r, &["branch", "-q", "--no-track", "feat", "HEAD~1"]);
    let s = status(&r, StatusOptions::default()).unwrap();
    let feat = s.branches.iter().find(|b| b.name == "feat").unwrap();
    assert!(feat.merged);
    assert_eq!(feat.unpushed, Some(1));
}

/// An upstream tracked under `refs/heads` (a fetch refspec can map one
/// there) is a local branch: level with it says nothing of remotes, so
/// the commits are counted.
#[test]
fn an_upstream_that_is_a_local_branch_is_no_remote() {
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    for f in ["g", "h"] {
        write(&r, f, "x\n");
        commit(&r, f);
    }
    let url = git(&r, &["remote", "get-url", "origin"]);
    git(&r, &["remote", "add", "up", url.trim()]);
    git(
        &r,
        &["config", "remote.up.fetch", "+refs/heads/*:refs/heads/up/*"],
    );
    git(&r, &["branch", "-q", "up/main", "main"]);
    git(&r, &["branch", "-q", "feat", "main"]);
    git(&r, &["config", "branch.feat.remote", "up"]);
    git(&r, &["config", "branch.feat.merge", "refs/heads/main"]);
    let s = status(&r, StatusOptions::default()).unwrap();
    let feat = s.branches.iter().find(|b| b.name == "feat").unwrap();
    let up = feat.upstream.as_ref().unwrap();
    assert_eq!((up.name.as_str(), up.ahead), ("up/main", 0));
    assert_eq!(feat.unpushed, Some(2));
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// On random histories (four branches, commits and merges, the graph
    /// written partway or not at all), any tip, others and likely ones
    /// count as git counts.
    #[test]
    fn counts_agree_with_git(
        ops in prop::collection::vec((0u8..4, 0usize..4), 4..14),
        graph_at in 0usize..20,
        others in prop::collection::vec(any::<bool>(), 32),
        likely in prop::collection::vec(any::<bool>(), 32),
        tip in any::<prop::sample::Index>(),
    ) {
        let sb = Sandbox::new();
        let r = sb.init("r");
        for b in 1..4 {
            git(&r, &["branch", "-q", &format!("b{b}")]);
        }
        git(&r, &["branch", "-q", "-m", "b0"]);
        for (k, (op, b)) in ops.iter().enumerate() {
            git(&r, &["switch", "-q", &format!("b{b}")]);
            if *op == 3 {
                let other = format!("b{}", (b + 1) % 4);
                git(&r, &["merge", "-q", "--no-edit", "--no-ff", &other]);
            } else {
                write(&r, &format!("f{k}"), "x\n");
                commit(&r, &format!("c{k}"));
            }
            if k == graph_at {
                write_graph(&r);
            }
        }
        let commits: Vec<String> = git(&r, &["rev-list", "--all"]).lines().map(String::from).collect();
        let pick = |which: &[bool]| -> Vec<String> {
            commits.iter().zip(which.iter().cycle()).filter(|(_, w)| **w).map(|(c, _)| c.clone()).collect()
        };
        let others = pick(&others);
        let likely: Vec<String> = others.iter().zip(likely.iter().cycle()).filter(|(_, w)| **w).map(|(c, _)| c.clone()).collect();
        let tip = tip.get(&commits).clone();
        let mut args = vec!["rev-list", "--count", tip.as_str(), "--not"];
        args.extend(others.iter().map(String::as_str));
        let expected: usize = git(&r, &args).trim().parse().unwrap();
        let ids = |v: &[String]| -> Vec<ObjectId> { v.iter().map(|c| ObjectId::from_hex(c.as_bytes()).unwrap()).collect() };
        argus_core::remember::forget();
        let repo = gix::open(&r).unwrap();
        let tip_id = ObjectId::from_hex(tip.as_bytes()).unwrap();
        let got = exclusive(&repo, tip_id, &ids(&likely), &ids(&others), &mut Remembered::of(&repo)).unwrap();
        prop_assert_eq!(got.n, expected, "{:?}", got);
        // and hiding the tips a list would leaves the same commits
        let mut want = vec!["rev-list", tip.as_str(), "--not"];
        want.extend(others.iter().map(String::as_str));
        let mut want: Vec<String> = git(&r, &want).lines().map(String::from).collect();
        let hidden: Vec<String> = reach::hiding(&repo, tip_id, &ids(&likely), &ids(&others), 1000)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut have = vec!["rev-list", tip.as_str(), "--not"];
        have.extend(hidden.iter().map(String::as_str));
        let mut have: Vec<String> = git(&r, &have).lines().map(String::from).collect();
        want.sort();
        have.sort();
        prop_assert_eq!(have, want);
    }
}

/// How a count was got, as `-v` says it.
#[test]
fn how_a_count_was_got_is_said() {
    let said = [
        (How::IsAnother, "the tip is another's"),
        (How::Remembered, "remembered"),
        (How::Likely, "on the likely remote branches"),
        (How::Pruned { hidden: 3 }, "walked hiding 3 tips"),
        (How::Full, "walked hiding every tip"),
    ];
    for (how, text) in said {
        assert_eq!(how.to_string(), text);
    }
}

/// Unpushed counts are kept in the repo's file too, so the next run (here,
/// with what's in memory forgotten) has them without walking; another
/// set of remote branches is counted afresh.
#[test]
fn unpushed_counts_are_kept_for_the_next_run() {
    use argus_core::remember::{forget, keep_in};
    let sb = Sandbox::new();
    let r = with_old_branches(&sb);
    two_more(&r);
    let dir = sb.path("cache");
    keep_in(&dir);
    let feat = |r: &Path| {
        let s = status(r, StatusOptions::default()).unwrap();
        s.branches
            .iter()
            .find(|b| b.name == "feat")
            .unwrap()
            .unpushed
    };
    assert_eq!(feat(&r), Some(2));
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let text = std::fs::read_to_string(&files[0]).unwrap();
    let tip = id(&r, "feat");
    assert!(
        text.lines()
            .any(|l| l.starts_with(&format!("unpushed {tip} ")) && l.ends_with(" 2")),
        "{text}"
    );
    forget();
    let walks = reach::walks();
    assert_eq!(feat(&r), Some(2));
    assert_eq!(reach::walks(), walks, "read, not walked");
    // inverse: a remote branch moved, so the others aren't the same
    git(&r, &["push", "-q", "origin", "feat~1:refs/heads/side"]);
    git(&r, &["fetch", "-q"]);
    forget();
    assert_eq!(feat(&r), Some(1));
    assert!(reach::walks() > walks);
}
