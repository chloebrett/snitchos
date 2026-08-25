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

/// Build a minimal valid ELF64 with a single `PT_LOAD` segment.
///
/// A local copy of snemu's own `loader::tests::tiny_elf`, which is `#[cfg(test)]`
/// and so unreachable from here. Duplicated deliberately rather than made public:
/// this is test scaffolding, and widening a crate's API to share scaffolding is a
/// worse trade than thirty lines of header bytes.
fn tiny_elf(entry: u64, segment: &[u8]) -> Vec<u8> {
    let mut img = vec![0u8; 64];
    img[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    img[4] = 2; // ELFCLASS64
    img[5] = 1; // ELFDATA2LSB
    img[6] = 1; // EV_CURRENT
    img[0x12..0x14].copy_from_slice(&243u16.to_le_bytes()); // EM_RISCV
    img[0x18..0x20].copy_from_slice(&entry.to_le_bytes()); // e_entry
    img[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    img[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    img[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

    let mut ph = vec![0u8; 56];
    ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    ph[0x08..0x10].copy_from_slice(&120u64.to_le_bytes()); // p_offset (64 + 56)
    ph[0x10..0x18].copy_from_slice(&entry.to_le_bytes()); // p_vaddr
    ph[0x18..0x20].copy_from_slice(&entry.to_le_bytes()); // p_paddr
    ph[0x20..0x28].copy_from_slice(&(segment.len() as u64).to_le_bytes()); // p_filesz
    ph[0x28..0x30].copy_from_slice(&(segment.len() as u64).to_le_bytes()); // p_memsz
    img.extend_from_slice(&ph);
    img.extend_from_slice(segment);
    img
}

/// The `#[wasm_bindgen]` shell, exercised **across the real JS boundary** rather
/// than merely compiled for it.
///
/// This test carries more weight than its size suggests. `Handle` exists only on
/// wasm32, so the host suite cannot reach it at all — and the shell's methods are
/// exactly where a browser-only failure would hide. Step 1b's whole lesson was that
/// "it compiles for wasm32" is the weaker claim; this is the stronger one for the
/// one module that cannot be checked any other way.
#[wasm_bindgen_test]
fn the_shell_boots_a_guest_steps_it_and_drains_its_output() {
    // A guest that writes 'A' to the ns16550a transmit register, then spins.
    let entry = 0x8000_0000u64;
    let program: [u32; 5] = [
        0x0010_0313, // addi x6, x0, 1
        0x01C3_1313, // slli x6, x6, 28   -> 0x1000_0000, the UART
        0x0410_0393, // addi x7, x0, 0x41 -> 'A'
        0x0073_0023, // sb   x7, 0(x6)
        0x0000_006F, // jal  x0, 0        -> spin
    ];
    let mut segment = Vec::new();
    for w in program {
        segment.extend_from_slice(&w.to_le_bytes());
    }

    let elf = tiny_elf(entry, &segment);
    let mut h = snemu_wasm::shell::Handle::new(&elf, 1024 * 1024).expect("the ELF loads");

    assert_eq!(h.instret(), 0, "a freshly loaded guest has retired nothing");

    let status = h.step_budget(32).expect("stepping serializes");
    assert!(status.contains("Running"), "expected Running, got {status}");
    assert_eq!(h.instret(), 32, "the clock advanced by the budget");

    assert_eq!(h.drain_uart(), "A", "the guest's UART byte reached the page");
    assert_eq!(h.drain_uart(), "", "and the cursor does not deliver it twice");

    assert_eq!(
        h.drain_frames().expect("frames serialize"),
        "[]",
        "this guest emits no telemetry, and an empty drain is still valid JSON"
    );
}

/// A bad image must arrive as a JS error, not a panic. A Rust panic across the wasm
/// boundary aborts the module and takes the page with it; an `Err` is something the
/// page can render.
#[wasm_bindgen_test]
fn a_non_elf_image_is_reported_as_an_error() {
    assert!(snemu_wasm::shell::Handle::new(b"not an elf", 1024 * 1024).is_err());
}
