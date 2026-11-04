//! Bounded-parallel execution of jobs, streaming progress events.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ops::{self, OpOutcome};
use crate::sync::Job;

#[derive(Debug, Clone)]
pub enum Event {
    Started { index: usize },
    Finished { index: usize, outcome: OpOutcome },
}

/// Run `jobs` with at most `concurrency` git processes at once. Jobs of the
/// same family run sequentially, in order. `cancel` stops scheduling new jobs
/// (running ones complete). Returns outcomes indexed like `jobs`; `None` for
/// jobs that were cancelled before starting.
pub fn run_jobs(
    jobs: &[Job],
    concurrency: usize,
    cancel: &AtomicBool,
    on_event: &(dyn Fn(Event) + Sync),
) -> Vec<Option<OpOutcome>> {
    let mut families: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (i, j) in jobs.iter().enumerate() {
        families.entry(j.family.clone()).or_default().push(i);
    }
    let queue: Mutex<VecDeque<Vec<usize>>> = Mutex::new(families.into_values().collect());
    let results: Mutex<Vec<Option<OpOutcome>>> = Mutex::new(vec![None; jobs.len()]);
    let workers = concurrency.max(1).min(jobs.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let Some(family) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    for index in family {
                        if cancel.load(Ordering::Relaxed) {
                            return;
                        }
                        on_event(Event::Started { index });
                        let outcome = ops::run(&jobs[index].path, &jobs[index].op);
                        let ok = outcome.success;
                        results.lock().unwrap()[index] = Some(outcome.clone());
                        on_event(Event::Finished { index, outcome });
                        if !ok {
                            break; // don't pile more ops onto a repo in trouble
                        }
                    }
                }
            });
        }
    });
    results.into_inner().unwrap()
}
