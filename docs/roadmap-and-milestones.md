# 🗺️ Roadmap & milestones

*Rewritten 2026-06-21; **shipped list and road section refreshed 2026-08-06**, when
a stock-take found this doc two milestones behind reality. The v0.6-era roadmap is
preserved at [roadmap-historical-through-v0.11.md](roadmap-historical-through-v0.11.md)
— useful for seeing how the plan evolved (the original v0.11 "metrics-ingestion
workload" never happened; the shell/console arc was pulled forward and v1.0
re-anchored on an interactive capability shell).*

> **Read this doc's numbering with suspicion.** Since v0.11 most of the project's
> output has arrived as *parallel tracks* — the language, the emulator, the model
> ladder, the board port, audio — none of which take version numbers. A contiguous
> `v0.x` list is a poor index of where the work actually is. `plans/` status headers
> and the [debt register](debt-register.md) are the truer picture.

*Milestones are narrative arcs, not time boxes. Each ships code + a devlog post (and, where it earns one, a companion video).*

# Principles
- **Granular milestones.** One coherent thing to build, understand, and explain. If it needs two screenshots to explain, it's probably two milestones.
- **Interface before implementation.** Ship the trait first with a trivial impl; richer impls are additive. (The `Filesystem` trait, the `Clock`/`FrameSink` traits, the device-class HAL traits all follow this.)
- **Effort-bounded, not calendar-bounded.** No deadlines; measure and adjust; scope cuts always acceptable.
- **Understanding is the constraint, not speed.** Use agent thinking time to learn the code, not just produce more of it.
- **Everything observable — even the cheats.** Each milestone earns a "post angle": the thing you can now *watch* in a trace.

# What "v1.0" means
v1.0 = **demoable**: a capability-secured microkernel you can actually *drive* — boot it, get a shell, run commands and edit a file over a real filesystem, with every operation observable end-to-end. It is **not** a finish line; work continues incrementally afterward. (This supersedes the old "real workload = metrics-ingestion server" framing — the interactive **shell + editor** is the nearer, more tangible "it's a real OS" moment, and the **arcade/game** is the post-v1.0 north-star real-time workload.)

# Shipped (v0.1 → v0.13)
Condensed — full detail in the [historical roadmap](roadmap-historical-through-v0.11.md) and the [README](../README.md).

- **v0.1–v0.3 ✅** — traced boot (postcard frames over virtio-console) → Grafana stack (Tempo/Prometheus/Grafana) → interrupts + SSTC timer + `Clock` trait.
- **v0.4 ✅ Memory** — Sv39 paging, higher-half kernel, bitmap frame allocator, growable kernel heap; every allocator instrumented.
- **v0.5 ✅ Threading** — cooperative round-robin scheduler; spans survive context switches (per-task `SpanCursor`); `ThreadRegister`/`ContextSwitch` frames.
- **v0.6 ✅ Cooperative SMP** — hart 1 online, per-CPU discipline, TLB-shootdown IPIs, `hart_id` on the wire; producer/consumer workload across the boundary.
- **v0.7 ✅ Userspace & capabilities** — v0.7a first userspace process (ambient, on purpose); v0.7b the capability rewrite (`CapTable`, handles, no ambient authority, `U`-bit isolation, snitched refusals).
- **v0.8 ✅ Preemption & priorities** — timer-driven preemption of userspace (`SPP==User` gate); static priorities + aging.
- **v0.9 ✅ IPC over capabilities** — synchronous endpoints, `call`/`reply` (one-shot reply caps), badged endpoints; trace context crosses the process boundary.
- **v0.10 ✅ RAMfs** — the `Filesystem` trait (`fs-core`) + `ramfs` + `fs-proto`; a File cap *is* a badged endpoint cap; bulk bytes via a kernel cross-address-space copy.
- **v0.11 ✅ Console input & spawn** — Tier-0 polled UART RX (`ConsoleRead`/`console_read`); spawn-with-caps (`Spawn` delegates exactly the caps the parent chooses). The substrate the shell stands on.
- **v0.12 ✅ Process lifecycle** — `Exit`/`Wait`/`WaitAny` + reaping, address-space teardown, and the **notification primitive** (async kernel→user signal); child-exit was its first consumer, `glitch`'s ring and the timer wheel reuse the shape.
- **v0.13 ✅ `init` bootstrap + the cap-id spine** — `init` is the userspace delegation root and the **default boot** (the old kernel scheduler demo is now `workload=demo`). It `EndpointCreate`s its own endpoint, spawns the FS server and a client with a minted bare `SEND`, and supervises via `WaitAny`. Every *holding* carries a global `cap_id` with a `parent_cap_id` link, so `CapEvent::Transferred` frames reconstruct the derivation tree.

**Landed alongside, not on the version ladder** — these were milestone-sized but arrived as parallel tracks:
- **Capability revocation** — transitive `Revoke` (=28) by handle, `CapEvent::Revoked`, the cross-table derivation-tree sweep, and `user/hello/src/bin/shell.rs` closing delegate → use → reclaim as observable `CapEvent`s.
- **Telemetry off the board** — COBS wire format, `UartFrameSink` + the full PLIC/THRE interrupt path (M2), and UDP transport over virtio-net (M2.5). See [../plans/uart-telemetry.md](../plans/uart-telemetry.md) and [../plans/network-telemetry.md](../plans/network-telemetry.md).
- **VisionFive 2 first light** — boots on real hardware, four U74s, userspace, heartbeat ([../plans/visionfive2-port.md](../plans/visionfive2-port.md)).
- **Userspace floating point**, including FP context switching, and **`glitch`** — the DAC as a userspace-held capability, plus the async sample ring and the kernel's first real-time deadline ([../plans/glitch-v2-async-ring.md](../plans/glitch-v2-async-ring.md)).

# The road to v1.0

The two v1.0 pillars — a shell and an editor — both exist in substance, but neither
arrived the way the milestones below predicted, so what's left is *consolidation*
rather than construction.

## The shell — substantially shipped, never formally closed
`user/hello/src/bin/shell.rs` reads a line, delegates exactly the caps a program needs, spawns it, waits, and revokes — the full grant → use → reclaim cycle, observable as `CapEvent`s. What the original milestone described and this does not yet have: **our own command vocabulary** over the RAMfs (the identity choice — not a `cat`/`ls` clone), and the `fs-*` end-to-end scenarios verified through it.

**Post angle:** "a shell where you can see exactly what each command is allowed to touch."

## The editor — `stim`, arriving from the language side
The editor turned out to be [`stim`](stim-design.md), a modal editor written **as a Stitch program** rather than as a Rust userspace app — a much more interesting result than the milestone asked for, and one that ships in the ramfs image. Tracked at [../plans/stim-v1.md](../plans/stim-v1.md); its remaining work is language-side, not kernel-side.

**Post angle:** "editing a file on a capability OS — and watching the bytes flow."

## What v1.0 actually needs now
Not new subsystems — the demo path exists end to end. The gap is that no single
run walks it: boot → shell → run commands over a real FS → edit a file → and have
every step legible in a trace. Naming and closing that walk is the remaining work.

## v1.0 — Demoable
The story stands on its own: boot → shell → run commands + edit a file over a real FS, capability-secured, every operation observable. Polish, a coherent demo, a series wrap — then keep building.

**Post angle:** "SnitchOS v1.0 — a capability OS you can actually drive, that snitches on itself."

# Post-v1.0 — the north-stars (loosely sequenced)

## The arcade — the observable real-time workload
The headline post-v1.0 arc; full design in [arcade-and-real-hardware-direction.md](arcade-and-real-hardware-direction.md). A game is the *best* observability workload — frame deadlines, input→photon latency, audio underruns, netcode jitter are legible real-time requirements a CRUD server lacks. **Guardrail:** the arcade is the *showpiece workload for the observable OS*, not a pivot to building a game console. Sequence:
- `Framebuffer` + `Input` device-class traits (virtio-gpu-2D / ramfb + console input) + a fixed-timestep game loop.
- **Tetris** (zero art — the platform-prover): frame-time + input-latency spans in Grafana.
- **Slay-the-Spire port** (first real userspace *app*; sprite/atlas pipeline; OS-owned RNG/time → deterministic, tamper-evident replay).
- **Software-3D Minecraft** (CPU rasterizer / voxel — the one genuinely novel subsystem).
- Novel capability-OS game primitives: sessions-as-caps, observable multi-tenancy / dynamic split-screen, untrusted-games-run-safely, record-and-replay leaderboards, debug-vision overlay, synesthetic kernel.

## Real hardware — VisionFive 2 (RISC-V, no arch port)
Stay RV64GC — the decisive lever is **not** porting to aarch64. HAL device-class traits + DTB discovery hardened in QEMU first; the board is an additive driver-port phase (SPI panel + GPIO/bridge input). See the arcade doc §2–3.

## Networking — smoltcp over virtio-net / dwmac
The IP stack reuses cleanly (smoltcp, no_std); a raw-TCP "network REPL" is the cheap interaction path; multiplayer rides this. (No SSH — it overshoots into std+tokio.)

## WASM — SnitchOS in a browser tab
The portability payoff: the unmodified kernel in a wasm RISC-V emulator (ports the *guarantees*) and/or the portable upper half compiled to `wasm32` (ports the *experience*); shared sessions over a relay. "Click → SnitchOS boots in your tab," and the portfolio-homepage showpiece.

## Stitch — a managed language on the capability OS
[Stitch](language-design.md) (Java-shaped, tree-walk → bytecode VM, generational GC, caps + telemetry as the novelty) running as a SnitchOS userspace component. A post-v1.0 milestone; currently progressing as a parallel side-project.

## FS deepening · audio · two-tier scheduler · WASM-userspace
CoW + snapshots → content-addressed + Merkle ("filesystem-as-Git"), additive behind the v0.10 trait. Over-engineered audio (RT deadlines, XRun forensics). Borg-style two-tier scheduler. WASM-*userspace* — SnitchOS *hosts* wasm apps (the inverse of "SnitchOS in a tab").

# Hardening (some milestones, some tax)
- **Notifications primitive** ✅ — shipped in v0.12 (child-exit/wait was its first consumer; the audio ring reuses the shape).
- **Kernel stack guard pages** ✅ — Tier A (canary + high-water gauge) and Tier B (guard pages) both shipped; deep-overflow reporting and the boot stack are deferred. See [plans/legacy/kernel-stack-guard-pages.md](../plans/legacy/kernel-stack-guard-pages.md).
- **Exit/teardown reclaim** ✅ — shipped in v0.12; the `spawn-reclaims-memory` itest guards it.
- **FS end-to-end verification** — still outstanding, and now the clearest piece of remaining v1.0 tax.
- **A dead server should refuse its clients, not hang them** — nothing refuses a call on an endpoint whose only receiver has exited. Three separate arcs have hit this, and it is the one place in the codebase where a failure is silent, against the project's own loudest rule.

# Open questions
- The v1.0 boundary (shell + editor is the current call). A compelling Tetris demo could tempt the arcade earlier, but the metal/3D risk argues for keeping it post-v1.0.
- Whether the host→kernel **control plane** (runtime knobs as `Frame` commands) shares the shell's dispatch — it should: one dispatch table, two front-ends (a human line-parser and a `Frame` decoder).
- **Whether contiguous version numbers still earn their place.** v0.12 and v0.13 landed as numbered milestones; revocation, the board port, audio, FP and the whole language/model side did not, and are no less real. Either the parallel tracks get folded into the numbering or the numbering stops pretending to be the index.
- aarch64: deliberately **not** — staying RISC-V is the lever that keeps real hardware cheap.

# Numbering note
Milestone numbers are contiguous. Punted/unscheduled work lives in the post-v1.0 list rather than holding reserved version numbers.
