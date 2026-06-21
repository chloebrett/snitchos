# 🗺️ Roadmap & milestones — HISTORICAL SNAPSHOT (v0.6-era)

> **This is a preserved historical roadmap, superseded on 2026-06-21 by
> [roadmap-and-milestones.md](roadmap-and-milestones.md).** Kept as a record of
> how the plan looked from the v0.6/v0.9 vantage point — useful for seeing what
> changed and why (notably: the v0.11 "metrics-ingestion workload" never happened;
> the project pulled the shell/console arc forward and re-anchored v1.0 on an
> interactive capability shell + a basic editor, with the arcade as the post-v1.0
> north-star). Don't treat anything below as the current plan.

*Milestones are narrative arcs, not time boxes. Each one ships code, a blog post, and a companion video.*

# Principles
- **Granular milestones.** ~20 small milestones beats 9 big ones. Each milestone is one coherent thing to build, understand, and explain. If a milestone needs two screenshots to explain, it should probably be two milestones.
- **Each milestone produces content.** A devlog+essay blog post and a companion video (No Boilerplate / ThePrimeagen style — tight script, slideshow + voiceover, Excalidraw diagrams). Content can pipeline up to ~1 milestone behind code, no further.
- **Interface before implementation.** Interfaces are expensive to change; implementations are cheap. Ship the trait first with a trivial implementation; richer implementations are additive later.
- **Effort-bounded, not calendar-bounded.** No deadlines. Measure and adjust. Scope cuts are always acceptable.
- **Speed is not the constraint; understanding is.** Reference point: a 15k-line well-factored tested Slay the Spire clone built in one week with agent collaboration. SnitchOS is harder (compounding decisions, slow QEMU iteration loop, less-trodden terrain for the agent) but the same throughput applies once past the v0.1–v0.3 infrastructure tax. The risk is running ahead of understanding — use agent thinking time to learn the code, not just to produce more of it.

# Road to v1.0
The v1.0 story: *a capability-secured microkernel running a real workload, where every operation is observable end-to-end.* Audio, networking, WASM, and FS-deepening are deliberately post-v1.0 — v1.0 is already a complete story without them.

**Status legend:** ✅ shipped (code + post) · 🚧 in progress · (unmarked) not started. As of 2026-06-14: v0.1→v0.8 shipped, v0.9 (IPC) almost done. Capabilities (v0.7b), the full userspace runtime, preemption (v0.8), and capability-based IPC (v0.9, landing) exist *today* — see `kernel-core/src/cap.rs`, `user/`. Don't read the future-tense prose below as "not yet built"; trust the marker.

## v0.1 — Hello, traced world ✅
Smallest kernel that boots on RISC-V in QEMU and emits boot-phase spans + a heartbeat loop as postcard frames over a dedicated serial channel. Host-side reader pretty-prints a live span tree to stdout. No userspace, no allocator, no interrupts.

**Post angle:** "Hello world, but the world snitches." Screen recording of the live span tree.

## v0.2 — Grafana arrives ✅
Replace the stdout reader with a real collector daemon. docker-compose stack: Tempo (traces), Prometheus (metrics), Grafana, optionally Loki. Add structured metrics (counters, gauges) alongside spans. Same heartbeat workload.

**Post angle:** "From printf to a real observability stack." First dashboard screenshot — the "this looks like a product" reveal.

## v0.3 — Interrupts & clock ✅
Trap handler, timer interrupts, a monotonic clock behind a `Clock` trait. The heartbeat becomes timer-driven instead of a busy loop. Trap entry/exit traced.

**Post angle:** "Teaching the kernel what time it is." Trace view showing periodic timer-driven spans.

## v0.4 — Memory ✅
Page table setup, higher-half kernel layout, physical frame allocator, kernel heap. All allocators instrumented — allocation/free as metrics, heap pressure visible in Grafana.

**Post angle:** "Bootstrapping allocators before allocators exist." Grafana panel of live heap usage.

## v0.5 — Threading & round-robin scheduler ✅
Multiple kernel threads, context switching, the simplest possible scheduler (round-robin, single queue, single CPU). Span context propagates across context switches — the first genuinely hard observability problem. Scheduler decisions traced.

**Post angle:** "Following a trace across a context switch." Multi-thread trace view.

## v0.6 — SMP (cooperative) ✅
Second hart online, per-CPU discipline made real, page-table mutation extended with TLB-shootdown IPIs, wire format carries `hart_id`. **Cooperative**, not preemptive — each hart runs its own runqueue, tasks `yield_now()` voluntarily, idle harts `wfi` and wake on IPI. Headline workload: a producer/consumer histogram that first runs cooperative single-core (baseline post), then on two harts with `Mutex<VecDeque>` (the chokepoint shows its cost), then with `heapless::spsc` (the chokepoint goes away).

**Why here, not later:** late SMP is an *unbounded audit* across every global and every "no-one-else-is-here" assumption — silent bugs under weak memory ordering. Late capabilities, by contrast, is a *scoped refactor* (rewrite the syscall layer). The v0.5 sync/percpu prefactor positioned this exactly; the audit surface is still small and enumerable today. Capabilities and IPC are the first concurrency-shaped subsystems — designing them post-SMP means they're born multi-hart-correct.

**Post angle:** three posts from one milestone — "two heartbeats on two harts," "what the chokepoint cost me," "what removing it bought me." See `plans/legacy/v0.6-smp-cooperative.md`.

## v0.7a — First userspace process (built deliberately wrong) ✅
User-mode entry, the first userspace process, exactly one syscall — with ambient authority, no capability discipline. Built intentionally the "Unix way" so the next milestone can feel the pain.

**Post angle:** "The first userspace process — and why I built it wrong on purpose."

## v0.7b — Capability system ✅
Introduce capabilities as the only access path. Refactor v0.7a's syscall to be capability-mediated. Per-process capability tables, capabilities as kernel objects, root caps to init only. Kernel begins adopting caps internally where it makes sense. Designed against the v0.6 multi-hart substrate from the start — no SMP retrofit later.

**Post angle:** "Why I rewrote the syscall layer: ambient authority vs. capabilities." The project's identity crystallizes here — a strong essay milestone.

## v0.8 — Preemption, priorities, time-sliced scheduler ✅
Cooperative becomes preemptive. Timer-driven preemption with full-trap-frame context switch (today's cooperative `switch` elides caller-saved regs per SysV ABI and can't survive mid-instruction interrupts). The `kernel::sync` chokepoint absorbs preempt-disable hooks in one file. Static priorities and time slicing layer on top. Borg-style two-tier (latency-sensitive + batch) is deferred further out.

**Why before IPC:** the two milestones are mutually unblocked — IPC's blocking paths go through voluntary `yield_now` (a normal call; caller-saved regs already dead per SysV ABI), so the full-trap-frame switch never invalidates them, and preemption needs nothing from IPC. Doing preemption first means IPC's eventual block/wake is born on the preemptive scheduler (the better substrate) rather than retrofitted onto it, and preemption's races land on a single-process system instead of colliding with brand-new endpoint state. (Swapped with v0.9 — was IPC-first.)

**Post angle:** "Making the scheduler take the CPU back."

## v0.9 — IPC over capabilities 🚧
Synchronous capability-based channels. First two-process workload. IPC paths fully traced — spans cross the process boundary. Notifications as a separate primitive (roughly the Zircon model). Endpoint state designed multi-hart-correct from day one. Block/wake sits on the v0.8 preemptive scheduler.

**Post angle:** "Two processes talking, and watching every word."

## v0.10 — Minimal RAMfs behind a stable Filesystem trait
A RAM-backed filesystem as a userspace component, accessed via capabilities. **The `Filesystem` trait is the deliverable** — `open / read / write / stat` etc. — with a trivial in-memory implementation behind it. Not persistent, no snapshots. CoW and content-addressing are additive later behind this same trait.

**Post angle:** "A filesystem in userspace — and the interface that lets it grow."

## v0.11 — Metrics-ingestion workload, end-to-end
The first real workload: a personal metrics ingestion server running as a userspace SnitchOS component. Laptop/phone push metrics in; SnitchOS stores them; they're served back out to Grafana. Network ingress via the host-bridge cheat (host shim forwards data in over the existing channel — no in-kernel network stack yet). The "OS is real" moment: observability now observes something real instead of trivia.

**Post angle:** "SnitchOS does an actual job now." End-to-end demo: real data flowing through a traced, capability-secured microkernel.

## v1.0 — Story complete
Polish, hardening, a coherent demo, and a wrap-up of the blog/video series. The complete v1.0 story stands on its own: capability-secured microkernel, real workload, total observability.

**Post angle:** "SnitchOS v1.0 — what it is, what I learned, what's next."

# Post-v1.0 (sequenced loosely)
- **v1.1 — Audio.** Over-engineered audio subsystem: sub-millisecond deadlines as a first-class concern, RT scheduling, audio-specific observability (per-buffer traces, XRun forensics, latency histograms). Its own multi-post arc. Optimize hard for demo interestingness here.
- **v1.2 — Real network stack.** smoltcp as a capability-isolated userspace component over virtio-net. Replaces the host-bridge cheat. Every packet a span; watch TCP slow-start in Grafana.
- **virtio RX → kernel shell → telemetry control plane.** Build the device-*writeable* receive path on the virtio-console: pre-posted buffers, `DESC_F_WRITE`, and **interrupt-driven** completion — the RX dual of today's polled TX (retires `transmit`'s "polling, not interrupt-driven" weakness). First consumer is a minimal kernel REPL (`ps`, `meminfo`, `tasks`) over the console; then a host→kernel `Frame` control plane reusing the *same* command dispatch — the collector pushes runtime knobs (trace verbosity, workload select, heartbeat interval). One dispatch table, two front-ends: a human line-parser and a `Frame` decoder. The hard part (RX + IRQ virtqueue) is shared; REPL vs control-plane is just what sits on top. Strong candidate to pull in pre-v1.0 — the interactive shell is a cheap, demoable first consumer, and the control plane is squarely on the observability thesis. Post-caps, the control plane becomes a capability-mediated surface. **Post angle:** "the kernel starts taking orders from its observer."
- **FS deepening — CoW + snapshots, then content-addressed + Merkle.** Additive behind the v0.9 `Filesystem` trait. Filesystem-as-Git. "The day SnitchOS learned to remember things."
- **Borg-style two-tier scheduler.** Latency-sensitive + batch tiers, SLO-driven scheduling, counterfactual analysis.
- **WASM userspace.** WASM as a universal portable runtime; capabilities map onto WASM imports.
- **aarch64 port.** Forces the HAL to actually be correct. "SnitchOS now runs on a Raspberry Pi." Timing unscheduled — slot when the HAL is mature.

# Numbering note
Milestone numbers are contiguous (v0.1…v0.11, v1.0). Punted/unscheduled work lives in the post-v1.0 list rather than holding reserved version numbers. If a milestone turns out too big mid-flight, split it and renumber forward — as happened when SMP was inserted at v0.6 (pushing the original v0.6a/v0.6b → v0.7a/v0.7b and everything downstream forward by one).

# Open questions
- Exact audio insertion point — v1.1 is the current plan, but there are factors pushing it both earlier and later. Tracked on the Audio sub-page.
- Whether to pull the **virtio RX → kernel shell → control plane** work in pre-v1.0 (currently in the post-v1.0 list). The RX+IRQ virtqueue path is useful infrastructure regardless, the REPL is a tangible demo, and the control plane is on-thesis — but a host→kernel command surface arguably wants the v0.7b capability layer underneath it first.
- Whether to interleave a non-FS milestone between v0.9 and the FS-deepening work to avoid an FS-heavy stretch.
- aarch64 timing.
