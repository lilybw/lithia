//! Sites and tracing diagnostics (spec §4.3, §12).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lithia::{DerivedNode, Site, SourceNode, Status, effect};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// One captured event: its level and every field rendered as `name=value`.
#[derive(Clone, Debug)]
struct Captured {
    level: Level,
    text: String,
}

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(self.0, "{}={} ", field.name(), value);
    }
}

struct Collector(Arc<Mutex<Vec<Captured>>>);

impl<S: Subscriber> Layer<S> for Collector {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields: Fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(Captured { level: *event.metadata().level(), text: fields.0 });
    }
}

fn events() -> Arc<Mutex<Vec<Captured>>> {
    static EVENTS: OnceLock<Arc<Mutex<Vec<Captured>>>> = OnceLock::new();
    EVENTS
        .get_or_init(|| {
            let events: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(Collector(events.clone()));
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            events
        })
        .clone()
}

/// Waits up to `limit` for an event at `level` whose text contains every needle.
async fn find(level: Level, needles: &[&str], limit: Duration) -> Option<Captured> {
    let events: Arc<Mutex<Vec<Captured>>> = events();
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + limit;
    loop {
        let found: Option<Captured> = events
            .lock()
            .unwrap()
            .iter()
            .find(|e: &&Captured| e.level == level && needles.iter().all(|n: &&str| e.text.contains(n)))
            .cloned();
        if found.is_some() || tokio::time::Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn site_of(status: Status) -> Option<Site> {
    match status {
        Status::Failed(failure) => Some(failure.site),
        _ => None,
    }
}

#[lithia::test]
async fn a_site_records_where_the_node_was_created() {
    let n: SourceNode<u32> = effect!(1);
    let line: u32 = line!() + 1;
    let failing: DerivedNode<u32> = effect!(|n| {
        if *n.get() > 0 {
            panic!("site probe")
        }
        0
    });
    lithia::flush().await;
    let site: Site = site_of(lithia::status(&failing)).expect("the body panicked");
    assert!(site.file.ends_with("diagnostics.rs"), "{}", site.file);
    assert_eq!(site.line, line);
    assert!(site.module.ends_with("diagnostics"), "{}", site.module);
    assert!((site.function)().contains("a_site_records_where_the_node_was_created"), "{}", (site.function)());
    assert!(site.tokens.contains("site probe"), "{}", site.tokens);
}

#[lithia::test]
async fn a_panicking_run_warns_with_its_site_and_message() {
    let _ = events();
    let n: SourceNode<u32> = effect!(0);
    let _boom: DerivedNode<u32> = effect!(|n| if *n.get() == 1 { panic!("warn-probe-7781") } else { 0 });
    lithia::flush().await;
    n.set(1);
    lithia::flush().await;
    let event: Option<Captured> =
        find(Level::WARN, &["run panicked", "warn-probe-7781", "diagnostics.rs"], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn a_node_freed_before_its_first_run_warns() {
    let _ = events();
    let marker_7782: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let _ = effect!(|marker_7782| *marker_7782.get());
    lithia::flush().await;
    let event: Option<Captured> =
        find(Level::WARN, &["freed before its first run", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn cuts_warn_for_value_holders_and_trace_for_effects() {
    let _ = events();
    let x: SourceNode<i32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let clamp: DerivedNode<bool> = effect!(|x| {
        let over: bool = *x.get() > 10;
        if over {
            x.set(10);
        }
        over
    });
    x.set(15);
    lithia::flush().await;
    assert_eq!(*x.get(), 10);
    let event: Option<Captured> =
        find(Level::WARN, &["cut a writer that holds a value", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
    drop(clamp);

    let y: SourceNode<i32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let _effect: DerivedNode<()> = effect!(|y| if *y.get() > 10 {
        y.set(10);
    });
    y.set(15);
    lithia::flush().await;
    let event: Option<Captured> = find(Level::TRACE, &["cut an effect", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn an_unattributed_loop_triggers_the_rerun_rate_warning() {
    let _ = events();
    let n: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let holder: DerivedNode<Option<DerivedNode<()>>> = effect!(|n| Some(effect!(|n| n.update(|v: &u32| v + 1))));
    let _ = tokio::time::timeout(Duration::from_millis(100), lithia::flush()).await;
    let event: Option<Captured> =
        find(Level::WARN, &["reran faster than the configured rate", &line], Duration::from_secs(5)).await;
    lithia::dispose(&holder);
    assert!(event.is_some(), "loop count {}", *n.get());
}
