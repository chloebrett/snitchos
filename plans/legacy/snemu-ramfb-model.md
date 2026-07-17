# Plan: a real ramfb device model in snemu

**Branch**: main (project works directly on main; the user commits)
**Status**: Active

Companion to [docs/framebuffer-design.md](../../docs/framebuffer-design.md) and
[plans/framebuffer-milestone-0.md](framebuffer-milestone-0.md) (complete — real QEMU
ramfb bring-up, both itests green). Right now snemu's `fw_cfg` support is a **no-op
stub** (`snemu/src/bus.rs`, current at time of writing): reads return 0, writes are
dropped — enough that a booted kernel doesn't bus-fault, but `etc/ramfb` never exists,
so `ramfb::init()` always takes the graceful-refusal path. The
`framebuffer-presents` itest **fails** under `cargo xtask snemu-itest --only
framebuffer-presents` today — confirmed live, not assumed. That's the gap this plan
closes.

## Goal

Model `fw_cfg` + the `etc/ramfb` file for real in snemu: the directory lookup
succeeds, the DMA write completes correctly (matching the kernel's already-tested wire
contract byte-for-byte), the resulting `RamfbCfg` is captured, and its state folds into
the determinism hash like every other device. `framebuffer-presents` passes under
`snemu-itest`. A `--dump-framebuffer <path>.ppm` CLI flag gives a way to *see* the
captured pixels today, without needing the browser/canvas harness.

## Explicitly out of scope

**The browser-tab / canvas rendering path.** Per the design doc, ramfb was chosen
specifically because it's cheap to reach a `<canvas>` from — but snemu today has *zero*
wasm/canvas/postMessage groundwork (confirmed: no wasm build target beyond the JIT's
`cfg(not(target_arch = "wasm32"))` interpreter-only scoping; no `wasm-bindgen`; no JS
host shim). Bolting that onto this plan would roughly triple its scope for a payoff
this plan doesn't need — the PPM dump proves the device model is *correct* without it.
Browser rendering is its own future plan once this one lands.

## Precedent to mirror

`snemu/src/virtio.rs`'s `Virtio` device model is the template: a plain struct with
`in_window(addr) -> bool` + `read(addr) -> u32` / `write(addr, value)` as its whole
external surface, wired into `Bus` by address-range dispatch. Register writes just
update state; the actual RAM-touching work happens in an explicit method
(`service_tx(&mut self, ram: &mut Memory)`) that `Bus` calls only after detecting the
"trigger" register write (the queue-notify doorbell). A `Fwcfg`/`Ramfb` device follows
the identical shape: register writes stage state; writing the DMA address register's
**low** half (the trigger, per the real spec and the kernel's own `write_file`
sequence) calls a `complete_dma(&mut self, ram: &mut Memory)` that reads the
descriptor, does the transfer, and writes the completion status back.

## The wire contract (already pinned — no guessing this time)

Unlike the QEMU-side plan, there's no spec-reading risk here: the kernel's half is
**already built and host-tested byte-exact**, and *is* the spec this device model must
satisfy:

- `kernel-core/src/fwcfg.rs` — directory format (`find_file`, BE `u32` count + 64-byte
  BE entries), `DmaAccess` (16 bytes BE: `control(4) length(4) address(8)`),
  `DMA_CTL_SELECT=0x08`, `DMA_CTL_WRITE=0x10`, `DMA_CTL_ERROR=0x01` (the exact bug I
  found and fixed for the QEMU side — reuse these constants, don't re-derive them).
- `kernel-core/src/ramfb.rs` — `RamfbCfg` (28 bytes BE:
  `addr(8) fourcc(4) flags(4) width(4) height(4) stride(4)`), `FOURCC_XRGB8888`.
- `kernel/src/device/fwcfg.rs` — the actual register offsets driven: `REG_DATA=0x00`,
  `REG_SELECTOR=0x08` (legacy directory read), `REG_DMA_ADDR_HIGH=0x10`,
  `REG_DMA_ADDR_LOW=0x14` (DMA trigger, low-half write).

The legacy (non-DMA) path is used for the directory read (`find_file` calls
`write_selector` + a loop of `read_data_byte`); the DMA path is used only for the
`RamfbCfg` write (`write_file`). Both must work.

## Testing doctrine

snemu is ordinary host-tested Rust (unlike the kernel binary) — full RED→GREEN per
step, mirroring `virtio.rs`'s test style (instantiate the device directly, drive raw
MMIO offsets, assert). **No MUTATE gate**: `snemu` isn't in `xtask mutants`'s package
list (`collector`, `protocol`, `kernel-core`, `hitch`, `hitch-pod`, `stitch`) —
consistent with the rest of the crate, skip it here too. Compensate with deliberate
boundary-case tests (wrong select_key, DMA before directory read, truncated directory
capacity) instead of a mutation pass.

## Acceptance Criteria

- [ ] Booting a real kernel in snemu, `find_file("etc/ramfb")` succeeds (the directory
      lookup returns a real entry, not the stub's empty-directory `count=0`).
- [ ] The DMA write of `RamfbCfg` completes cleanly (`control` reads back `0`, not
      still-pending or `DMA_CTL_ERROR`) — mirrors the real hang bug's fix, this time
      caught by the device model being correct rather than the driver.
- [ ] The captured `RamfbCfg` (addr/fourcc/flags/width/height/stride) is queryable from
      `Bus`/`Machine` after a successful write, and its bytes are correct (BE-decoded).
- [ ] `Bus::hash_state` includes the fwcfg/ramfb device's state, so two snemu runs of
      the same guest program still hash identically (determinism preserved, not
      accidentally broken by adding stateful device).
- [ ] `cargo xtask snemu-itest --only framebuffer-presents` passes.
      `framebuffer-absent-degrades-gracefully` (no directory entry requested) still
      passes too — the refusal path isn't regressed.
- [ ] `--dump-framebuffer out.ppm` (or equivalent snemu CLI flag) writes a real PPM
      image of the captured framebuffer region, viewable with any image tool — visual
      proof the pixels are the right color/dimensions without a browser.

## Steps

Every step follows RED→GREEN→KILL-BOUNDARY-CASES→REFACTOR (no MUTATE — see doctrine).

### Step 1: `Fwcfg` device — file directory, legacy read path

**Acceptance criteria**: given a fresh `Fwcfg` device seeded with an `etc/ramfb` entry,
selecting `SELECTOR_FILE_DIR` (0x19) then reading sequential data-register bytes
reproduces a directory blob that `kernel_core::fwcfg::find_file` (imported directly —
snemu can depend on `kernel-core` — or a byte-identical local reimplementation) parses
back to the same `FwCfgFile { select_key, size }`. Selecting an unknown key and reading
returns `0` bytes (matches the stub's current safe-degrade behavior for anything
un-modeled).
**RED** (`snemu/src/fwcfg.rs`, new module): tests instantiate `Fwcfg::new()` directly
(mirroring `virtio.rs`'s test style) — assert `read_data_byte()` sequence after
`write_selector(0x19)` matches a hand-built expected directory blob byte-for-byte
(count header + one 64-byte entry for `etc/ramfb`), and that re-selecting resets the
read cursor.
**GREEN**: `Fwcfg` struct with a fixed internal directory (for this milestone, exactly
one file: `etc/ramfb`), `write_selector`/`read_data_byte`, an internal cursor.
**KILL BOUNDARY CASES**: reading past the directory's end (cursor overrun — should
return `0`, not panic/wrap); selecting mid-read (cursor must reset).
**Done when**: criteria met; module compiles standalone (not yet wired into `Bus`).

### Step 2: DMA write — descriptor read, payload capture, completion write-back

**Acceptance criteria**: given a `Fwcfg` device and a `&mut Memory` with a valid
16-byte `DmaAccess` descriptor staged at some PA (control = `select_key<<16 |
SELECT|WRITE`, matching `etc/ramfb`'s key, length=28, address=payload PA) and the
28-byte `RamfbCfg` payload staged at that payload PA, calling `complete_dma(&mut self,
ram: &mut Memory, desc_pa: u64)` reads both, stores the decoded `RamfbCfg` internally,
and writes `0` back into the descriptor's `control` field in `ram`. An unknown
`select_key` writes back `DMA_CTL_ERROR` instead and does *not* capture a config.
**RED**: tests build a `Memory` fixture (however `virtio.rs`'s existing RAM-touching
tests construct one — reuse that pattern), stage descriptor + payload bytes by hand,
call `complete_dma`, assert the captured `RamfbCfg` fields and the descriptor's
post-call control bytes in `ram`.
**GREEN**: `complete_dma` — decode the 16 BE bytes at `desc_pa` into `control/length/
address`, validate the key against the known `etc/ramfb` select_key, read `length`
bytes at `address`, BE-decode into `RamfbCfg` fields, store on `self`, write completion
status back to `ram` at `desc_pa`.
**KILL BOUNDARY CASES**: wrong select_key (error path, no capture); `length` not
exactly 28 (this milestone's only file — decide: reject, or accept-and-truncate?
Reject with `DMA_CTL_ERROR` — matches "unknown/malformed request" semantics and is the
conservative choice); descriptor's `WRITE` bit clear (a `READ` request — not
implemented this milestone, error out rather than silently no-op).
**Done when**: criteria met.

### Step 3: wire into `Bus` — register dispatch + the DMA trigger

**Acceptance criteria**: `Bus`'s existing fw_cfg stub is replaced by a real `Fwcfg`
field; writing `SELECTOR_FILE_DIR` to offset `0x08` then reading offset `0x00`
repeatedly returns the real directory bytes through the *bus*, not just the bare
device; writing the DMA address register's low half (offset `0x14`) through the bus
triggers `complete_dma` (mirroring how a virtio queue-notify write triggers
`service_tx` today) using the just-written 64-bit descriptor address (high half
written first, per the kernel driver's actual sequence — `kernel/src/device/fwcfg.rs`
`write_file`).
**RED** (`snemu/src/bus.rs`): replace/extend the existing stub tests (`bus.rs`'s
current fw_cfg tests, which pin the *old* no-op contract) with tests driving the same
offsets end-to-end through `Bus::read_u8`/`write_u16`/`write_u32`, asserting the real
directory + a full select+write DMA round-trip succeeds through the bus.
**GREEN**: `Bus { ram, uart, virtio, fwcfg: Fwcfg }`; `read_u8`/`write_u16` route
`0x00`/`0x08` to the device; `write_u32` at `0x10`/`0x14` stages the address halves and,
on the `0x14` (low) write, calls `self.fwcfg.complete_dma(&mut self.ram, desc_pa)` —
same "detect trigger register, call the RAM-touching method" split as virtio's notify.
**Done when**: criteria met; old stub tests either updated to the new real behavior or
removed if they specifically pinned the no-op contract (don't silently delete
coverage — confirm each old test's intent before touching it, per project convention
on not retiring distinct-path coverage).

### Step 4: fold into `Bus::hash_state`

**Acceptance criteria**: two snemu runs of the identical guest program (one that
completes the ramfb DMA write) produce identical `Machine::state_hash()` — the new
device didn't accidentally introduce nondeterminism (e.g. capturing a raw pointer,
using an uninitialized read, or depending on host-timing).
**RED**: a test booting a minimal guest program that drives the fwcfg directory read +
DMA write twice (two fresh `Machine`s or a clone-and-replay), asserting
`state_hash()` matches.
**GREEN**: extend `Bus::hash_state` to fold in the captured `RamfbCfg` fields (or
`None`) alongside the existing RAM/UART/virtio contributions.
**Done when**: criteria met.

### Step 5: `--dump-framebuffer` CLI flag — visual proof without a browser

**Acceptance criteria**: running the real kernel in snemu with
`--dump-framebuffer out.ppm` for enough steps to reach the first heartbeat present
writes a valid PPM (P6) file at `out.ppm` whose dimensions match the captured
`RamfbCfg.width/height` and whose pixel bytes match the guest-RAM region at
`RamfbCfg.addr` (converted from the kernel's stored XRGB8888 byte order to PPM's RGB
triples).
**RED**: a test (or a `snemu/src/bin` integration-style test, matching however
`main.rs`'s existing CLI is tested — check for precedent, else a pure function
`ram_to_ppm(&Memory, &RamfbCfg) -> Vec<u8>` is fully host-testable in isolation, which
is preferable — keep the CLI plumbing thin).
**GREEN**: `ram_to_ppm` (pure, testable) + a `main.rs` CLI flag that calls it after the
step loop if a `RamfbCfg` was captured, else a clear "no framebuffer captured" message
(not a silent empty file).
**Done when**: criteria met; manually open the dumped PPM once to eyeball-confirm it
looks like the expected clear color (`0x20_20_40`, dark blue-ish) — the human-in-the-
loop check this plan's automation can't fully replace.

### Step 6: prove it against the real itest

**Acceptance criteria**: `cargo xtask snemu-itest --only framebuffer-presents` passes;
`cargo xtask snemu-itest --only framebuffer-absent` (or the closest substring) still
passes, unregressed.
**Not a RED/GREEN step** — this is the integration proof the prior five steps built
toward, same role as the QEMU-side plan's Step 7 itest. No new code expected; if it
fails, the failure points back at whichever of steps 1–4 has a real bug (same
UART-print-bisection technique that found the `SELECT`/`ERROR` swap works here too —
snemu is easier to instrument than QEMU, since it's just Rust you can `dbg!()`
directly, no MMIO ceremony).
**Done when**: both scenarios pass under `snemu-itest`.

## Pre-PR Quality Gate

1. `cargo test -p snemu` — full pass.
2. Refactoring assessment (no mutation testing — see doctrine).
3. `cargo xtask clippy` clean.
4. `cargo xtask snemu-itest --only framebuffer` green (both scenarios).
5. `cargo xtask test` — confirm no regression in the real QEMU itest suite (the `Bus`
   changes are snemu-only, but the shared `kernel-core::fwcfg` types are touched by
   nothing here — should be a no-op check, worth confirming anyway).

## Out of scope (follow-up plans)

- **Browser/canvas rendering** — wasm build target, `wasm-bindgen`, JS host shim,
  `<canvas>` present loop. A real, separate plan once this one proves the device model
  correct. The PPM dump is this plan's stand-in proof.
- **virtio-input in snemu** — no input device modeled yet; irrelevant until the
  moving-rect / input milestone (framebuffer-design.md's Milestone 0 step 2) exists on
  the real-QEMU side first.
- **Multiple fw_cfg files** — this plan hardcodes exactly one directory entry
  (`etc/ramfb`). A general-purpose fw_cfg file registry is more machinery than this
  milestone needs.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
