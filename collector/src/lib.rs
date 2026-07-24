//! collector: decode the kernel's telemetry `Frame` stream and project it into
//! spans, metrics, and capability-derivation traces.
//!
//! This crate is split so the *core* — decode, span/cap state, the frame
//! projections — has no host-only dependencies and compiles for
//! `wasm32-unknown-unknown`. The browser front-end (see
//! `docs/uart-telemetry-design.md` §"Where this is going") runs that core in-tab
//! with no backend; the guest (snemu) already compiles to wasm unmodified, so it
//! would be perverse for the collector to be the reason a server is required.
//!
//! The host-only exporters (OTLP/Loki over HTTP, the Prometheus server) live
//! behind the default `native` feature and are absent from the wasm build. The
//! `collector` binary (`main.rs`) is a thin native wiring layer over this library.

pub mod state;

// Crate-internal: `caps` is an implementation detail of `state`'s cap-derivation
// projection, and `url` is used only by the native exporters. Neither is part of
// the public surface, so they stay `mod` — keeping them `pub` would expose e.g.
// `CapTracker` as public API and trip `new_without_default` for no reason.
mod caps;
#[cfg(feature = "native")]
mod url;

// HTTP exporters + the Prometheus server. `ureq` pulls in `ring`, which has no
// wasm build, so these are gated: present for the binary, absent for wasm.
#[cfg(feature = "native")]
pub mod loki;
// The frame-stream source (socket / file replay / serial). Native: it opens
// sockets and files. The wasm front-end feeds `decode_stream` an in-memory buffer.
#[cfg(feature = "native")]
pub mod source;
#[cfg(feature = "native")]
pub mod otlp;
#[cfg(feature = "native")]
pub mod prom;

/// Sink for completed spans. Implement this to add a new output format. Each
/// implementation receives every `CompletedSpan` produced by the kernel session;
/// routing (enable/disable, endpoint config) is the caller's responsibility.
pub trait SpanExporter: Send {
    fn export(&self, span: &state::CompletedSpan);
}

/// The console text a frame carries, if it is a guest log line (`Frame::Log`);
/// `None` for telemetry frames (spans, metrics, cap events, …).
///
/// The *render* half of "collector as terminal": under `console=frames` a `Log`
/// is the guest's own console output and should read as a plain line, not a
/// `Log { msg: …, task_id: … }` struct dump. The reader prints the returned line
/// verbatim and falls back to the `Debug` form otherwise. See
/// `docs/uart-telemetry-design.md` Decision 4.
#[must_use]
pub fn log_text<'m>(frame: &protocol::Frame<'m>) -> Option<&'m str> {
    match frame {
        protocol::Frame::Log { msg, .. } => Some(msg),
        _ => None,
    }
}

#[cfg(test)]
mod log_text_tests {
    use super::log_text;
    use protocol::{Frame, SpanId};

    #[test]
    fn a_log_frame_yields_its_message() {
        let f = Frame::Log { msg: "entering heartbeat", task_id: 0, t: 1, hart_id: 0 };
        assert_eq!(log_text(&f), Some("entering heartbeat"));
    }

    #[test]
    fn a_telemetry_frame_yields_nothing() {
        // A span end is telemetry, not a console line — the reader shows it as Debug.
        assert_eq!(log_text(&Frame::SpanEnd { id: SpanId(1), t: 2 }), None);
    }
}
