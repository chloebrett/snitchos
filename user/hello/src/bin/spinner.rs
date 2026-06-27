//! A child that never exits — it loops yielding the CPU. Used by the
//! `workload=wait-any` supervisor as a sibling that stays alive, so `WaitAny`
//! deterministically returns the *other* child (the one that exits).

#![no_std]
#![no_main]

use snitchos_user::{entry, yield_now};

#[entry]
fn main() {
    loop {
        yield_now();
    }
}
