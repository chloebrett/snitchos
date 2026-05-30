# 🗺️ Roadmap & milestones

*Milestones are narrative arcs, not time boxes. Each one ships code, a blog post, and a companion video.*

# Principles
- **Granular milestones.** ~20 small milestones beats 9 big ones. Each milestone is one coherent thing to build, understand, and explain. If a milestone needs two screenshots to explain, it should probably be two milestones.
- **Each milestone produces content.** A devlog+essay blog post and a companion video (No Boilerplate / ThePrimeagen style — tight script, slideshow + voiceover, Excalidraw diagrams). Content can pipeline up to ~1 milestone behind code, no further.
- **Interface before implementation.** Interfaces are expensive to change; implementations are cheap. Ship the trait first with a trivial implementation; richer implementations are additive later.
- **Effort-bounded, not calendar-bounded.** No deadlines. Measure and adjust. Scope cuts are always acceptable.
- **Speed is not the constraint; understanding is.** Reference point: a 15k-line well-factored tested Slay the Spire clone built in one week with agent collaboration. SnitchOS is harder (compounding decisions, slow QEMU iteration loop, less-trodden terrain for the agent) but the same throughput applies once past the v0.1–v0.3 infrastructure tax. The risk is running ahead of understanding — use agent thinking time to learn the code, not just to produce more of it.

# Road to v1.0
The v1.0 story: *a capability-secured microkernel running a real workload, where every operation is observable end-to-end.* Audio, networking, WASM, and FS-deepening are deliberately post-v1.0 — v1.0 is already a complete story without them.

## v0.1 — Hello, traced world
Smallest kernel that boots on RISC-V in QEMU and emits boot-phase spans + a heartbeat loop as postcard frames over a dedicated serial channel. Host-side reader pretty-prints a live span tree to stdout. No userspace, no allocator, no interrupts.

**Post angle:** "Hello world, but the world snitches." Screen recording of the live span tree.

## v0.2 — Grafana arrives
Replace the stdout reader with a real collector daemon. docker-compose stack: Tempo (traces), Prometheus (metrics), Grafana, optionally Loki. Add structured metrics (counters, gauges) alongside spans. Same heartbeat workload.

**Post angle:** "From printf to a real observability stack." First dashboard screenshot — the "this looks like a product" reveal.

## v0.3 — Interrupts & clock
Trap handler, timer interrupts, a monotonic clock behind a `Clock` trait. The heartbeat becomes timer-driven instead of a busy loop. Trap entry/exit traced.

**Post angle:** "Teaching the kernel what time it is." Trace view showing periodic timer-driven spans.

## v0.4 — Memory
Page table setup, higher-half kernel layout, physical frame allocator, kernel heap. All allocators instrumented — allocation/free as metrics, heap pressure visible in Grafana.

**Post angle:** "Bootstrapping allocators before allocators exist." Grafana panel of live heap usage.

## v0.5 — Threading & round-robin scheduler
Multiple kernel threads, context switching, the simplest possible scheduler (round-robin, single queue, single CPU). Span context propagates across context switches — the first genuinely hard observability problem. Scheduler decisions traced.

**Post angle:** "Following a trace across a context switch." Multi-thread trace view.

## v0.6a — First userspace process (built deliberately wrong)
User-mode entry, the first userspace process, exactly one syscall — with ambient authority, no capability discipline. Built intentionally the "Unix way" so the next milestone can feel the pain.

**Post angle:** "The first userspace process — and why I built it wrong on purpose."

## v0.6b — Capability system
Introduce capabilities as the only access path. Refactor v0.6a's syscall to be capability-mediated. Per-process capability tables, capabilities as kernel objects, root caps to init only. Kernel begins adopting caps internally where it makes sense.

**Post angle:** "Why I rewrote the syscall layer: ambient authority vs. capabilities." The project's identity crystallizes here — a strong essay milestone.

## v0.7 — IPC over capabilities
Synchronous capability-based channels. First two-process workload. IPC paths fully traced — spans cross the process boundary. Notifications as a separate primitive (roughly the Zircon model).

**Post angle:** "Two processes talking, and watching every word."

## v0.8 — Priorities & time-sliced scheduler
Scheduler evolves: static priorities, priority aging, time slicing. Borg-style two-tier (latency-sensitive + batch) is deferred further out. Scheduler decision traces get richer.

**Post angle:** "Making the scheduler care about priority."

## v0.9 — Minimal RAMfs behind a stable Filesystem trait
A RAM-backed filesystem as a userspace component, accessed via capabilities. **The `Filesystem` trait is the deliverable** — `open / read / write / stat` etc. — with a trivial in-memory implementation behind it. Not persistent, no snapshots. CoW and content-addressing are additive later behind this same trait.

**Post angle:** "A filesystem in userspace — and the interface that lets it grow."

## v0.10 — Metrics-ingestion workload, end-to-end
The first real workload: a personal metrics ingestion server running as a userspace SnitchOS component. Laptop/phone push metrics in; SnitchOS stores them; they're served back out to Grafana. Network ingress via the host-bridge cheat (host shim forwards data in over the existing channel — no in-kernel network stack yet). The "OS is real" moment: observability now observes something real instead of trivia.

**Post angle:** "SnitchOS does an actual job now." End-to-end demo: real data flowing through a traced, capability-secured microkernel.

## v1.0 — Story complete
Polish, hardening, a coherent demo, and a wrap-up of the blog/video series. The complete v1.0 story stands on its own: capability-secured microkernel, real workload, total observability.

**Post angle:** "SnitchOS v1.0 — what it is, what I learned, what's next."

# Post-v1.0 (sequenced loosely)
- **v1.1 — Audio.** Over-engineered audio subsystem: sub-millisecond deadlines as a first-class concern, RT scheduling, audio-specific observability (per-buffer traces, XRun forensics, latency histograms). Its own multi-post arc. Optimize hard for demo interestingness here.
- **v1.2 — Real network stack.** smoltcp as a capability-isolated userspace component over virtio-net. Replaces the host-bridge cheat. Every packet a span; watch TCP slow-start in Grafana.
- **FS deepening — CoW + snapshots, then content-addressed + Merkle.** Additive behind the v0.9 `Filesystem` trait. Filesystem-as-Git. "The day SnitchOS learned to remember things."
- **Borg-style two-tier scheduler.** Latency-sensitive + batch tiers, SLO-driven scheduling, counterfactual analysis.
- **WASM userspace.** WASM as a universal portable runtime; capabilities map onto WASM imports.
- **aarch64 port.** Forces the HAL to actually be correct. "SnitchOS now runs on a Raspberry Pi." Timing unscheduled — slot when the HAL is mature.

# Numbering note
Milestone numbers are contiguous (v0.1…v0.10, v1.0). Punted/unscheduled work lives in the post-v1.0 list rather than holding reserved version numbers. If a milestone turns out too big mid-flight, split it and renumber forward.

# Open questions
- Exact audio insertion point — v1.1 is the current plan, but there are factors pushing it both earlier and later. Tracked on the Audio sub-page.
- Whether to interleave a non-FS milestone between v0.9 and the FS-deepening work to avoid an FS-heavy stretch.
- aarch64 timing.
