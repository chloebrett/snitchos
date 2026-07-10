# Release build exposes a latent kernel bug (timer death + UART corruption)

**Status:** open bug, deferred. Found while exploring a release-build kernel for
faster itests (would cut debug's per-instruction bloat across the whole suite).

## Symptom

Build the kernel with `--release` (optimized) and run the itests:

- **Under snemu**: the kernel effectively doesn't boot — the decoded "console" is
  a contiguous dump of the kernel's `.rodata` string table (workload names,
  DTB-parser messages, `linked_list_allocator` errors like *"Freed node aliases
  existing hole! Bad free?"*, `BTreeMap` internal assertions). Every scenario
  fails; even `boot-reaches-heartbeat`.
- **Under QEMU** (the real-hardware model, so this is the load-bearing evidence):
  the kernel **boots and runs userspace** — real telemetry flows (`fs-client`
  SpanStart / StringRegister / MetricRegister on hart 1) — **but the timer IRQ
  stops firing** (`no kernel.heartbeat SpanStart within 30s`), and the UART log is
  **non-UTF-8 garbage**.

Debug build: everything passes. Same source, only the optimization level differs.

## Diagnosis

Because it reproduces on **QEMU**, this is a **latent kernel bug that release
codegen surfaces — not a snemu fidelity gap.** The signature (works in debug,
timer dies + UART garbage only when optimized) is classic UB: an uninitialized
value, a missing `volatile` on an MMIO/CSR access, a data race, or a memory-layout
assumption that debug's zeroing/stack-spilling accidentally satisfies. Matches the
project's own heuristic: *varying crash modes surfacing only under a codegen change
= corruption/UB, suspect it before instrumenting.*

The heap-allocator error strings in the `.rodata` dump (`linked_list_allocator`)
are a hint but **not** proof they fired — they're embedded constants the corrupted
console-read happened to walk over. The load-bearing facts are **timer IRQ death**
and **UART garbage** under QEMU-release.

## Where to look (when hunting)

- The timer path: `trap::init_timer`, `stimecmp`/Sstc arming, the trap handler's
  `sstatus.SIE`/`sie.STIE` state. A missing `volatile`/`compiler_fence` around CSR
  or MMIO writes is the prime suspect — debug doesn't reorder/elide, release does.
- The UART (`ns16550a`) MMIO writes — same missing-`volatile` family.
- Anything relying on incidental zero-init or a specific stack layout.
- Bisect optimization level: `opt-level = 1/2/3`, and `-C no-...` flags, to find the
  transform that triggers it.

## Why it matters

Two payoffs when fixed: (1) a release kernel unlocks a large, suite-wide itest
speedup (debug's unoptimized per-instruction bloat inflates instret everywhere —
see the frame-oom O(n²) finding, ~70 instr per scan iteration), and (2) a
latent-UB bug that only debug hides is a real robustness risk for the kernel
itself.
