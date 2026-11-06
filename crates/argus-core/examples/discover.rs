//! Time discovery alone (no status), for performance work:
//!
//! ```sh
//! cargo run --release -p argus-core --example discover -- [-L] [--skip PATTERN]… [-n RUNS] ROOT…
//! ```
//!
//! `--no-real` and `--no-prune` turn off the `-L` optimizations, to compare;
//! `--list` prints what was found (repos, then truncated directories).
//!
//! Prints the repos found and the median and fastest of the runs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use argus_core::discover::{DiscoverOptions, Skip, Tuning, discover_all};

fn main() {
    let mut roots = vec![];
    let mut skip = vec![];
    let mut follow = false;
    let mut runs = 10;
    let mut tuning = Tuning::default();
    let mut list = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-L" => follow = true,
            "--no-real" => tuning.real_from_links = false,
            "--no-prune" => tuning.prune_covered_links = false,
            "--list" => list = true,
            "--skip" => skip.push(args.next().expect("--skip PATTERN")),
            "-n" => runs = args.next().and_then(|n| n.parse().ok()).expect("-n RUNS"),
            _ => roots.push(PathBuf::from(a)),
        }
    }
    let opts = DiscoverOptions {
        skip: Skip::new(&skip, &roots).expect("skip patterns"),
        follow_symlinks: follow,
        max_depth: 0,
        tuning,
        ..Default::default()
    };
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(discover_all(&roots, &opts));
            t.elapsed()
        })
        .collect();
    times.sort();
    let d = discover_all(&roots, &opts);
    if list {
        for f in &d.found {
            println!("{:?} {}", f.kind, f.path.display());
        }
        for t in &d.truncated {
            println!("truncated {}", t.display());
        }
    }
    println!(
        "{} repos; median {:.1?}, fastest {:.1?} ({runs} runs)",
        d.found.len(),
        times[times.len() / 2],
        times[0]
    );
}
