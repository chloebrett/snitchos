# Release build exposed two latent bugs (timer death + userspace alloc-loop)

**Status:** the primary kernel bug is **fixed**; a second (userspace) bug is
**worked around** and left as a follow-up. Building the kernel `--release` — to
cut debug's per-instruction bloat across the whole itest suite — surfaced two
independent latent bugs that debug codegen had been hiding.

Reproduce fast + deterministically with **`cargo xtask snemu-itest --release`**
(the `--release` flag added for exactly this: plumbs a release profile through
`qemu::build_kernel_profiled` / `qemu::kernel_bin` / `snemu_diff::prepare_profiled`).

---

## Bug 1 — kernel `tp` truncated across the higher-half trampoline (FIXED)

### Symptom

- **snemu-release:** the kernel reaches `entering heartbeat`, then a kernel page
  fault cascades into a `.rodata` dump on the "console" (the panic/format path
  chasing corrupted pointers). Every scenario failed — 0/108.
- **QEMU-release:** boots and runs userspace, but the **timer IRQ stops firing**
  (no `kernel.heartbeat`) and the UART emits **non-UTF-8 garbage**.
- Debug: perfect. Same source, only opt-level differs — classic UB.

### Root cause

`tp` (the RISC-V thread pointer, which points at this hart's `PER_HART_DATA`
slot) was **truncated to 32 bits**: `0x00000000_8032xxxx` instead of the correct
higher-half VA `0xffffffff_8032xxxx`. The high `0xffffffff` was lost.

In `kmain`, the higher-half **trampoline** (`main.rs`, the inline-asm
`lla t0,1f; add t0,t0,off; add sp,sp,off; jr t0`) moves PC from physical to
higher-half. Under `code-model=medium`, `&static` addresses are materialized
PC-relative (`auipc`), so they resolve to **whichever half PC is currently in**.
The optimizer is free to schedule the `auipc` that computes `&PER_HART_DATA` (for
`percpu::init`'s `mv tp`) **before** the trampoline jump — where PC is still
physical — so `tp` got the physical address. Debug never hoisted it.

The bad `tp` was **dormant for ages**: `current_hartid()` range-checks `tp`
against the `PER_HART_DATA` bounds and silently returns hart 0 if it's out of
range — so a truncated `tp` just looked like "always hart 0". The recently-added
per-hart **exception-stack** asm in `trap.S` (`ld t0, 24(tp)`) reads `tp` *raw*,
with no fallback → load page fault on the first from-kernel trap (the timer) →
the fault handler (running the same path) → the garbage/rodata-dump cascade.

### Diagnosis method (worth reusing)

The fault handler couldn't format its own report (the panic path was chasing
corrupted state), so a **raw hex dump with no `core::fmt`** (`scause`/`stval`/`sepc`
via a nibble loop straight to the emergency UART) gave clean numbers →
`llvm-objdump` the `sepc` → the faulting instruction was `ld t0, 24(tp)` in
`trap_entry`, and `stval` was the truncated `tp`. Then `llvm-objdump` the two
`mv tp,…` sites showed the `auipc` for `&PER_HART_DATA` scheduled *before* the
trampoline `jr`.

### Fix

In `percpu::init`, materialize `PER_HART_DATA`'s base with a **side-effecting
`asm!("lla …")`** instead of a plain `&PER_HART_DATA[hartid]`. A non-`pure`
`asm!` block is ordered *after* the trampoline's `asm!`, so the address is
computed post-jump at higher-half PC and can't be hoisted across the trampoline.

**Result:** release snemu-itest **0/108 → 104/108**; QEMU-release boots clean to
heartbeat (verified — no timer death, no UART garbage).

---

## Bug 2 — userspace opt≥2 UB class (WORKED AROUND)

### Symptom

The 4 remaining release failures (`fs-readdir`, `fs-remove`,
`fs-lookup-rights-gate`, `spawn-reclaims-memory`) are a **userspace** codegen bug.
`build.rs` builds the embedded programs `--release` too (from the `PROFILE` env),
so it's real release codegen, not a snemu gap. The FS server (and the spawn/reap
path) enters an **unbounded allocation loop**: talc's OOM handler
(`MmapOnOom::handle_oom` → `sys_map_anon`) fires **hundreds of 68 KiB `MapAnon`s**
marching to the 16 MiB per-process heap cap → OOM → Rust alloc-error → the
userspace `#[panic_handler]`'s `loop { spin_loop() }` **hangs the process** → the
client's request never returns and the scenario times out. Debug = **1** `MapAnon`
(the program blocks idle on `Receive`).

### What we know (opt-level bisect)

- opt-level **1 = clean (108/108)**; opt-level **2 and 3 = bug present** → an
  **opt-level-2 LLVM transform**.
- Bisecting by crate (`[profile.release.package.X] opt-level = 1`, rest opt-3):
  pinning **only `snitchos-user`** (the userspace runtime crate) fixes all the FS
  scenarios. `spawn-reclaims-memory` survives that pin → **at least one more
  opt≥2 UB in another userspace crate** (spawn/reap path, likely `user/hello`). So
  it's a *class*, not a single site.
- **Kernel is unaffected** at opt-3.
- Heisenbug: a `debug_write` placed inside the readdir handler makes the flood
  vanish entirely — the corruption/UB signature (a bad length/state that
  optimization exposes and any perturbation hides). The syscall-wrapper asm
  clobbers on the readdir path (`copy_to_caller` / `reply` / `sys_map_anon`) look
  correct on inspection, so the UB is subtler. **Not yet root-caused to a line.**

### Workaround (applied)

The release-itest speedup is **kernel-dominated** — the embedded userspace is a
tiny fraction of total instret — so `kernel/build.rs` now pins the nested
userspace build to **`--config profile.release.opt-level=1`** while the kernel
keeps the workspace release opt-level (3). This sidesteps the whole userspace-UB
class at negligible speed cost.

**Result:** release snemu-itest **108/108 in 10.7s** (vs the debug suite's ~27s).

### Follow-up (open)

Root-cause the userspace opt≥2 UB(s) so the pin can be removed. Approaches:
MIRI the pure wire path (`ramfs` + `fs-proto` `encode`/`decode`); audit
`snitchos-user` for uninitialized reads / provenance / a mis-modelled asm effect;
find the second crate by pinning `user/hello` and re-bisecting `spawn-reclaims`.

---

## Why it matters

The release kernel unlocks a suite-wide itest speedup (debug's unoptimized
per-instruction bloat inflates instret everywhere — cf. the frame-oom O(n²) scan,
~70 instr/word). We now have it (~2.5× wall-clock on the snemu suite), and two
latent bugs debug was hiding are surfaced — one fixed, one contained.
