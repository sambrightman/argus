//! What argus does, step by step, and how long each step took: for `-v`
//! (on stderr) and the TUI's messages. A step can hold steps of its own
//! (a scan, each repo's status in it, each part of that), across threads,
//! as `tracing` spans; a layer of ours turns them into [`Step`]s for the
//! sink. Off until a sink is set; while it's off, no step is made and no
//! text built.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, RwLock};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// One step: what kind it is (`status`, `files`, `git`…), what was done
/// (`status ~/dev/argus`), how long it took if it was timed, and the
/// steps it took, in the order they started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: String,
    pub what: String,
    pub took: Option<Duration>,
    pub steps: Vec<Step>,
    started: Instant,
}

impl Step {
    pub fn new(kind: &str, what: impl Into<String>, took: Option<Duration>) -> Self {
        Step {
            kind: kind.into(),
            what: what.into(),
            took,
            steps: vec![],
            started: Instant::now() - took.unwrap_or_default(),
        }
    }
}

/// How a step's time divides: its own (none of its steps running), its
/// steps' (some running), and their times added up, which is more than
/// theirs where they ran side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    pub own: Duration,
    pub sub_steps: Duration,
    pub total: Duration,
}

impl Step {
    /// When it started.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// It, started at `at` (as a test lays steps out).
    pub fn starting(self, at: Instant) -> Self {
        Step {
            started: at,
            ..self
        }
    }

    /// How its time divides, if it was timed and took timed steps.
    pub fn split(&self) -> Option<Split> {
        let took = self.took?;
        let end = self.started + took;
        let timed: Vec<(Instant, Duration)> = self
            .steps
            .iter()
            .filter_map(|s| s.took.map(|t| (s.started, t)))
            .collect();
        if timed.is_empty() {
            return None;
        }
        let total = timed.iter().map(|(_, t)| *t).sum();
        // the time any ran, within this one's: the union of their spans
        let mut spans: Vec<(Instant, Instant)> = timed
            .iter()
            .map(|&(at, t)| (at.max(self.started), (at + t).min(end)))
            .collect();
        spans.sort();
        let mut sub_steps = Duration::ZERO;
        let mut reached = self.started;
        for (from, to) in spans {
            sub_steps += to.saturating_duration_since(from.max(reached));
            reached = reached.max(to);
        }
        Some(Split {
            own: took.saturating_sub(sub_steps),
            sub_steps,
            total,
        })
    }

    /// Its time and, if it took steps, how that divides: `30ms  scan: 2
    /// repos (own 5ms, sub-steps 25ms overlapping, 45ms summed)`: its own, when
    /// none of its steps ran; theirs, overlapping ones counted once; and
    /// theirs summed, more than that where they ran side by side.
    pub fn with_split(&self) -> String {
        self.said_in(None)
    }

    /// [`Step::with_split`], a part named with the step it's in, `of`:
    /// `12ms  status ~/dev/argus: files`, as it's said away from it.
    pub fn said_in(&self, of: Option<&str>) -> String {
        let time = match self.took {
            Some(d) => duration(d),
            None => String::new(),
        };
        let said = format!("{time:>7}  {}", self.named_in(of));
        match self.split() {
            Some(s) => format!(
                "{said} (own {}, sub-steps {} overlapping, {} summed)",
                duration(s.own),
                duration(s.sub_steps),
                duration(s.total)
            ),
            None => said,
        }
    }

    /// [`Step::said_in`] with its own time first, as the views sorted by
    /// it show it: `2ms  scan (total 10ms, sub-steps 8ms overlapping, 10ms
    /// summed)`. One that took no timed steps is all its own: said as by
    /// `said_in`.
    pub fn own_first_in(&self, of: Option<&str>) -> String {
        let Some(s) = self.split() else {
            return self.said_in(of);
        };
        format!(
            "{:>7}  {} (total {}, sub-steps {} overlapping, {} summed)",
            duration(s.own),
            self.named_in(of),
            self.took.map(duration).unwrap_or_default(),
            duration(s.sub_steps),
            duration(s.total)
        )
    }

    /// What it did, a part (said by its kind, alone or followed by `: `
    /// and more) named with the step it's in, `of`: `status ~/dev/argus:
    /// files`, `branch x in ~/dev/argus: unpushed: 0, remembered`.
    pub fn named_in(&self, of: Option<&str>) -> String {
        let part = match self.what.strip_prefix(self.kind.as_str()) {
            Some(rest) => rest.is_empty() || rest.starts_with(": "),
            None => false,
        };
        match of.filter(|of| !of.is_empty()) {
            Some(of) if part => format!("{of}: {}", self.what),
            _ => self.what.clone(),
        }
    }
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.took {
            Some(d) => write!(f, "{:>7}  {}", duration(d), self.what),
            None => write!(f, "{:>7}  {}", "", self.what),
        }
    }
}

/// How steps reach the sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Each step by itself as it ends (a step's own before it): `-v`.
    Each,
    /// Each outermost step as it ends, with all it took in it.
    Whole,
}

pub type Sink = Arc<dyn Fn(Step) + Send + Sync>;

static ON: AtomicBool = AtomicBool::new(false);
static SINK: RwLock<Option<(Form, Sink)>> = RwLock::new(None);

/// Where steps go from now on, and in what form; `None` turns them off.
pub fn set_sink(sink: Option<(Form, Sink)>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::registry().with(Steps);
        // (another set first, as by a test harness: steps aren't made)
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
    let on = sink.is_some();
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = sink;
    ON.store(on, Ordering::Relaxed);
}

/// Whether steps are wanted (a sink is set).
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn form() -> Option<Form> {
    SINK.read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.0)
}

fn emit(step: Step) {
    let sink = SINK.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some((_, sink)) = sink {
        sink(step);
    }
}

fn open(kind: &'static str) -> tracing::Span {
    tracing::info_span!(target: "argus", "step", kind, what = tracing::field::Empty)
}

/// Run `f` as a step of `kind`, said as `what`, holding the steps `f`
/// takes.
pub fn step<T>(kind: &'static str, what: impl FnOnce() -> String, f: impl FnOnce() -> T) -> T {
    let span = open(kind);
    if !span.is_disabled() {
        span.record("what", what().as_str());
    }
    span.in_scope(f)
}

/// A step begun, ending when this is dropped (or [`Begun::end`]): for one
/// that spans more than a call, such as the TUI's startup until its first
/// scan is in. Steps taken on this thread meanwhile are its, and on others
/// through [`within`].
pub struct Begun(#[allow(dead_code)] tracing::span::EnteredSpan);

/// Begin a step of `kind`, said as `what`.
pub fn begin(kind: &'static str, what: impl FnOnce() -> String) -> Begun {
    let span = open(kind);
    if !span.is_disabled() {
        span.record("what", what().as_str());
    }
    Begun(span.entered())
}

impl Begun {
    /// End it now.
    pub fn end(self) {}
}

/// Run `f` as a step of `kind`, said from its result (e.g. how many repos
/// a scan found).
pub fn timed<T>(kind: &'static str, what: impl FnOnce(&T) -> String, f: impl FnOnce() -> T) -> T {
    let span = open(kind);
    let out = span.in_scope(f);
    if !span.is_disabled() {
        span.record("what", what(&out).as_str());
    }
    out
}

/// Say the step the caller is in as `what` instead (it failed, say).
pub fn amend(what: impl FnOnce() -> String) {
    let span = tracing::Span::current();
    if !span.is_disabled() {
        span.record("what", what().as_str());
    }
}

/// A part of the step it's in, said by its kind (`files`, `branches`); by
/// itself, with that step: `status ~/dev/argus: files`.
pub fn part<T>(kind: &'static str, f: impl FnOnce() -> T) -> T {
    step(kind, || kind.to_string(), f)
}

/// A step that isn't timed: something said along the way.
pub fn note(what: impl FnOnce() -> String) {
    if enabled() {
        tracing::info!(target: "argus", kind = "note", what = what().as_str());
    }
}

/// A step of `kind` that took `took`, timed already (a command run).
pub fn took(kind: &'static str, what: impl FnOnce() -> String, took: Duration) {
    if enabled() {
        let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        tracing::info!(target: "argus", kind, what = what().as_str(), took = nanos);
    }
}

/// Longer than a frame: what's done for each, said only when it took this.
pub const SLOW: Duration = Duration::from_millis(16);

/// A step of `kind` that's taken often (a frame), said only when slow.
pub fn if_slow<T>(kind: &'static str, what: impl FnOnce() -> String, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let started = Instant::now();
    let out = f();
    let d = started.elapsed();
    if d >= SLOW {
        took(kind, what, d);
    }
    out
}

/// The step the caller is in, to carry to another thread: steps taken
/// there with [`Within::run`] are its.
#[derive(Clone)]
pub struct Within(tracing::Span);

pub fn within() -> Within {
    Within(tracing::Span::current())
}

impl Within {
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        self.0.in_scope(f)
    }
}

/// A step being taken: its fields as they arrive, and the steps it took.
struct Open {
    kind: String,
    what: String,
    started: Instant,
    steps: Vec<Step>,
}

/// `kind`, `what` and `took` of a step or event.
#[derive(Default)]
struct Fields {
    kind: Option<String>,
    what: Option<String>,
    took: Option<Duration>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "kind" => self.kind = Some(value.into()),
            "what" => self.what = Some(value.into()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "took" {
            self.took = Some(Duration::from_nanos(value));
        }
    }

    // (ours are strs and a u64: nothing comes this way)
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
}

/// The layer turning argus's spans and events into [`Step`]s.
struct Steps;

impl<S> Layer<S> for Steps
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        match meta.target() {
            // (asked each time: steps go on and off with the sink)
            "argus" => tracing::subscriber::Interest::sometimes(),
            _ => tracing::subscriber::Interest::never(),
        }
    }

    fn enabled(&self, meta: &Metadata<'_>, _: Context<'_, S>) -> bool {
        meta.target() == "argus" && enabled()
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Open {
                kind: fields.kind.unwrap_or_default(),
                what: fields.what.unwrap_or_default(),
                started: Instant::now(),
                steps: vec![],
            });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        values.record(&mut fields);
        if let (Some(what), Some(span)) = (fields.what, ctx.span(id))
            && let Some(open) = span.extensions_mut().get_mut::<Open>()
        {
            open.what = what;
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let kind = fields.kind.unwrap_or_default();
        let step = Step::new(&kind, fields.what.unwrap_or_default(), fields.took);
        let held = form() == Some(Form::Whole);
        match ctx.event_span(event).filter(|_| held) {
            Some(span) => {
                if let Some(open) = span.extensions_mut().get_mut::<Open>() {
                    open.steps.push(step);
                }
            }
            None => emit(step),
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let Some(open) = span.extensions_mut().remove::<Open>() else {
            return;
        };
        let mut steps = open.steps;
        steps.sort_by_key(|s| s.started);
        let step = Step {
            kind: open.kind,
            what: open.what,
            took: Some(open.started.elapsed()),
            steps,
            started: open.started,
        };
        match form() {
            Some(Form::Whole) => match span.parent() {
                Some(parent) => {
                    if let Some(open) = parent.extensions_mut().get_mut::<Open>() {
                        open.steps.push(step);
                    }
                }
                None => emit(step),
            },
            // (its own were said as they ended, none held in it)
            Some(Form::Each) => {
                let of = span.parent().and_then(|p| {
                    let ext = p.extensions();
                    ext.get::<Open>().map(|o| o.what.clone())
                });
                let what = step.named_in(of.as_deref());
                emit(Step { what, ..step })
            }
            None => {}
        }
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

    /// The steps `f` takes, as `form` gives them, of those whose text starts
    /// with `prefix` (other tests in this process may take steps meanwhile).
    fn steps(form: Form, prefix: &'static str, f: impl FnOnce()) -> Vec<Step> {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let got: Arc<Mutex<Vec<Step>>> = Arc::default();
        let into = got.clone();
        let sink: Sink = Arc::new(move |s: Step| {
            if s.what.starts_with(prefix) {
                into.lock().unwrap().push(s);
            }
        });
        set_sink(Some((form, sink)));
        f();
        set_sink(None);
        got.lock().unwrap().clone()
    }

    fn whats(steps: &[Step]) -> Vec<&str> {
        steps.iter().map(|s| s.what.as_str()).collect()
    }

    /// A step's steps are in it, across threads too, in the order they
    /// started; told whole, the outermost goes to the sink.
    #[test]
    fn steps_hold_their_steps_whole() {
        let got = steps(Form::Whole, "t1 ", || {
            step(
                "scan",
                || "t1 scan".into(),
                || {
                    let n = timed("discover", |n: &usize| format!("found {n}"), || 3);
                    assert_eq!(n, 3);
                    let here = within();
                    std::thread::scope(|s| {
                        s.spawn(|| here.run(|| part("files", || ())));
                    });
                    note(|| "noted".into());
                    took("git", || "git status".into(), Duration::from_millis(5));
                },
            );
        });
        assert_eq!(whats(&got), ["t1 scan"]);
        let scan = &got[0];
        assert_eq!(scan.kind, "scan");
        // (the command's started its 5ms before it was told)
        assert_eq!(
            whats(&scan.steps),
            ["git status", "found 3", "files", "noted"]
        );
        let git = scan.steps.iter().find(|s| s.kind == "git").unwrap();
        assert_eq!(git.took, Some(Duration::from_millis(5)));
        assert_eq!(
            scan.steps.iter().find(|s| s.kind == "note").unwrap().took,
            None
        );
    }

    /// Told each by itself, a step's own come first, none held in it.
    #[test]
    fn steps_come_each_as_they_end() {
        let got = steps(Form::Each, "t2", || {
            step(
                "scan",
                || "t2 scan".into(),
                || {
                    step(
                        "status",
                        || "t2 status".into(),
                        || {
                            part("files", || ());
                            amend(|| "t2 status, failed".into());
                        },
                    );
                    note(|| "t2 noted".into());
                },
            );
        });
        // a part said with the step it's in
        assert_eq!(
            whats(&got),
            [
                "t2 status: files",
                "t2 status, failed",
                "t2 noted",
                "t2 scan"
            ]
        );
        assert!(got.iter().all(|s| s.steps.is_empty()));
    }

    /// A step taken often is said only when it took a frame or more.
    #[test]
    fn often_steps_are_said_when_slow() {
        let got = steps(Form::Each, "t3", || {
            let n = if_slow("frame", || "t3 quick".into(), || 1);
            if_slow(
                "frame",
                || "t3 slow".into(),
                || {
                    std::thread::sleep(SLOW);
                },
            );
            assert_eq!(n, 1);
        });
        assert_eq!(whats(&got), ["t3 slow"]);
        assert!(got[0].took.is_some_and(|t| t >= SLOW));
    }

    #[test]
    fn without_a_sink_no_step_is_described() {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        set_sink(None);
        assert!(!enabled());
        note(|| panic!("described with no sink"));
        took("git", || panic!("described with no sink"), Duration::ZERO);
        assert_eq!(
            timed("x", |_: &u8| panic!("described with no sink"), || 7),
            7
        );
        assert_eq!(step("x", || panic!("described with no sink"), || 8), 8);
        assert_eq!(if_slow("x", || panic!("described with no sink"), || 9), 9);
    }

    /// A step's own time is when none of its steps ran; theirs, when any
    /// did, however they overlapped, within its own span; and their total.
    #[test]
    fn a_steps_time_divides_into_its_own_and_its_steps() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let at = |n: u64| t0 + ms(n);
        let step = |took: u64, from: u64| Step::new("x", "x", Some(ms(took))).starting(at(from));
        let with = |took: u64, steps: Vec<Step>| Step {
            steps,
            ..step(took, 0)
        };
        let split = |own: u64, sub: u64, total: u64| Split {
            own: ms(own),
            sub_steps: ms(sub),
            total: ms(total),
        };
        // one after another: 1 of its own, 4 of 2 each
        let after = with(9, (0..4).map(|n| step(2, 1 + 2 * n)).collect());
        assert_eq!(after.split(), Some(split(1, 8, 8)));
        // all at once
        let at_once = with(3, (0..4).map(|_| step(2, 1)).collect());
        assert_eq!(at_once.split(), Some(split(1, 2, 8)));
        // two pairs, the second starting halfway through the first
        let pairs = with(4, vec![step(2, 1), step(2, 1), step(2, 2), step(2, 2)]);
        assert_eq!(pairs.split(), Some(split(1, 3, 8)));
        // apart, with a gap: covered is each, not from first to last
        let apart = with(10, vec![step(2, 1), step(3, 6)]);
        assert_eq!(apart.split(), Some(split(5, 5, 5)));
        // one inside another, and another inside it after
        let inside = with(10, vec![step(8, 1), step(2, 3)]);
        assert_eq!(inside.split(), Some(split(2, 8, 10)));
        let then = with(12, vec![step(8, 1), step(2, 3), step(2, 6)]);
        assert_eq!(then.split(), Some(split(4, 8, 12)));
        // one wholly past its end
        let outside = with(5, vec![step(2, 1), step(2, 7)]);
        assert_eq!(outside.split(), Some(split(3, 2, 4)));
        // (said before it started, or past its end: only its part counts)
        let past = with(5, vec![step(4, 3)]);
        assert_eq!(past.split(), Some(split(3, 2, 4)));
        // nothing timed in it: no split, said by itself
        let noted = with(5, vec![Step::new("note", "roots", None)]);
        assert_eq!(noted.split(), None);
        assert_eq!(step(5, 0).split(), None);
        assert_eq!(noted.with_split(), "    5ms  x");
        // a part, named with what it's in; not a step said in full
        let part = Step::new("files", "files", Some(ms(3)));
        assert_eq!(
            part.said_in(Some("status ~/a")),
            "    3ms  status ~/a: files"
        );
        assert_eq!(part.said_in(None), "    3ms  files");
        assert_eq!(part.said_in(Some("")), "    3ms  files");
        // said with more after its kind, still a part
        let counted = Step::new("unpushed", "unpushed: 2, remembered", Some(ms(3)));
        assert_eq!(
            counted.said_in(Some("branch x")),
            "    3ms  branch x: unpushed: 2, remembered"
        );
        // inverse: its kind only begins a word of it, or has no colon after
        let begun = Step::new("files", "filesystem ~/a", Some(ms(3)));
        assert_eq!(begun.said_in(Some("status ~/a")), "    3ms  filesystem ~/a");
        let spaced = Step::new("files", "files 3", Some(ms(3)));
        assert_eq!(spaced.said_in(Some("status ~/a")), "    3ms  files 3");
        let status = Step::new("status", "status ~/b", Some(ms(5)));
        assert_eq!(status.said_in(Some("scan")), "    5ms  status ~/b");
        // own time first (sorted by it): all it took with the rest; one
        // that took no steps is all its own, said as before
        let scan = with(10, vec![step(8, 1), step(2, 3)]);
        assert_eq!(
            scan.said_in(None),
            "   10ms  x (own 2ms, sub-steps 8ms overlapping, 10ms summed)"
        );
        assert_eq!(
            scan.own_first_in(None),
            "    2ms  x (total 10ms, sub-steps 8ms overlapping, 10ms summed)"
        );
        assert_eq!(
            part.own_first_in(Some("status ~/a")),
            part.said_in(Some("status ~/a"))
        );
        assert_eq!(noted.own_first_in(None), noted.said_in(None));
        assert_eq!(Step::new("note", "n", None).said_in(None), "         n");
        assert_eq!(
            pairs.with_split(),
            "    4ms  x (own 1ms, sub-steps 3ms overlapping, 8ms summed)"
        );
        // untimed: nothing
        assert_eq!(Step::new("note", "n", None).split(), None);
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(Duration::from_micros(999)), "<1ms");
        assert_eq!(duration(Duration::from_millis(1)), "1ms");
        assert_eq!(duration(Duration::from_millis(999)), "999ms");
        assert_eq!(duration(Duration::from_millis(1000)), "1.00s");
        assert_eq!(duration(Duration::from_millis(31_456)), "31.46s");
        let s = Step::new("status", "status ~/x", Some(Duration::from_millis(12)));
        assert_eq!(s.to_string(), "   12ms  status ~/x");
        assert_eq!(
            Step::new("note", "roots ~/dev", None).to_string(),
            "         roots ~/dev"
        );
    }
}
