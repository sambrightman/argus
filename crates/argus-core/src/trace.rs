//! What argus does, step by step, and how long each step took: for `-v`
//! (on stderr) and the TUI's history. Off until a sink is set; while it's
//! off, a step's text is never built.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// One step: what was done (e.g. `status ~/dev/argus`), and how long it
/// took if it was timed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub what: String,
    pub took: Option<Duration>,
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.took {
            Some(d) => write!(f, "{:>7}  {}", duration(d), self.what),
            None => write!(f, "{:>7}  {}", "", self.what),
        }
    }
}

pub type Sink = Arc<dyn Fn(Step) + Send + Sync>;

static ON: AtomicBool = AtomicBool::new(false);
static SINK: RwLock<Option<Sink>> = RwLock::new(None);

/// Where steps go from now on; `None` turns them off.
pub fn set_sink(sink: Option<Sink>) {
    let on = sink.is_some();
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = sink;
    ON.store(on, Ordering::Relaxed);
}

/// Whether steps are wanted (a sink is set).
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn emit(step: Step) {
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(sink) = sink {
        sink(step);
    }
}

/// A step that isn't timed.
pub fn note(what: impl FnOnce() -> String) {
    if enabled() {
        emit(Step {
            what: what(),
            took: None,
        });
    }
}

/// A step that took `took`.
pub fn took(what: impl FnOnce() -> String, took: Duration) {
    if enabled() {
        emit(Step {
            what: what(),
            took: Some(took),
        });
    }
}

/// Run `f` as a step, described from its result (e.g. how many repos a
/// scan found).
pub fn timed<T>(what: impl FnOnce(&T) -> String, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let started = Instant::now();
    let out = f();
    took(|| what(&out), started.elapsed());
    out
}

/// The parts of one step and how long each took, for its description:
/// e.g. a repo's status, as files, branches and so on.
#[derive(Debug, Default)]
pub struct Parts(Vec<(&'static str, Duration)>);

impl Parts {
    pub fn time<T>(&mut self, part: &'static str, f: impl FnOnce() -> T) -> T {
        if !enabled() {
            return f();
        }
        let started = Instant::now();
        let out = f();
        self.0.push((part, started.elapsed()));
        out
    }

    /// The parts that took a millisecond or more, slowest first, e.g.
    /// ` (files 1.20s, branches 3ms)`; nothing if none did.
    pub fn summary(&self) -> String {
        let mut slow: Vec<&(&str, Duration)> = self
            .0
            .iter()
            .filter(|(_, d)| *d >= Duration::from_millis(1))
            .collect();
        if slow.is_empty() {
            return String::new();
        }
        slow.sort_by_key(|a| std::cmp::Reverse(a.1));
        let parts: Vec<String> = slow
            .iter()
            .map(|(p, d)| format!("{p} {}", duration(*d)))
            .collect();
        format!(" ({})", parts.join(", "))
    }
}

/// `d` in a few characters: `<1ms`, `12ms`, `1.23s`.
pub fn duration(d: Duration) -> String {
    if d < Duration::from_millis(1) {
        "<1ms".into()
    } else if d < Duration::from_secs(1) {
        format!("{}ms", d.as_millis())
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests here share the one sink.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// The steps `f` emits whose text starts with `prefix` (other tests
    /// in this process may emit steps meanwhile).
    fn steps(prefix: &'static str, f: impl FnOnce()) -> Vec<Step> {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let got: Arc<Mutex<Vec<Step>>> = Arc::default();
        let into = got.clone();
        set_sink(Some(Arc::new(move |s: Step| {
            if s.what.starts_with(prefix) {
                into.lock().unwrap().push(s);
            }
        })));
        f();
        set_sink(None);
        got.lock().unwrap().clone()
    }

    #[test]
    fn steps_go_to_the_sink_while_one_is_set() {
        let got = steps("t1 ", || {
            note(|| "t1 noted".into());
            took(|| "t1 took".into(), Duration::from_millis(5));
            let n = timed(|n: &usize| format!("t1 found {n}"), || 3);
            assert_eq!(n, 3);
        });
        let what: Vec<&str> = got.iter().map(|s| s.what.as_str()).collect();
        assert_eq!(what, ["t1 noted", "t1 took", "t1 found 3"]);
        assert_eq!(got[0].took, None);
        assert_eq!(got[1].took, Some(Duration::from_millis(5)));
        assert!(got[2].took.is_some());
    }

    #[test]
    fn without_a_sink_no_step_is_described() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        set_sink(None);
        assert!(!enabled());
        note(|| panic!("described with no sink"));
        took(|| panic!("described with no sink"), Duration::ZERO);
        assert_eq!(timed(|_: &u8| panic!("described with no sink"), || 7), 7);
        let mut parts = Parts::default();
        assert_eq!(parts.time("files", || 1), 1);
        assert_eq!(parts.summary(), "");
    }

    #[test]
    fn parts_are_summed_up_slowest_first_leaving_out_the_quick() {
        let parts = Parts(vec![
            ("refs", Duration::from_micros(300)),
            ("branches", Duration::from_millis(3)),
            ("files", Duration::from_millis(1200)),
        ]);
        assert_eq!(parts.summary(), " (files 1.20s, branches 3ms)");
        assert_eq!(
            Parts(vec![("refs", Duration::from_micros(999))]).summary(),
            ""
        );
        // and timed only while steps are wanted
        steps("t3 ", || {
            let mut p = Parts::default();
            p.time("files", || std::thread::sleep(Duration::from_millis(2)));
            assert_eq!(p.0.len(), 1);
            assert!(p.summary().starts_with(" (files "), "{}", p.summary());
        });
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(Duration::from_micros(999)), "<1ms");
        assert_eq!(duration(Duration::from_millis(1)), "1ms");
        assert_eq!(duration(Duration::from_millis(999)), "999ms");
        assert_eq!(duration(Duration::from_millis(1000)), "1.00s");
        assert_eq!(duration(Duration::from_millis(31_456)), "31.46s");
        let s = Step {
            what: "status ~/x".into(),
            took: Some(Duration::from_millis(12)),
        };
        assert_eq!(s.to_string(), "   12ms  status ~/x");
        let s = Step {
            what: "roots ~/dev".into(),
            took: None,
        };
        assert_eq!(s.to_string(), "         roots ~/dev");
    }
}
