//! The `glitch` beep client (`workload=glitch-beep`) — the userspace re-cast of
//! the in-kernel beep. Holds a bare `SEND` on the glitch endpoint and asks the
//! server to play one 440 Hz tone; the *server* owns the DAC cap and the volume,
//! so this client names only the note. Mirrors `fs-client`. See `plans/glitch.md`
//! (Increment 6).

#![no_std]
#![no_main]

use glitch_proto::{Play, Reply};
use snitchos_user::{Endpoint, Metric, bootstrap, entry, register_counter, tracer};

/// The beep: 440 Hz for 1 s — the same tone as the in-kernel Tier-0 beep, now
/// sourced from a userspace server holding the DAC cap.
const BEEP_FREQ_HZ: u32 = 440;
const BEEP_DURATION_MS: u32 = 1000;

#[entry(needs = [("glitch", ENDPOINT, SEND)])]
fn main() {
    // The client names its own checkpoint metric — one emit when the server
    // confirms the play. The itest asserts on the *server's* `plays_total` /
    // `samples_emitted`; this is the client-side witness that the reply arrived.
    let played: Metric = register_counter("snitchos.beep.played_total");

    // Resolve the glitch endpoint by the role name declared in `#[entry(needs)]`
    // — by name, not a positional handle index (it lands at the first delegated
    // slot either way).
    let Some(glitch) = bootstrap().get::<Endpoint>("glitch") else {
        return;
    };

    // Ask for one tone. The span stays open across the `call`, so the server's
    // `glitch.play` nests under it across the process boundary (as in `fs-client`).
    let _s = tracer().span("beep.request");
    let req = Play { freq_hz: BEEP_FREQ_HZ, duration_ms: BEEP_DURATION_MS };
    let Ok((words, _)) = glitch.call(req.encode()) else {
        return;
    };
    if let Ok(Reply::Played) = Reply::decode(words) {
        played.emit(1);
    }
}
