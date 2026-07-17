# Plan — snemu page-straddling access fix + fidelity-debugging robustness

**Status:** planned (not started). Root cause confirmed; fix + follow-ups scoped below.

## The bug

snemu's instruction fetch translates the guest PC **once** and reads the whole
instruction as contiguous *physical* bytes:

```rust
// cpu.rs, slow-path fetch (Hart::step)
let pc_pa = self.translate_or_trap(self.pc, Access::Fetch, bus)?;
let half  = bus.read_u16(pc_pa)?;
let raw   = if is_compressed(half) { expand(half)? }
            else { bus.read_u32(pc_pa)? };   // <-- 4 contiguous physical bytes
```

Translation is **per-page**. A 4-byte instruction at a `…ffe` offset (legal under
the C extension, which allows 2-byte alignment of 4-byte instructions) has its high
16 bits in the *next* virtual page — which may map to a **non-contiguous physical
frame**. `read_u32(pc_pa)` then reads the high half from the wrong frame.

### Evidence (workload=supervised, `--opt mid`, pure interpreter)

```
pc=0x10000ffe  pc_pa=0x807d1ffe  hi_pa=0x807d4000  naive=0x513  correct=0x650513
```

The next virtual page (`0x10001000`) maps to physical `0x807d4000`, **not**
`0x807d2000`. snemu read the high half as `0x0000` (zeros in the wrong frame), so it
fetched `0x00000513` instead of `0x00650513`:

- `0x00650513` = `addi a0, a0, 6`  (the correct instruction)
- `0x00000513` = `addi a0, x0, 0`  = **`li a0, 0`**

The code was materialising a global table pointer with `auipc a0,0x4; addi a0,a0,6`
→ `0x10005000`. snemu zeroed `a0` → the null propagated (`a0 → s2`) → `ld a1,0x10(s2)`
faulted at address `0x10` → the kernel's U-fault handler **parks** the task (no
teardown, `kernel/src/trap/mod.rs:191`) → the supervisor died before emitting the
escalate trio (`supervised.halted`, `svc.crasher.escalated`,
`escalate.crasher.intensity-exceeded`) → `snemu diff` reported the drop.

**Why it stayed hidden:** it needs *both* a straddling instruction at `…ffe` *and*
the two pages on non-contiguous frames. The opt-1-pinned release userspace happened
to produce both; debug userspace lays out differently. 114/114 itests passed because
their layouts never hit the combination. It is a latent fidelity gap, not a
regression.

## Fix 1 — straddle-aware instruction fetch (the urgent one)

When the 4-byte read would cross a page boundary, translate and read each 16-bit
half separately. The straddle condition for a 2-byte-aligned PC is exactly
`(pc & 0xfff) == 0xffe` (the general form: the second half at `pc+2` lands in a
different page).

```rust
} else {
    self.cur_ilen = ILEN_FULL;
    if self.pc & 0xfff > 0xffc {
        // 4-byte instruction straddles a page boundary: its upper half is in the
        // next virtual page, which may map to a non-contiguous physical frame.
        // Translate + read each half separately (a faulting upper half traps as a
        // real instruction-page-fault, exactly like hardware).
        let lo = bus.read_u16(pc_pa)?;
        let Some(hi_pa) = self.translate_or_trap(self.pc + 2, Access::Fetch, bus) else {
            return Ok(HartEffect::None); // upper half faulted → trapped
        };
        u32::from(lo) | (u32::from(bus.read_u16(hi_pa)?) << 16)
    } else {
        bus.read_u32(pc_pa)?
    }
}
```

**Two fetch sites, both must change:**
1. `Hart::step` slow path (above) — the pure interpreter / oracle.
2. `Hart::fetch_for_compile` — the block-JIT (M6) frontend, which does its own
   `mmu::translate` + `read_u32`. It must stay byte-identical to the interpreter, so
   it needs the same split (or it can decline the block at a straddling PC and let the
   interpreter handle that instruction).

The Tier-1 decode cache keys decoded instructions by PC, so once fetch is correct the
cache stores the correct decode — no separate change, but it must be built *on* the
fixed fetch.

## Fix 2 — straddle-aware data load/store (defensive completeness)

The same single-translate-then-contiguous-read pattern exists for data:
`load`/`store` translate the data VA once (`translate_or_trap(va, Access::{Load,Store})`)
then read/write the full width at that PA.

**Severity note:** a *naturally aligned* access can never cross a 4 KiB page (8-byte
loads are 8-aligned; 8 divides 4096), so aligned `LD`/`SD` — what Rust normally emits
— are safe. Only a **misaligned** wide access that happens to straddle a page is
affected. That's far rarer than the fetch case (RVC guarantees the fetch hazard is
common), which is why the fetch bug fired and this one hasn't been observed. But it's
the same latent defect and should be closed while we're here.

- First audit `mem.rs` (`read_u32`/`read_u64` reject a straddle at the RAM *end*, per
  the existing test at `mem.rs:136`) to confirm current misaligned-within-page
  behaviour, and match QEMU's cross-page semantics.
- Then split any cross-page data access into two per-page accesses, or trap
  misaligned-across-page if that's what the target hardware model does. Decide against
  QEMU's actual behaviour (it handles misaligned including cross-page).
- Audit the native memop fast path (`memset`/`memcpy`): it already declines pages it
  would fault on, so it likely walks page-by-page — confirm it doesn't assume
  contiguity.

## Tests (snemu unit tests — enough to prevent regression)

The regression test *is* the prevention artifact. Build a guest page table where two
consecutive VPNs map to **non-contiguous** physical frames, place a 4-byte instruction
at `…ffe`, step once, and assert it executed correctly:

- **Fetch:** map VPN `N` → frame `X`, VPN `N+1` → frame `X+K` (K ≠ 1). Write
  `addi a0, a0, 6` across the boundary at `base+0xffe`. Step; assert `a0` changed by 6,
  not zeroed. A companion assert: the "naive" contiguous read would have produced a
  *different* word (so the test genuinely exercises the split, not a lucky layout).
- **Fetch upper-half fault:** leave VPN `N+1` unmapped; assert an
  instruction-page-fault trap with `stval` in the second page (matches hardware, which
  faults on the half it couldn't fetch).
- **Load/store:** a misaligned `LD`/`SD` crossing a non-contiguous boundary reads/writes
  the right bytes on both frames.

snemu already has cpu-level tests that assemble small programs (e.g.
`auipc_adds_the_immediate_to_the_physical_pc`); these extend that style with a
hand-built non-contiguous mapping. A small helper — "map these VPNs to these
(deliberately scrambled) frames" — is worth factoring so future tests can fragment
physical memory cheaply.

## Prevention baked into the design (so the next one can't be written)

- **A "fat" translation result.** `translate_or_trap` returns a bare `pa: u64`, which
  erases the page boundary the instant translation finishes — that erasure *is* the
  bug. Have it return `pa` **plus bytes-remaining-in-page**, so any reader with
  `width > remaining` is forced to handle the split. The type stops you from
  forgetting. (This is the same shape of lesson as the ramfb `SELECT`/`ERROR` swap:
  encode the constraint in the type, don't rely on the author remembering it.)
- **Debug-mode straddle self-check.** A `#[cfg(debug_assertions)]` assert in fetch that,
  whenever `(pc & 0xfff) > 0xffc`, compares the per-half read against the contiguous
  read and panics with the PC on mismatch. This is the detector that *found* the bug,
  promoted to a permanent invariant — it would have fired on the first release run
  instead of after a multi-hour hunt. Keep it after the fix as a guard against the
  fix regressing or a new access path skipping it.

## Robustness follow-ups (make the *next* fidelity bug trivial)

These are why the hunt took hours: the debugger operated on telemetry frames while the
bug lived in instruction fetch, three abstraction layers down. Each item below lets
snemu observe at the layer the bug actually lives at.

### A. Auto-report unexpected U-mode faults (agreed — do with the fix)

snemu is *our* emulator, so it can narrate its own divergence in a way a black-box
QEMU can't. On an unexpected U-mode page fault / illegal instruction, log
`sepc / scause / stval`, the faulting instruction word (decoded), and — via the
existing `snemu/src/symbols.rs` — the **symbolised** guest PC (`in <fn> called from
<fn>`). I had to add every one of these by hand; built in, step one of the hunt would
have printed `load fault @0x10, sepc=0x100012a6 (in hashbrown::…::find)`.

Design decisions:
- Default-on for `snemu boot` (the meta-loop / debugging driver); it already streams
  UART to stderr, so one more diagnostic line fits.
- Gate it so intentional-fault workloads (`userspace-fault`, `userspace-bad-ptr`, the
  stack-guard family) don't cry wolf — either a known-expected-fault allowlist or a
  `--trace-faults` opt-in for the itest path. First unexpected fault is the signal.
- Symbolisation needs the guest ELF's symbol table; the release userspace is stripped,
  so also support resolving against the *unstripped* build output, or emit raw PC when
  symbols are unavailable.

### B. Fault post-mortem + snapshot-resume debug mode (agreed — high value)

Every hypothesis cost a ~2.5-minute kernel+snemu rebuild. snemu already has
snapshot/fork (the `--share-snapshots` itest speedup, `run_fork`). Point it at
debugging:

- **Cheap first:** `--break-at-user-fault` — on the first unexpected U-mode fault, dump
  full state (all GPRs, `sepc/scause/stval`, faulting instruction, a symbolised call
  chain, and the surrounding stack) and stop. A fault-triggered post-mortem replaces
  the manual register-dump + objdump archaeology I did by hand.
- **Bigger:** snapshot at a checkpoint (or at `--snapshot-at <instret>`) and allow
  resume-with-pokes, so each "what if this register/byte were X" experiment is a second,
  not a rebuild. The fork infra exists; this is wiring, not new mechanism.

### C. Register/memory lockstep diff vs QEMU (larger, but the right long-term tool)

`snemu diff` diffs *telemetry frames* — the wrong altitude for an instruction-level
bug. A mode that single-steps snemu and QEMU (via QEMU's gdbstub) and reports the
**first architectural divergence** — "at `0x10000ffe`, `a0` should be `0x10005000`,
snemu has `0`" — would have pointed straight at the faulting instruction. This is the
tool this whole *class* of deterministic-fidelity bug will keep wanting; worth a
milestone of its own.

### D. Fragmenting test allocator

This bug was masked for a long time because snemu's tests map pages contiguously (the
natural thing to write), hiding the entire straddle/contiguity dimension. A test
harness that deliberately **scrambles frame assignment** would surface not just this
but the whole class of "assumed physical contiguity" defects (Fix 2's data-straddle
included). The helper from the Tests section is the seed of this.

## Related but separate — the `snemu diff --all` clock-skew verdict

Not part of this fix, tracked so it isn't lost. Frame-count matching fixed the
*dropped* direction (`0q` everywhere; `dropped_names` correctly caught *this* bug's
drop), but the sweep still shows 37/44 FAIL on the *invented* direction — that residue
is **clock-skew, not truncation**: snemu's `rdtime = instret` clock outruns QEMU's real
10 MHz mtime, so at an equal frame count snemu has already emitted a time-triggered
`kernel.heartbeat` QEMU hasn't reached in wall-time. `BENIGN_ONLY_SNEMU` forgives that
only when `snemu_crashed`; broadening the forgiveness to non-crashing workloads (after
confirming the `3s/4s/5s` multi-name cases are also purely time-triggered) would make
`--all` report real divergences instead of heartbeat noise. See the memory note
`project_snemu_diff_clockskew_sweep`.

## Sequencing

1. Fix 1 (fetch) + its unit tests + the debug-mode straddle assert. Verify
   `supervised --opt mid` now emits the escalate trio under snemu (the end-to-end
   proof), and re-run the itest gate.
2. Follow-up A (auto-report U-mode faults, symbolised) — small, lands with the fix.
3. Fix 2 (data load/store straddle) + tests + the fat translation-result type (the
   type change is what makes Fix 1 and Fix 2 permanent rather than spot-patches).
4. Follow-up B (fault post-mortem / snapshot-resume), then D (fragmenting test
   allocator).
5. Follow-up C (lockstep diff) — its own milestone.
6. Separately: the sweep clock-skew verdict.
