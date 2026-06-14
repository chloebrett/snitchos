#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, rights};

// One binary, two roles — the kernel decides which by the rights on the
// endpoint cap it granted this process. A `RECV | MINT` holder mints a badged
// `SEND` cap (observed as a `CapEvent::Transferred` carrying the badge); a
// `SEND`-only holder is refused (a `SyscallRefused`). The capability, not the
// code, decides the outcome — the whole point of v0.9c.
#[entry]
fn main() {
    let _ = endpoint().mint_badged(0xF00D, rights::SEND);
}
