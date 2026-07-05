//! The **unsatisfiable** child in `workload=manifest-satisfy`: it declares a need
//! the satisfier can't meet — an `Endpoint` with `RECV`, when the satisfier holds
//! only `SEND`. `hitch::satisfy` returns `Unsatisfied`, so the satisfier refuses to
//! `SpawnImage` it (snitching `satisfy.refused.recv`) rather than granting a
//! partial set. If this program ever runs, the satisfier wrongly granted an
//! unsatisfiable slot — the marker below is what the refusal scenario asserts is
//! *absent*.

#![no_std]
#![no_main]

use snitchos_user::{entry, register_counter};

#[entry(needs = [("recv", ENDPOINT, RECV)])]
fn main() {
    register_counter("snitchos.fs_hungry.ran").emit(1);
}
