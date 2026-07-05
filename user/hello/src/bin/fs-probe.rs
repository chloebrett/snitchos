//! The child in `workload=manifest-satisfy`: `SpawnImage`d by the `satisfier`,
//! which read *this program's* declared `needs` off the FS and granted the `fs`
//! endpoint **by name** (not a positional handle). Reads that cap via
//! `bootstrap().get`, attaches to the FS, and emits a marker — proving the
//! name-declared, `satisfy`-granted cap actually works end to end.

#![no_std]
#![no_main]

use snitchos_user::{Endpoint, bootstrap, entry, register_counter};

#[entry(needs = [("fs", ENDPOINT, SEND)])]
fn main() {
    // Resolve the FS endpoint by the role name we declared — the satisfier put it
    // at the first delegated handle to satisfy this exact slot.
    let Some(fs) = bootstrap().get::<Endpoint>("fs") else {
        return;
    };
    // Attach (a badge-0 send mints the root cap). Getting a root cap back proves
    // the satisfied `SEND` cap reaches the live FS.
    if let Ok((_r, Some(_root))) = fs.call([0, 0, 0, 0]) {
        register_counter("snitchos.fs_probe.reached").emit(1);
    }
}
