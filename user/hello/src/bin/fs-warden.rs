//! The **exact-match** child in `workload=manifest-satisfy`: it declares exactly the
//! rights the satisfier holds — an `Endpoint` with `MINT | SEND` — so `hitch::satisfy`
//! returns `Grant::Use` and the satisfier delegates its wide cap as-is (no mint).
//! Reads the cap by role name, attaches to the FS, and emits a marker — proving the
//! Use path (the counterpart to `fs-probe`'s attenuated `Mint`).

#![no_std]
#![no_main]

use snitchos_user::{Endpoint, bootstrap, entry, register_counter};

#[entry(needs = [("fs", ENDPOINT, MINT | SEND)])]
fn main() {
    let Some(fs) = bootstrap().get::<Endpoint>("fs") else {
        return;
    };
    // The wide cap still carries `SEND`, so it attaches like any client — proving the
    // exact-match `Use` delegation landed a working cap.
    if let Ok((_r, Some(_root))) = fs.call([0, 0, 0, 0]) {
        register_counter("snitchos.fs_warden.reached").emit(1);
    }
}
