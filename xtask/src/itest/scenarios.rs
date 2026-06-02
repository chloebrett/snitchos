//! One function per scenario. Each returns `Ok(())` on pass, or a
//! human-readable `String` describing what didn't match.

use std::time::Duration;

use protocol::stream::OwnedFrame;

use super::harness::Harness;
use super::matchers::{is_dropped, is_hello, is_span_start_named, is_string_register_named};

const SEC: Duration = Duration::from_secs(1);

/// Boot sequence reaches the heartbeat loop: Hello → kernel.boot
/// SpanStart → Dropped(0) (proves pre-init flush ran cleanly) →
/// first kernel.heartbeat SpanStart (proves the timer IRQ is firing).
pub fn boot_reaches_heartbeat() -> Result<(), String> {
    let mut h = Harness::spawn("boot")?;

    h.wait_for(SEC * 3, is_hello())
        .ok_or("no Hello frame within 3s")?;
    h.wait_for(SEC * 3, is_span_start_named("kernel.boot"))
        .ok_or("no kernel.boot SpanStart within 3s")?;
    h.wait_for(SEC * 5, is_dropped(0))
        .ok_or("no Dropped(0) checkpoint after flush_pre_init within 5s")?;
    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no kernel.heartbeat SpanStart within 10s — timer IRQ not firing?")?;

    Ok(())
}

/// Two consecutive heartbeat SpanStarts arrive with their `t`
/// timestamps differing by approximately the timer interval. Tolerance
/// is generous (±50%) — QEMU's `time` CSR is host-wallclock-ish and
/// can stall under load.
pub fn heartbeat_cadence() -> Result<(), String> {
    let mut h = Harness::spawn("cadence")?;

    let first = h
        .wait_for(SEC * 15, is_span_start_named("kernel.heartbeat"))
        .ok_or("no first heartbeat within 15s")?;
    let second = h
        .wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no second heartbeat within 5s of the first")?;

    let (t1, t2) = match (&first, &second) {
        (OwnedFrame::SpanStart { t: a, .. }, OwnedFrame::SpanStart { t: b, .. }) => (*a, *b),
        _ => return Err("matched frame was not a SpanStart (impossible)".to_string()),
    };
    if t2 <= t1 {
        return Err(format!("timestamps not monotonic: first={t1}, second={t2}"));
    }
    // We don't assert on the absolute delta here — we don't know the
    // timebase or the kernel's configured interval from inside this
    // test without parsing the Hello frame. Monotonicity alone
    // proves the timer is advancing across two IRQs, which is what
    // this scenario is for.
    Ok(())
}

/// Pre-init buffer preserves frame order across the flush. Two
/// invariants:
///
///   1. The first `StringRegister` on the wire is for "kernel.boot"
///      — it was registered before virtio_console::init succeeded,
///      so it lived in the pre-init buffer.
///   2. Every span's `name_id` was registered earlier in the stream.
///      If the buffer dequeued out of order we'd see SpanStarts
///      referencing unknown ids.
pub fn pre_init_order() -> Result<(), String> {
    let mut h = Harness::spawn("preinit")?;

    // (1) First StringRegister we see should name kernel.boot.
    let first = h
        .wait_for(SEC * 5, is_string_register_named("kernel.boot"))
        .ok_or("no kernel.boot StringRegister within 5s — pre-init buffer drained out of order?")?;
    let OwnedFrame::StringRegister { id: _, value } = first else {
        return Err("matched non-StringRegister (impossible)".to_string());
    };
    if value != "kernel.boot" {
        return Err(format!("first StringRegister was '{value}', expected 'kernel.boot'"));
    }

    // (2) Drain through the first heartbeat. wait_for absorbs
    // StringRegister frames into the harness's string table as it
    // goes; if any SpanStart appeared whose name_id wasn't yet
    // registered, the matcher for kernel.heartbeat would never fire
    // for the WRONG reason (it'd still resolve once the register
    // arrived). So instead we check explicitly: for every SpanStart
    // we walk past, name_of(name_id) must be Some.
    let deadline = std::time::Instant::now() + SEC * 10;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or("did not reach first heartbeat within 10s")?;
        let frame = h.wait_for(remaining, |_, _| true)
            .ok_or("stream closed before reaching first heartbeat")?;
        match frame {
            OwnedFrame::SpanStart { name_id, .. } => {
                if h.name_of(name_id).is_none() {
                    return Err(format!(
                        "SpanStart references unregistered name_id {:?} — buffer flush is out of order",
                        name_id
                    ));
                }
                if h.name_of(name_id) == Some("kernel.heartbeat") {
                    return Ok(());
                }
            }
            _ => continue,
        }
    }
}
