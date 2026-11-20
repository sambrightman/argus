//! Steps for `-v`: what argus's own code reports. The sink is the
//! process's one; only this test sets it in this binary, and it keeps the
//! steps of its own repo.

use std::sync::{Arc, Mutex};

use argus_core::ops::{self, Op};
use argus_core::testkit::{Sandbox, write};
use argus_core::trace::{self, Step};

/// Each git command an operation runs is a step, with its time, saying
/// where it ran and whether it failed.
#[test]
fn git_commands_are_steps() {
    let sb = Sandbox::new();
    let repo = sb.init("steps-repo");
    write(&repo, "new", "x\n");
    let got: Arc<Mutex<Vec<Step>>> = Arc::default();
    let into = got.clone();
    trace::set_sink(Some(Arc::new(move |s: Step| {
        if s.what.contains("steps-repo") {
            into.lock().unwrap().push(s);
        }
    })));
    let added = ops::run(
        &repo,
        &Op::Add {
            paths: vec!["new".into()],
        },
    );
    let missing = ops::run(
        &repo,
        &Op::Add {
            paths: vec!["missing".into()],
        },
    );
    trace::set_sink(None);
    assert!(added.success && !missing.success);
    let got = got.lock().unwrap().clone();
    let git: Vec<&Step> = got
        .iter()
        .filter(|s| s.what.starts_with("git add"))
        .collect();
    assert_eq!(git.len(), 2, "{got:?}");
    assert!(git.iter().all(|s| s.took.is_some()));
    assert!(
        git[0].what.contains("new in ") && git[0].what.ends_with("steps-repo"),
        "{git:?}"
    );
    assert!(git[1].what.ends_with("steps-repo, failed"), "{git:?}");
}
