//! Print gix's raw status items for a repo (debugging aid).
fn main() {
    let p = std::env::args().nth(1).expect("path");
    let repo = gix::open(&p).unwrap();
    let iter = repo
        .status(gix::progress::Discard)
        .unwrap()
        .untracked_files(gix::status::UntrackedFiles::Files)
        .into_iter(None)
        .unwrap();
    for item in iter {
        match item.unwrap() {
            gix::status::Item::IndexWorktree(
                gix::status::index_worktree::Item::DirectoryContents { entry, .. },
            ) => {
                println!(
                    "dir-walk {:?} status={:?} kind={:?} prop={:?}",
                    entry.rela_path, entry.status, entry.disk_kind, entry.property
                )
            }
            other => println!("{:?}", other.location()),
        }
    }
}
