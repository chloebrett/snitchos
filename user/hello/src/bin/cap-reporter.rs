//! `cap-reporter` — a supervised service that **snitches on its own authority,
//! then crashes** (supervision step 4). Each incarnation enumerates its *own*
//! capability table via `cap_list` and reports whether the supervisor's durable
//! endpoint (`svc-ep`, delegated as a badged `SEND`) actually landed — then exits
//! non-zero so the supervisor restarts it and re-grants the cap.
//!
//! This is the snitch-on-the-snitch oracle for the D3 invariant: the kernel's
//! `CapEvent::Transferred` is the supervisor's *claim* that it re-granted the cap;
//! `cap_list` from the restarted service is the holder's *independent* report of
//! what it holds. A `holds_endpoint = 1` emitted by a **post-restart** incarnation
//! proves the re-grant reached the fresh `CapTable` — not just the first launch.

#![no_std]
#![no_main]

use snitchos_user::{CapDesc, cap_list, entry, exit_with, object_kind, register_counter, rights};

/// The object name the supervisor gave its durable endpoint (`endpoint_create`).
const ENDPOINT_NAME: &str = "svc-ep";

#[entry]
fn main() {
    // Enumerate our own holdings. Bootstrap gives us telemetry + span; the only
    // ENDPOINT-kind cap we could hold is the one the supervisor delegated.
    let mut buf = [CapDesc::default(); 8];
    let n = cap_list(&mut buf);
    let held = &buf[..n.min(buf.len())];

    let holds_endpoint = held.iter().any(|c| {
        c.kind == object_kind::ENDPOINT
            && c.rights & rights::SEND != 0
            && c.name_str() == ENDPOINT_NAME
    });

    // Report what we independently observe — 1 only if the re-granted cap is really
    // in our table with the expected object and rights.
    register_counter("snitchos.reporter.holds_endpoint").emit(i64::from(holds_endpoint));

    // Crash: a non-zero exit is a failure the supervisor restarts (and re-grants).
    exit_with(17);
}
