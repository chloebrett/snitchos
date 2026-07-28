//! FP context-switch oracle (`workload=fp-churn`): fill every FP register with a
//! pattern only this process would produce, get preempted, read them back, and
//! report how many came back wrong.
//!
//! Two copies of this program run concurrently. That is the whole design: the
//! kernel is zero-FP, so a *single* FP process can never observe a clobber — there
//! is nobody to clobber it. Only a second FP process makes the register file
//! contended, which is exactly the case `FpEnableDecision::RefuseBusy` used to
//! refuse and `switch_action` now has to handle.
//!
//! **Why the fill, spin and read-back live in one `asm!` block.** Split across three
//! blocks, the compiler is entitled to use FP registers in between — it would be
//! clobbering them legitimately, and the test would fail for a reason that has
//! nothing to do with the scheduler. Inside one block, nothing but the kernel can
//! touch them, so a mismatch has exactly one possible author.
//!
//! The spin is an integer countdown rather than a sleep: it must span at least one
//! timer tick so preemption lands *inside* the block, and it must not make a syscall
//! (which would be a second, uninteresting path to the same switch).

#![no_std]
#![no_main]

use snitchos_user::{entry, register_counter, register_gauge};

/// Rounds of fill-spin-compare. Each round is an independent trial; more rounds
/// means more chances for a preemption to land inside the window.
const ROUNDS: u64 = 64;

/// Countdown per round. Sized to comfortably exceed a timer tick so that the
/// preemption this test exists to survive actually happens — the scenario asserts
/// context switches climbed, so an undersized spin fails loudly rather than passing
/// vacuously.
const SPIN: u64 = 300_000;

/// Fill `f0`–`f31` from `pattern`, burn `SPIN` iterations, then store them to `out`.
///
/// # Safety
///
/// `pattern` and `out` must each be valid for 32 `u64`s. The FP registers are
/// clobbered, which the operand list declares.
unsafe fn churn(pattern: *const u64, out: *mut u64) {
    // SAFETY: caller guarantees both pointers address 32 u64s; every FP register
    // written is declared clobbered below. `fld`/`fsd` need `sstatus.FS` on — the
    // first `fld` traps and the kernel's lazy enable turns it on and retries.
    unsafe {
        core::arch::asm!(
            // Fill from the pattern.
            "fld f0,    0({p})", "fld f1,    8({p})", "fld f2,   16({p})", "fld f3,   24({p})",
            "fld f4,   32({p})", "fld f5,   40({p})", "fld f6,   48({p})", "fld f7,   56({p})",
            "fld f8,   64({p})", "fld f9,   72({p})", "fld f10,  80({p})", "fld f11,  88({p})",
            "fld f12,  96({p})", "fld f13, 104({p})", "fld f14, 112({p})", "fld f15, 120({p})",
            "fld f16, 128({p})", "fld f17, 136({p})", "fld f18, 144({p})", "fld f19, 152({p})",
            "fld f20, 160({p})", "fld f21, 168({p})", "fld f22, 176({p})", "fld f23, 184({p})",
            "fld f24, 192({p})", "fld f25, 200({p})", "fld f26, 208({p})", "fld f27, 216({p})",
            "fld f28, 224({p})", "fld f29, 232({p})", "fld f30, 240({p})", "fld f31, 248({p})",
            // Burn time in integer-land so a timer tick lands in here.
            "2:",
            "addi {n}, {n}, -1",
            "bnez {n}, 2b",
            // Read back whatever survived.
            "fsd f0,    0({o})", "fsd f1,    8({o})", "fsd f2,   16({o})", "fsd f3,   24({o})",
            "fsd f4,   32({o})", "fsd f5,   40({o})", "fsd f6,   48({o})", "fsd f7,   56({o})",
            "fsd f8,   64({o})", "fsd f9,   72({o})", "fsd f10,  80({o})", "fsd f11,  88({o})",
            "fsd f12,  96({o})", "fsd f13, 104({o})", "fsd f14, 112({o})", "fsd f15, 120({o})",
            "fsd f16, 128({o})", "fsd f17, 136({o})", "fsd f18, 144({o})", "fsd f19, 152({o})",
            "fsd f20, 160({o})", "fsd f21, 168({o})", "fsd f22, 176({o})", "fsd f23, 184({o})",
            "fsd f24, 192({o})", "fsd f25, 200({o})", "fsd f26, 208({o})", "fsd f27, 216({o})",
            "fsd f28, 224({o})", "fsd f29, 232({o})", "fsd f30, 240({o})", "fsd f31, 248({o})",
            p = in(reg) pattern,
            o = in(reg) out,
            n = inout(reg) SPIN => _,
            out("f0") _, out("f1") _, out("f2") _, out("f3") _,
            out("f4") _, out("f5") _, out("f6") _, out("f7") _,
            out("f8") _, out("f9") _, out("f10") _, out("f11") _,
            out("f12") _, out("f13") _, out("f14") _, out("f15") _,
            out("f16") _, out("f17") _, out("f18") _, out("f19") _,
            out("f20") _, out("f21") _, out("f22") _, out("f23") _,
            out("f24") _, out("f25") _, out("f26") _, out("f27") _,
            out("f28") _, out("f29") _, out("f30") _, out("f31") _,
            options(nostack),
        );
    }
}

#[entry]
fn main() {
    // A pattern this process alone would write, so a clobber shows up as a value
    // that could only have come from the other process.
    //
    // The seed has to be a *runtime* difference: both processes are the same ELF at
    // the same virtual addresses, so nothing static — not a global's address, not a
    // stack slot's — differs between them. The clock does: the two are spawned in
    // sequence, so they read different ticks. Emitted below, and the scenario asserts
    // it saw two distinct values, because two processes that happened to agree on a
    // seed would make a clobber invisible and the test vacuous.
    let seed = snitchos_user::clock_now() | 1;
    let mut pattern = [0u64; 32];
    for (i, slot) in pattern.iter_mut().enumerate() {
        // Bit patterns, not floats: `fld`/`fsd` move bits without interpreting them,
        // so nothing here can be canonicalised into a different NaN on the way
        // through. A float-valued test would be checking the FPU, not the switch.
        *slot = seed.wrapping_mul(i as u64 + 1) ^ (i as u64) << 56;
    }

    let mismatches = register_counter("snitchos.fp_churn.mismatches_total");
    let rounds = register_counter("snitchos.fp_churn.rounds_total");
    let witness = register_gauge("snitchos.fp_churn.first_bad_register");
    // Published so the scenario can prove the two processes are distinguishable at
    // all — see the seed comment above.
    register_gauge("snitchos.fp_churn.seed").emit(seed as i64);

    let mut bad = 0i64;
    let mut first_bad = -1i64;
    for round in 1..=ROUNDS {
        let mut seen = [0u64; 32];
        // SAFETY: both arrays are 32 u64s and live for the call.
        unsafe { churn(pattern.as_ptr(), seen.as_mut_ptr()) };
        for (i, (want, got)) in pattern.iter().zip(seen.iter()).enumerate() {
            if want != got {
                bad += 1;
                if first_bad < 0 {
                    first_bad = i as i64;
                }
            }
        }
        rounds.emit(round as i64);
        mismatches.emit(bad);
        witness.emit(first_bad);
    }

    // Report and stop. Exiting (rather than spinning) keeps the scenario's frame
    // stream short and lets the reaper prove the FP release path runs.
    snitchos_user::exit();
}
