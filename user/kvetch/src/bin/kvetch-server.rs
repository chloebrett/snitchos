//! `kvetch-server` — the completion server, backed by babble (no weights).
//! The receive loop lives in [`kvetch::serve`]; this binary is the entry point.

#![no_std]
#![no_main]

use snitchos_user::entry;

#[entry]
fn main() {
    kvetch::serve()
}
