//! Temporary: where gix's status time goes in one repo.
use std::time::{Duration, Instant};
fn run(
    repo: &gix::Repository,
    label: &str,
    untracked: Option<gix::status::UntrackedFiles>,
    with_tree: bool,
) {
    let mut best = Duration::MAX;
    let mut n = 0;
    for _ in 0..7 {
        let t = Instant::now();
        let mut p = repo.status(gix::progress::Discard).unwrap();
        p = match untracked {
            Some(u) => p.untracked_files(u),
            None => p.untracked_files(gix::status::UntrackedFiles::None),
        };
        p = p.index_worktree_submodules(gix::status::Submodule::Given {
            ignore: gix::submodule::config::Ignore::All,
            check_dirty: false,
        });
        n = if with_tree {
            p.into_iter(None).unwrap().count()
        } else {
            p.into_index_worktree_iter(Vec::new()).unwrap().count()
        };
        best = best.min(t.elapsed());
    }
    println!("{label:42} {best:?} ({n} items)");
}
fn main() {
    let repo = gix::open(std::env::args().nth(1).unwrap()).unwrap();
    run(&repo, "index vs worktree only (no untracked)", None, false);
    run(
        &repo,
        "+ untracked (Files)",
        Some(gix::status::UntrackedFiles::Files),
        false,
    );
    run(
        &repo,
        "+ untracked (Collapsed)",
        Some(gix::status::UntrackedFiles::Collapsed),
        false,
    );
    run(&repo, "+ HEAD vs index (no untracked)", None, true);
    run(
        &repo,
        "all, as argus (Files + tree)",
        Some(gix::status::UntrackedFiles::Files),
        true,
    );
    let t = Instant::now();
    let _ = repo.index_or_empty().unwrap();
    println!("{:42} {:?}", "read index", t.elapsed());
}
