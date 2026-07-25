//! The `glitch` audio server (`workload=glitch-beep`): serves `Play` requests
//! over its endpoint, holding the DAC as an `AudioSink` capability. The receive
//! loop lives in [`glitch::serve`]; this binary is just its entry point. Mirrors
//! `fs-server`. The kernel grants the endpoint (`RECV`) + the `AudioSink` via the
//! IPC launch path, so no `#[entry(needs)]` is declared.

#![no_std]
#![no_main]

use snitchos_user::entry;

#[entry]
fn main() {
    glitch::serve()
}
