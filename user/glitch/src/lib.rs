//! The userspace `glitch` audio server's receive loop, factored out of the binary
//! (`bin/beep.rs` supplies no policy — it's a client). Mirrors [`fs::serve`].
//!
//! Holds an `AudioSink` cap and `RECV` on the shared endpoint. One kind of caller
//! in v1: a client `call`s a [`Play`] request; glitch synthesizes it (the pure
//! [`glitch_core::plan_play`] policy), streams the samples through the cap-gated
//! `AudioWrite` syscall, and replies `Played`/`Refused`. Volume is the server's
//! policy, so a client names only the note. See `plans/glitch.md` (Increment 5c).

#![no_std]

use glitch_core::plan_play;
use glitch_proto::{Play, Reply};
use snitchos_user::{
    AUDIO_WRITE_MAX, Endpoint, Metric, audio_write, delegated_handle, register_counter, reply,
    tracer,
};

/// The shared glitch endpoint, read as the first **delegated** cap. Matches
/// `fs::serve`: the kernel `run_ipc` launch path lands the endpoint at
/// `delegated_handle(0)` (after the two bootstrap caps).
fn glitch_endpoint() -> Endpoint {
    Endpoint::from_raw_handle(delegated_handle(0))
}

/// The `AudioSink` cap the kernel granted glitch — the **second** delegated cap,
/// right after the endpoint (`run_ipc` grants endpoint then `AudioSink`).
fn audio_sink() -> usize {
    delegated_handle(1)
}

/// Serve `Play` requests over the glitch endpoint forever. Registers the server's
/// own `plays_total` counter (the kernel doesn't know it ahead of time), then
/// answers each `call` — synthesizing and emitting on success, snitching `Refused`
/// on a bad frequency or a refused write.
pub fn serve() -> ! {
    let plays: Metric = register_counter("snitchos.glitch.plays_total");
    let sink = audio_sink();
    loop {
        let Ok(r) = glitch_endpoint().receive_with_reply() else {
            continue;
        };
        let Some(reply_handle) = r.reply else {
            continue; // one-way send: no request semantics in v1
        };
        let Ok(req) = Play::decode(r.msg) else {
            let _ = reply(reply_handle, Reply::Refused.encode());
            continue;
        };
        // Each play is a span. The kernel seeded our cursor with the caller's op
        // span on `receive`, so this nests under the client's request — the trace
        // crosses the process boundary for free (as in `fs::serve`).
        let _play = tracer().span("glitch.play");
        let reply_msg = match emit(sink, req) {
            Ok(()) => {
                plays.emit(1);
                Reply::Played
            }
            Err(()) => Reply::Refused,
        };
        let _ = reply(reply_handle, reply_msg.encode());
    }
}

/// Synthesize `req` and stream it to the DAC in ≤[`AUDIO_WRITE_MAX`] chunks (the
/// kernel's per-call cap). `Err` if the frequency is unsynthesizable, or the kernel
/// refused a write (we don't hold the `AudioSink`, or a bad range).
fn emit(sink: usize, req: Play) -> Result<(), ()> {
    let samples = plan_play(req).ok_or(())?;
    let mut buf = [0i16; AUDIO_WRITE_MAX];
    let mut n = 0;
    for s in samples {
        buf[n] = s;
        n += 1;
        if n == AUDIO_WRITE_MAX {
            audio_write(sink, &buf).map_err(|_| ())?;
            n = 0;
        }
    }
    if n > 0 {
        audio_write(sink, &buf[..n]).map_err(|_| ())?;
    }
    Ok(())
}
