//! Discovery against a real second filesystem (macOS: a disk image an
//! ordinary user can attach).

#[cfg(target_os = "macos")]
#[test]
fn one_file_system_stays_on_each_roots_filesystem() {
    use argus_core::discover::{DiscoverOptions, discover};
    use argus_core::testkit::{Mounted, Sandbox};
    let sb = Sandbox::new();
    sb.init("dev/here");
    let mounted = Mounted::new(&sb, "dev/mnt");
    sb.init("dev/mnt/elsewhere");
    sb.init("outside/linked");
    std::os::unix::fs::symlink(mounted.0.join("elsewhere"), sb.path("dev/onto-mount")).unwrap();
    std::os::unix::fs::symlink(sb.path("outside"), sb.path("dev/same-fs")).unwrap();
    let names = |roots: &[std::path::PathBuf], follow: bool, one: bool| {
        let opts = DiscoverOptions {
            follow_symlinks: follow,
            one_file_system: one,
            ..Default::default()
        };
        let mut v: Vec<String> = discover(roots, &opts)
            .into_iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    let dev = [sb.path("dev")];
    assert_eq!(
        names(&dev, false, false),
        ["elsewhere", "here"],
        "inverse: crosses"
    );
    assert_eq!(names(&dev, false, true), ["here"]);
    assert_eq!(names(&dev, true, false), ["elsewhere", "here", "linked"]);
    assert_eq!(
        names(&dev, true, true),
        ["here", "linked"],
        "a link onto it isn't followed"
    );
    assert_eq!(
        names(std::slice::from_ref(&mounted.0), false, true),
        ["elsewhere"],
        "a root on it"
    );
    let both = [sb.path("dev"), mounted.0.clone()];
    assert_eq!(
        names(&both, false, true),
        ["elsewhere", "here"],
        "each root its own"
    );
}
