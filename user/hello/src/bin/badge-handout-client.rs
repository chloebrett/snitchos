#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry};

// v0.9c cap-transfer-in-reply client. `call`s the handout server asking for a
// cap badged 0xBEEF; the server transfers a badged cap back, which `call`
// returns as `Some(handle)`. Emit a telemetry tick on receipt so the itest can
// see the capability crossed the process boundary.
#[entry]
fn main() {
    if let Ok((_resp, Some(_cap))) = endpoint().call([0xBEEF, 0, 0, 0]) {
        let _ = telemetry().emit(1);
    }
}
