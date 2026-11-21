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

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
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
            // (its own were said as they ended)
            Some(Form::Each) => {
                let of = span.parent().and_then(|p| {
                    let ext = p.extensions();
                    ext.get::<Open>()
                        .map(|o| o.what.clone())
                        .filter(|w| !w.is_empty())
                });
                let what = match of {
                    Some(of) if step.what == step.kind => format!("{of}: {}", step.what),
                    _ => step.what,
                };
                emit(Step {
                    what,
                    steps: vec![],
                    ..step
                })
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
