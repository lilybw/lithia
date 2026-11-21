// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Every tracing event emitted in this binary, collected by its one global subscriber.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// One captured event: its level and every field rendered as `name=value`.
#[derive(Clone, Debug)]
pub struct Captured {
    pub level: Level,
    pub text: String,
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

/// The events so far; the first call installs the subscriber.
pub fn events() -> Arc<Mutex<Vec<Captured>>> {
    static EVENTS: OnceLock<Arc<Mutex<Vec<Captured>>>> = OnceLock::new();
    EVENTS
        .get_or_init(|| {
            let events: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
            let subscriber: tracing_subscriber::layer::Layered<Collector, tracing_subscriber::Registry> =
                tracing_subscriber::registry().with(Collector(events.clone()));
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            events
        })
        .clone()
}
