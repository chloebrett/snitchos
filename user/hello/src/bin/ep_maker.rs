//! `workload=endpoint-create` — proves the `EndpointCreate` syscall hands back a
//! real, owning endpoint capability. The program manufactures its own endpoint
//! (no kernel-created one), then mints a badged `SEND` cap on it: minting requires
//! the returned cap to actually name an endpoint *and* carry `MINT`, so a success
//! proves `EndpointCreate` delivered the owning `RECV | MINT` cap. Emits a marker
//! (1 = minted, 0 = refused). The full `RECV` round-trip is exercised in Step 6
//! (init brings up the FS server on its created endpoint).

#![no_std]
#![no_main]

use snitchos_user::{endpoint_create, entry, exit, register_counter, rights};

#[entry]
fn main() {
    let ep = endpoint_create();
    let minted = ep.mint_badged(0xE9, rights::SEND).is_ok();
    register_counter("snitchos.epmaker.minted").emit(i64::from(minted));
    exit();
}
