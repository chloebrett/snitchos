//! The `glitch` under-run probe (`workload=glitch-starve`) — a deliberately bad
//! real-time citizen, so the `XRun` observable can be watched *failing*.
//!
//! It holds the `AudioSink` directly (no server, no IPC: this is about the DAC feed
//! deadline, not the cap graph), enqueues one short batch **declaring more samples are
//! coming**, and then never feeds again. The ring drains within milliseconds and every
//! audio deadline after that is a genuine missed feed.
//!
//! Why this exists: until the stream-active signal landed, `AUDIO_ACTIVE` was never set
//! and an empty ring always read as idle silence — the under-run path was live code
//! that could not execute. The negative control is the point. A real-time observable
//! nobody has watched fail is indistinguishable from a healthy system, which is exactly
//! how `fp_init_hart`'s missing hand-off hid until a *negative* oracle went looking.
//!
//! See `plans/glitch-v2-async-ring.md` Increment 9.

#![no_std]
#![no_main]

use snitchos_user::{
    Metric, audio_enqueue_streaming, delegated_handle, entry, register_counter, tracer, yield_now,
};

/// Samples to prime the ring with. Small on purpose: at the 8 kHz feed this is a few
/// milliseconds of audio, so the ring runs dry almost immediately and the first
/// under-run lands well inside one heartbeat rather than several seconds later.
const PRIME_SAMPLES: usize = 32;

/// The `AudioSink` granted by the `IpcAudio` launch path (cap-table slot 3).
const AUDIO_SINK: usize = delegated_handle(1);

#[entry]
fn main() {
    let primed: Metric = register_counter("snitchos.starve.primed_total");

    // A quiet, non-zero waveform. The samples are never meant to be listened to —
    // what matters is that the ring holds something and then stops being refilled.
    let mut buf = [0i16; PRIME_SAMPLES];
    let mut i = 0;
    while i < PRIME_SAMPLES {
        buf[i] = if i % 2 == 0 { 512 } else { -512 };
        i += 1;
    }

    let _s = tracer().span("starve.prime");
    // `more = true` is the whole experiment: it promises the kernel a stream that this
    // process then abandons. Without the promise the drain would read the silence as
    // the end of a play (`Idle`) and turn the feed off, and no fault would be counted.
    if audio_enqueue_streaming(AUDIO_SINK, &buf, true).is_ok() {
        primed.emit(1);
    }
    drop(_s);

    // Starve it. Yield forever without ever refilling, so the drain keeps finding an
    // empty ring while the stream is still declared open.
    loop {
        yield_now();
    }
}
