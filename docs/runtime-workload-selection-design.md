# 🎛️ Runtime workload selection

*One kernel binary. Workloads chosen at boot via kernel bootargs, not at compile time. The test scaffolding is purely additive — with no bootarg, an instrumented build runs the exact production default.*

Status: **proposed** — bootargs channel **verified by spike** (see *Feasibility gate*). Supersedes the per-workload cargo-feature scheme accumulated through v0.4–v0.6.

## The problem

The kernel has grown a pile of compile-time feature flags whose only job is to swap the boot workload:

| feature | what it does |
|---|---|
| `oom-leak` | heartbeat leaks 1024 frames/tick (frame-allocator OOM scenario) |
| `heap-oom` | heartbeat leaks heap blocks/tick (heap OOM scenario) |
| `smp-workload` | producer on hart 0, consumer on hart 1 (v0.6 cross-hart workload) |
| `deflake-spawn-storm` | hart 0 runs a serialised `spawn_on(1, …)` loop |
| `deflake-ipi-pong` | tight cross-hart IPI loop |
| `deflake-shootdown-storm` | tight `mmu::shootdown` loop |
| `deflake-mutex-storm` | two tasks hammer a shared `Mutex` |
| `deflake-virtio-storm` | hart 0 emit-storm, hart 1 atomic spin |

They all share one shape: `#[cfg]`-gate which tasks `kmain` spawns, and occasionally tweak the heartbeat. The cost of doing this at compile time:

- **N kernel builds.** Each integration scenario that needs a non-default workload triggers a *separate* `cargo build` with its feature set. The itest suite rebuilds the kernel many times per run — a direct drag on suite wall-clock (a standing concern: invest in suite speed before lowering the `--repeat` gate).
- **`#[cfg]` sprawl.** Every workload task carries a growing `#[cfg_attr(any(feature = "deflake-…", …, feature = "smp-workload"), allow(dead_code))]` attribute. Adding a workload means editing that `any(...)` list in several files.
- **Divergence risk.** A feature build is a *different binary* with a *different default path* — the cfg can silently drift what "default boot" means between builds, undermining baseline comparisons.

The flags conflate two separable decisions: **which workloads exist in the binary** (legitimately compile-time) and **which one runs** (naturally runtime).

## The decision

**Select the boot workload at runtime from the kernel command line (DTB `/chosen/bootargs`), behind a single additive `itest-workloads` umbrella feature.**

Three parts:

1. **Input channel — DTB bootargs.** QEMU `-append "workload=<name>"` populates the device tree's `chosen/bootargs`. `kmain` reads it (the `fdt` crate is already a dependency and the DTB is already parsed in `kmain`), parses a `workload=` key, and dispatches. Selecting a workload is `-append`, not a rebuild.

2. **Compile-time umbrella — `itest-workloads`, additive only.** The whole registry (the alternate workloads, the bootargs parse, the runtime dispatch) is gated behind one feature. When the feature is **off** (the default `cargo xtask boot`, any future production build), none of it compiles in and `kmain` runs the standard demo directly. When **on**, the registry is *added* — but with **no bootarg, the kernel still runs the exact same standard demo.** The feature never changes the default-selected path; it only adds selectable alternatives.

3. **Runtime selection is exclusive.** Additive at compile time, one-of at runtime: picking `workload=smp` runs *that instead of* the default demo (and `workload=deflake-spawn-storm` strips `task_a`/`task_b` for its clean measurement surface, exactly as the cfg does today). Modelled as `Option<WorkloadKind>` — `None` (no/unknown bootarg) means "run the default demo."

The itest harness **always builds with `itest-workloads`**, so the entire suite shares **one** kernel binary; each scenario selects its workload with `-append`.

## Why bootargs (alternatives considered)

| approach | verdict |
|---|---|
| **DTB `/chosen/bootargs` + `-append`** | **Chosen.** Reuses the `fdt` dep and the existing `kmain` DTB parse. Standard real-OS mechanism (Linux `init=`, seL4) — teachable, fits the learning-project ethos. One ELF, runtime selection. |
| QEMU `fw_cfg` device | More flexible (structured blobs, not just a string) but more MMIO plumbing for no benefit at "pick a workload" granularity. Reach for it only if config outgrows a flat string. |
| Host→guest control channel (virtio RX / 2nd serial) | We have telemetry *TX* only; this needs a new RX path. Powerful (interactive "start workload X now") but it's a v0.8-IPC-era capability, not a config mechanism. Over-built for this. |
| Status quo (compile-time features) | The thing we're replacing. Keep *only* for what must not ship — and the umbrella feature already covers that. |

## Internal design

- **`WorkloadKind` enum** in `kernel-core` (pure data, alongside scause decoding / the frame bitmap).
- **`select(bootargs: &str) -> Option<WorkloadKind>`** in `kernel-core` — pure, host-tested via `cargo test`, TDD'd. Parses the `workload=` token; `None` on absent/unknown. This is the testable seam; the wiring around it stays in `kernel/`.
- **`static SELECTED: Once<WorkloadKind>`** (via `kernel::sync::Once`) — set once in `kmain` from the parsed bootargs, read by both dispatch sites.
- **Two dispatch sites, one cfg each** (replacing scattered `any(...)` gates):
  - `kmain`: `run_default_demo()` is one function. Feature-off calls it directly. Feature-on calls `select(...)` and dispatches; the `None` arm calls the *same* `run_default_demo()` — that identity is the additive guarantee made concrete.
  - heartbeat: matches `SELECTED` for the workloads that change per-tick behaviour (`oom-leak`, `heap-oom`); every other selection uses the default smoke.

The DTB read must happen **before `unmap_identity`** — the DTB physical region lives in the identity gigapage that gets torn down, and the `dtb` borrow in `kmain` is already live at the spawn site, dropped just before teardown. The read slots in naturally where the spawns already are.

## Harness change

- `Harness::spawn(label)` and the new `Harness::spawn_with_workload(label, "smp")` both use the **one** `itest-workloads` build (built once, reused). The latter passes `-append "workload=smp"`; the former passes nothing → standard demo.
- `spawn_with_features` stays only if some build genuinely needs different *compile-time* config (none does today once the 8 workload features collapse).
- `cargo xtask boot` grows a `--workload <name>` ergonomic flag (sets `-append`, implies `itest-workloads`) so live measurement / demos are `cargo xtask boot --workload smp` — no rebuild, straight into `cargo xtask reader` or Grafana.

## Consequences

**Wins**
- **One kernel build for the whole itest suite** — removes the per-variant rebuilds; the largest single lever on suite wall-clock.
- **Default path is byte-identical logic across production and itest builds** (additive guarantee) — baselines can't drift via cfg.
- **8 features → 1**; the `any(feature = …)` dead-code sprawl collapses to a single `#[cfg(feature = "itest-workloads")]` on the registry.
- Live measurement/demo of any workload without a rebuild.

**Costs / caveats (named, accepted)**
- **The itest binary is not a hypothetical lean binary.** Compiling the registry in changes layout/codegen. Accepted because there is no separate production deploy yet (the kernel *is* the artifact). Historical note: an earlier draft worried this would distort the `*-storm` scenarios because they characterised an unfixed cross-hart race. That race was found and fixed — a dropped `MutexGuard` in `virtio_console::send` (a logic bug, not memory ordering), so the storms are now ordinary regression/stress tests with no special codegen sensitivity. A flake-rate sanity check after migrating them is still prudent, but they need no special porting order.
- **Destructive workloads ship in the itest kernel.** `heap-oom`/`oom-leak` deliberately exhaust/crash. Harmless under runtime gating — each scenario is its own QEMU and they only fire when selected — but worth stating so nobody is surprised the OOM code is present.
- **Loss of dead-code elimination when the feature is on.** By design; the feature-off build stays lean, which is the build that would ever ship.

## Feasibility gate — ✅ verified

The one load-bearing unknown was whether `-append` reaches `/chosen/bootargs` with our `-kernel <ELF>` + `virt` setup, and whether it's readable in `kmain` before `unmap_identity`. **Confirmed by spike:** a throwaway `dtb.chosen().bootargs()` read in `kmain` (placed right after `console::init`, well before `unmap_identity`) printed `bootargs = "workload=smp-spike"` for `-append "workload=smp-spike"`. The `fdt` 0.1.5 `chosen().bootargs()` accessor works post-MMU here; no closure-chain crash (that gotcha is pre-MMU only). The `fw_cfg` fallback (alternative 2) is therefore not needed; if it ever were, the internal design (enum + `select` + `Once` + two dispatch sites) is unchanged — only the input channel swaps.

## Migration sequence (incremental, each step green)

1. ~~**Spike** the bootargs read~~ — done (feasibility gate above).
2. ~~Add `itest-workloads` + `WorkloadKind` + `select()` + the `kmain` dispatch, registering `smp` only; migrate `smp-producer-consumer-correctness` to `-append workload=smp`.~~ **Done** — `kernel_core::bootargs::{WorkloadKind, select}` (TDD'd, mutants clean), `kmain` selects via `dtb.chosen().bootargs()`, `smp-workload` cargo feature deleted, scenario green 10/10 via `Harness::spawn_with_workload("smp")`. *(The `kmain` dispatch reads the selection into a local rather than a `Once` — only `kmain` needs it today; revisit `Once` if the heartbeat needs it in step 3.)* **Deferred sub-step:** the suite still rebuilds per `itest-workloads` scenario; flipping the up-front build to `itest-workloads` so the whole suite shares one binary is the build-dedup win, best done once more workloads are ported.
3. ~~Port `oom-leak`, `heap-oom`. Delete their feature defs.~~ **Done** — `WorkloadKind::{FrameOom, HeapOom}` (TDD'd, mutants clean); the selection now lives in `kernel::boot_workload` (`Once<Option<WorkloadKind>>`, set unconditionally in `kmain`) so the heartbeat's `frame_smoke`/`heap_smoke_pattern` read it at runtime instead of via `#[cfg]`. `oom-leak`/`heap-oom` cargo features deleted, `frame::free`'s oom-leak dead-code `expect` removed; scenarios select `workload=frame-oom` / `workload=heap-oom`. Both green; default smoke path unaffected.
4. Port the `*-storm` workloads (any order — the race they once characterised is fixed). **Rename here:** they become runtime selections `workload=mutex-storm`, `workload=spawn-storm`, etc. — the now-misleading `deflake-` prefix disappears as the cargo features are deleted, so no separate rename-then-delete churn. A flake-rate sanity check after the move is prudent but not gating.
5. Add `cargo xtask boot --workload`.

## Open questions

- **Naming.** `itest-workloads` is accurate (itest is the always-on consumer), but `smp-workload` is also the *post/demo* workload, not only a test. `selectable-workloads` / `workload-registry` are alternatives. Leaning `itest-workloads` per the "additive scaffolding the tests ship with" framing.
- **Should `boot --workload` auto-imply the feature?** Probably yes for ergonomics; a bare `--workload` on a feature-off build should error clearly rather than silently run the default.
- **Do any `deflake-*` storms need boot conditions a bootarg can't express?** If a storm needs something set *before* the spawn dispatch runs, confirm the bootarg is parsed early enough (it is, at the spawn site) — flagged for the step-4 port.

---
*Companion to `plans/v0.6-smp-cooperative.md`. When this is built, fold the outcome into CLAUDE.md's workload/itest sections and delete the obsolete feature documentation from `kernel/Cargo.toml`.*
