//! The wasm32 half of the differential probe.
//!
//! Run with `wasm-pack test --node snemu-wasm`. This is the *actual* browser
//! target (`wasm32-unknown-unknown`), not a WASI stand-in — so a `std` call that
//! compiles everywhere but traps only here, which is exactly how this class of bug
//! presents, has nowhere to hide.
//!
//! The assertions live in `snemu_wasm::probe::check`, shared verbatim with the host
//! test. Nothing is asserted here that the host does not also assert, and nothing
//! is relaxed for wasm — a difference between the two targets is the finding.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

/// The 32-bit build must reproduce the 64-bit build's architectural result exactly:
/// same registers, same UART bytes, same retired instret. This is the assertion
/// that would catch a narrowed `usize` computing a different address.
#[wasm_bindgen_test]
fn the_probe_matches_the_host_under_wasm32() {
    snemu_wasm::probe::check_portable(&snemu_wasm::probe::run());
}

/// The measured limit of that agreement, kept executable so it cannot quietly
/// become untrue in either direction: `state_hash` folds in `usize`-width lengths,
/// so byte-identical machine state hashes differently in a 32-bit build. If this
/// ever starts passing, snemu's digest became width-independent and the cross-target
/// differential above can be widened to include it.
#[wasm_bindgen_test]
fn the_state_hash_is_width_dependent_and_so_stays_out_of_the_differential() {
    assert_ne!(
        snemu_wasm::probe::run().state_hash,
        snemu_wasm::probe::EXPECTED_STATE_HASH,
        "state_hash unexpectedly matches the 64-bit host"
    );
}

/// Which hashing component, if any, is target-dependent. Split into three so a
/// failure names the mechanism rather than leaving it to inference.
#[wasm_bindgen_test]
fn hashing_a_u64_matches_the_host() {
    assert_eq!(
        snemu_wasm::probe::hash_diagnostics()[0],
        snemu_wasm::probe::EXPECTED_HASH_DIAGNOSTICS[0]
    );
}

/// The mechanism, isolated: `<[u8]>::hash` length-prefixes with a `usize`, so this
/// is the one component of the three that does *not* survive the target change.
/// Asserting the inequality records the measurement rather than leaving it in a
/// commit message.
#[wasm_bindgen_test]
fn hashing_a_slice_via_hash_is_the_component_that_differs() {
    assert_ne!(
        snemu_wasm::probe::hash_diagnostics()[1],
        snemu_wasm::probe::EXPECTED_HASH_DIAGNOSTICS[1]
    );
}

#[wasm_bindgen_test]
fn hashing_raw_bytes_via_write_matches_the_host() {
    assert_eq!(
        snemu_wasm::probe::hash_diagnostics()[2],
        snemu_wasm::probe::EXPECTED_HASH_DIAGNOSTICS[2]
    );
}
