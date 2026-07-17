# 🗺️ Roadmap & milestones

*Rewritten 2026-06-21 from the current vantage point. The v0.6-era roadmap is
preserved at [roadmap-historical-through-v0.11.md](roadmap-historical-through-v0.11.md)
— useful for seeing how the plan evolved (the original v0.11 "metrics-ingestion
workload" never happened; the shell/console arc was pulled forward and v1.0
re-anchored on an interactive capability shell).*

*Milestones are narrative arcs, not time boxes. Each ships code + a devlog post (and, where it earns one, a companion video).*

# Principles
- **Granular milestones.** One coherent thing to build, understand, and explain. If it needs two screenshots to explain, it's probably two milestones.
- **Interface before implementation.** Ship the trait first with a trivial impl; richer impls are additive. (The `Filesystem` trait, the `Clock`/`FrameSink` traits, the device-class HAL traits all follow this.)
- **Effort-bounded, not calendar-bounded.** No deadlines; measure and adjust; scope cuts always acceptable.
- **Understanding is the constraint, not speed.** Use agent thinking time to learn the code, not just produce more of it.
- **Everything observable — even the cheats.** Each milestone earns a "post angle": the thing you can now *watch* in a trace.

# What "v1.0" means
v1.0 = **demoable**: a capability-secured microkernel you can actually *drive* — boot it, get a shell, run commands and edit a file over a real filesystem, with every operation observable end-to-end. It is **not** a finish line; work continues incrementally afterward. (This supersedes the old "real workload = metrics-ingestion server" framing — the interactive **shell + editor** is the nearer, more tangible "it's a real OS" moment, and the **arcade/game** is the post-v1.0 north-star real-time workload.)

# Shipped (v0.1 → v0.11)
Condensed — full detail in the [historical roadmap](roadmap-historical-through-v0.11.md) and the [README](../README.md).

- **v0.1–v0.3 ✅** — traced boot (postcard frames over virtio-console) → Grafana stack (Tempo/Prometheus/Grafana) → interrupts + SSTC timer + `Clock` trait.
- **v0.4 ✅ Memory** — Sv39 paging, higher-half kernel, bitmap frame allocator, growable kernel heap; every allocator instrumented.
- **v0.5 ✅ Threading** — cooperative round-robin scheduler; spans survive context switches (per-task `SpanCursor`); `ThreadRegister`/`ContextSwitch` frames.
- **v0.6 ✅ Cooperative SMP** — hart 1 online, per-CPU discipline, TLB-shootdown IPIs, `hart_id` on the wire; producer/consumer workload across the boundary.
- **v0.7 ✅ Userspace & capabilities** — v0.7a first userspace process (ambient, on purpose); v0.7b the capability rewrite (`CapTable`, handles, no ambient authority, `U`-bit isolation, snitched refusals).
- **v0.8 ✅ Preemption & priorities** — timer-driven preemption of userspace (`SPP==User` gate); static priorities + aging.
- **v0.9 ✅ IPC over capabilities** — synchronous endpoints, `call`/`reply` (one-shot reply caps), badged endpoints; trace context crosses the process boundary.
- **v0.10 ✅ RAMfs** — the `Filesystem` trait (`fs-core`) + `ramfs` + `fs-proto`; a File cap *is* a badged endpoint cap; bulk bytes via a kernel cross-address-space copy.
- **v0.11 🚧 Console input & spawn** — Tier-0 polled UART RX (`ConsoleRead`/`console_read`); spawn-with-caps (`Spawn` delegates exactly the caps the parent chooses). The substrate the shell stands on.

# The road to v1.0

## v0.12 — Process lifecycle: Exit / Wait (+ notifications)
The shell's blocker: a parent must launch a child and *reap* it before reading the next line. `Exit` notifies the parent; `Wait(child)` blocks until the child exits. Built on the existing `block_current`/`wake`. Introduces the **notification primitive** (async kernel→user signal) — child-exit is its first consumer, devices reuse it later. Address-space teardown/reclaim (stop leaking on exit) folds in here, or stays tax if it bloats the milestone.

**Post angle:** "the kernel learns to reap its children."

## v0.13 — The explicit-authority shell
`init` grants the shell its starting world; `user/shell` reads a line, resolves it, delegates **exactly** the caps each program needs, spawns it, waits. Commands run over the RAMfs with delegated file caps — and we invent **our own command vocabulary**, not a `cat`/`ls` clone (an identity choice: this isn't a Linux cosplay). Every delegation is an observable `CapEvent` — "watch least-authority happen." (The shell is the FS's first real consumer, so the `fs-*` end-to-end scenarios get verified here.)

**Post angle:** "a shell where you can see exactly what each command is allowed to touch."

## v0.14 — A basic text editor
A small userspace editor: open a file (FS read cap), edit a buffer (console input), write it back (FS write cap). The first interactive *app* — proves the full loop input → app → FS, capability-confined and fully traced.

**Post angle:** "editing a file on a capability OS — and watching the bytes flow."

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
- **Notifications primitive** — milestone-worthy; folded into v0.12 (child-exit/wait is its first consumer; devices reuse it).
- **Kernel stack guard pages** — its own small hardening item: Tier A (canary + high-water gauge) is cheap; guard pages are the real fault-on-overflow fix. Motivated by the v0.11 spawn stack overflow. See [plans/legacy/kernel-stack-guard-pages.md](../plans/legacy/kernel-stack-guard-pages.md).
- **FS end-to-end verification** and **Exit/teardown reclaim** — tax within v0.12/v0.13, not milestones.

# Open questions
- The v1.0 boundary (shell + editor is the current call). A compelling Tetris demo could tempt the arcade earlier, but the metal/3D risk argues for keeping it post-v1.0.
- Whether the host→kernel **control plane** (runtime knobs as `Frame` commands) shares the shell's dispatch — it should: one dispatch table, two front-ends (a human line-parser and a `Frame` decoder).
- Numbering kept contiguous (v0.12 lifecycle → v0.13 shell → v0.14 editor → v1.0); split + renumber forward if a milestone proves too big (as SMP did at v0.6).
- aarch64: deliberately **not** — staying RISC-V is the lever that keeps real hardware cheap.

# Numbering note
Milestone numbers are contiguous. Punted/unscheduled work lives in the post-v1.0 list rather than holding reserved version numbers.
