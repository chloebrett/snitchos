//! The telemetry backend abstraction: *what happens* when a Stitch program
//! opens a span or emits a metric, decoupled from the `emit`/`span` natives that
//! trigger it.
//!
//! A program holds one backend for its whole run (shared through every [`Env`]
//! clone). Two live today:
//!
//! - [`RecordingTelemetry`] (the default) buffers events in memory — what the
//!   host REPL and the test harness read back, and what `wire::lower` turns into
//!   wire frames for the collector.
//! - the on-target backend (a later increment, target-only) routes straight to
//!   `user/runtime`'s capability-mediated syscalls — `tracer().span()`,
//!   `telemetry()` metric `emit`, `clock_now()` — so a Stitch process on
//!   `SnitchOS` produces real frames through the kernel pipeline.
//!
//! [`Env`]: crate::env::Env

use core::cell::RefCell;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;
use crate::value::{TelemetryEvent, Value};

/// A sink for the telemetry a Stitch program produces. Methods take `&self`
/// (backends use interior mutability) so one backend can be shared, via `Rc`,
/// across every scope and closure of a run.
pub trait Telemetry {
    fn span_open(&self, name: &str);
    fn span_close(&self, name: &str);
    fn emit(&self, name: &str, value: &Value);

    /// A copy of everything recorded so far. Non-recording backends (e.g. the
    /// on-target one, whose events have already left as frames) return empty.
    fn snapshot(&self) -> Vec<TelemetryEvent> {
        Vec::new()
    }

    /// Like [`snapshot`](Self::snapshot) but also clears the buffer — lets a
    /// long-lived REPL render just this line's events.
    fn drain(&self) -> Vec<TelemetryEvent> {
        Vec::new()
    }
}

/// The default backend: buffer every event in memory. The v0 in-memory sink,
/// now behind the trait.
#[derive(Default)]
pub struct RecordingTelemetry {
    events: RefCell<Vec<TelemetryEvent>>,
}

impl Telemetry for RecordingTelemetry {
    fn span_open(&self, name: &str) {
        self.events
            .borrow_mut()
            .push(TelemetryEvent::SpanOpen { name: name.into() });
    }

    fn span_close(&self, name: &str) {
        self.events
            .borrow_mut()
            .push(TelemetryEvent::SpanClose { name: name.into() });
    }

    fn emit(&self, name: &str, value: &Value) {
        self.events.borrow_mut().push(TelemetryEvent::Emit {
            name: name.into(),
            value: value.clone(),
        });
    }

    fn snapshot(&self) -> Vec<TelemetryEvent> {
        self.events.borrow().clone()
    }

    fn drain(&self) -> Vec<TelemetryEvent> {
        core::mem::take(&mut *self.events.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use core::cell::Cell;

    #[derive(Default)]
    struct CountingBackend {
        opens: Cell<u32>,
        closes: Cell<u32>,
        emits: Cell<u32>,
    }

    impl Telemetry for CountingBackend {
        fn span_open(&self, _name: &str) {
            self.opens.set(self.opens.get() + 1);
        }
        fn span_close(&self, _name: &str) {
            self.closes.set(self.closes.get() + 1);
        }
        fn emit(&self, _name: &str, _value: &Value) {
            self.emits.set(self.emits.get() + 1);
        }
    }

    #[test]
    fn env_routes_telemetry_to_the_installed_backend() {
        let backend = Rc::new(CountingBackend::default());
        let env = Env::with_telemetry(backend.clone());

        env.span_open("s");
        env.emit_metric("m", &Value::Int(1));
        env.span_close("s");

        assert_eq!(
            (
                backend.opens.get(),
                backend.emits.get(),
                backend.closes.get()
            ),
            (1, 1, 1),
        );
    }

    #[test]
    fn the_recording_backend_buffers_events_in_order() {
        let rec = RecordingTelemetry::default();

        rec.span_open("s");
        rec.emit("m", &Value::Int(7));
        rec.span_close("s");

        assert_eq!(
            rec.snapshot(),
            vec![
                TelemetryEvent::SpanOpen { name: "s".into() },
                TelemetryEvent::Emit {
                    name: "m".into(),
                    value: Value::Int(7),
                },
                TelemetryEvent::SpanClose { name: "s".into() },
            ],
        );
    }
}
