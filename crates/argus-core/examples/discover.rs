//! Time discovery alone (no status), for performance work:
//!
//! ```sh
//! cargo run --release -p argus-core --example discover -- [-L] [-x] [--skip PATTERN]… [-n RUNS] [-j THREADS] ROOT…
//! ```
//!
//! Runs on as many threads as argus would (`[concurrency] disk`'s default),
//! unless `-j` says otherwise.
//!
//! `-x` stays on each root's filesystem. `--no-real` and `--no-prune` turn off the `-L` optimizations, to compare;
//! `--list` prints what was found (repos, then truncated directories).
//!
//! Prints the repos found and the median and fastest of the runs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use argus_core::config::Config;
use argus_core::discover::{DiscoverOptions, Skip, Tuning, discover_all};
use argus_core::workspace::in_pool;

fn main() {
    let mut roots = vec![];
    let mut skip = vec![];
    let mut follow = false;
    let mut one_fs = false;
    let mut runs = 10;
    let mut tuning = Tuning::default();
    let mut list = false;
    let mut threads = Config::default().disk_jobs();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-L" => follow = true,
            "-x" => one_fs = true,
            "--no-real" => tuning.real_from_links = false,
            "--no-prune" => tuning.prune_covered_links = false,
            "--list" => list = true,
            "--skip" => skip.push(args.next().expect("--skip PATTERN")),
            "-n" => runs = args.next().and_then(|n| n.parse().ok()).expect("-n RUNS"),
            "-j" => {
                threads = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .expect("-j THREADS")
            }
            _ => roots.push(PathBuf::from(a)),
        }
    }
    let opts = DiscoverOptions {
        skip: Skip::new(&skip, &roots).expect("skip patterns"),
        follow_symlinks: follow,
        one_file_system: one_fs,
        max_depth: 0,
        tuning,
        ..Default::default()
    };
    let mut times: Vec<Duration> = vec![];
    let mut d = None;
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        d = Some(in_pool(threads, || discover_all(&roots, &opts)));
        times.push(t.elapsed());
    }
    times.sort();
    let d = d.expect("at least one run");
    if list {
        for f in &d.found {
            println!("{:?} {}", f.kind, f.path.display());
        }
        for t in &d.truncated {
            println!("truncated {}", t.display());
        }
    }
    println!(
        "{} repos; median {:.1?}, fastest {:.1?} ({runs} runs, {threads} threads)",
        d.found.len(),
        times[times.len() / 2],
        times[0]
    );
}
