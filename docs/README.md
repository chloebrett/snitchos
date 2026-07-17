# 🐀 SnitchOS

*A capability-secured microkernel where every kernel operation is observable.*

# What this is
SnitchOS is a toy OS built in Rust, designed as a multi-year learning project with heavy AI-agent collaboration. It is not for production. It is an academic exercise that doubles as a public learning resource and a demonstrable artifact of systems work.

The name comes from the killer feature: the kernel rats on itself. Every operation is traced, every metric is exported, every decision is observable. Couple that with capability-based security and a microkernel architecture and the project has a story: *what does an OS look like when you design observability and security in from line one?*

# Goals and weighting
- **Learning vehicle (40%)** — primary goal. Depth in a layer adjacent to day-job (Senior SWE, Google Maps, backend + Android). Finishing is nice but not the point. Optimize for surface area of interesting decisions encountered.
- **Demonstrable artifact (40%)** — articles, possibly YouTube videos, well-documented as a learning resource for others. Evidence of systems proficiency.
- **Agent collaboration laboratory (20%)** — working productively with Claude to cover more ground than would otherwise be possible. Meta-improving the collaboration is a means, not the end.

# The three pillars (the story)
1. **Observability from the ground up.** Binary-framed telemetry over a dedicated serial port from line one. OpenTelemetry-style traces, Prometheus metrics, structured logs. Grafana dashboards. The killer feature exists in v0.1.
2. **Capability-based security.** No ambient authority. Every resource access goes through an unforgeable handle. Inspired by seL4 / Fuchsia / KeyKOS lineage.
3. **Microkernel architecture.** Kernel does the minimum: address spaces, threads, capabilities, IPC, scheduling, interrupts, low-level memory. Everything else is a capability-isolated userspace component. Target <10K kernel LOC for v1.

# Bonus features that fit the story
- **Over-engineered audio support.** Sub-millisecond deadlines as a first-class kernel concern. Combines beautifully with observability (per-buffer traces, XRun forensics) and capabilities (audio devices as caps).
- **Deep WASM userspace.** WASM as universal portable runtime. Capabilities map naturally to WASM imports. Sandbox already exists.
- **CoW + content-addressed + Merkle-verified filesystem.** Filesystem-as-Git. Snapshots, dedup, integrity, time travel.

# Explicit non-goals
- Not production-ready. Not aiming to be.
- Not Linux ABI compatible. Rust source portability + WASM is the compatibility story.
- Not POSIX compatible. The design themes are incompatible.
- Not multi-arch from line one. RISC-V first, aarch64 as a deliberate later milestone.
- Not a research contribution. A learning artifact.
- Not going to be finished. The compounding bet is on the journey.

# Methodology
- **TDD throughout.** Hosted unit tests for pure logic, kernel integration tests in QEMU, end-to-end scenario tests.
- **Incremental milestones.** Each milestone is a shippable narrative arc — a blog post or video falls out as a side effect, not extra work.
- **Documentation as first-class deliverable.** ADRs for every meaningful decision. Learning journal. Public writing.
- **Agent collaboration with discipline.** Design doc first, read every line, write the hard parts by hand, periodic agent-off periods, pair-don't-delegate.

# Status

**Shipped through v0.13.** The kernel boots on RISC-V (QEMU *and* our own emulator),
runs preemptive multi-hart userspace, and mediates every resource access through
capabilities. `init` is the default boot: a userspace root that spawns a userspace
filesystem server and a shell over cap-mediated IPC. Everything is traced.

[**Roadmap & milestones**](roadmap-and-milestones.md) is the current plan of record and
the honest status page — start there. Active implementation tracks live in
[`plans/`](../plans/); completed ones in [`plans/legacy/`](../plans/legacy/).

# Index

*Every doc carries its own status line — trust that over this list. Conventions: a
superseded doc keeps its filename and gains a banner pointing at its successor
(`roadmap-historical-through-v0.11.md` is the worked example); hand-drawn diagrams
carry a `<!-- diagram: reviewed <date>, owner=… -->` banner. There is no `docs/legacy/`
— a design doc's value survives shipping.*

## Architecture — how the built system works

The pillars:

- [Observability design](observability-design.md) — wire format + span semantics. ⚠️ v0.1-era: authoritative on the format, **superseded on the emit path** (see its banner).
- [Capability system design](capability-system-design.md) — no ambient authority; the project's spine. *Shipped v0.7b.*
- [IPC design](ipc-design.md) — synchronous endpoints, badges. *Shipped v0.9.* · [call/reply walkthrough](ipc-call-reply.md)
- [Filesystem design](filesystem-design.md) — a userspace FS reached only through caps. *Shipped v0.10.*
- [Notification design](notification-design.md) — the signal primitive. *Shipped v0.12.*
- [Supervision design](supervision-design.md) — init as a service supervisor. *v1 shipped.* · [lifecycle diagram](supervision-lifecycle.md)
- [Capability names](capability-names-design.md) *(shipped)* · [Cap revocation](cap-revocation-design.md) *(shipped)* · [review, 2026-07-05](cap-names-review-2026-07-05.md)
- [Typed processes & the data model](typed-processes-and-the-data-model-design.md) *(largely shipped)* · [FS executables](fs-executables-design.md) *(partly shipped)*
- [Runtime workload selection](runtime-workload-selection-design.md) — why one kernel binary holds every workload. *Implemented.*

Kernel internals (read before touching the matching code):

- [Memory map](memory-map.md) — *"the single most re-explained thing in the kernel; read it before touching any translation site."*
- [Trap & interrupt model](trap-and-interrupt-model.md) · [Boot handoff](boot-handoff.md) · [Context switch](context-switch.md)
- [RISC-V boot & SBI](riscv-boot-and-sbi.md) — reference material from v0.1 bring-up.
- [Concepts & findings](concepts-and-findings.md) — cross-cutting understanding that informs the design.

## The reinvented wheels

- [snemu](snemu-design.md) — our RISC-V emulator. *Built: JIT, ramfb, multi-hart.* · [snapshot tree](snemu-itest-snapshot-tree-design.md) *(shipped)* · [perf options](snemu-perf-options.md) · [packing viz](snemu-itest-packing-viz-design.md) *(unbuilt)*
- [Stitch](language-design.md) — the language. *Substantially built; VM + GC ahead.* · [pipeline](stitch-pipeline.md) · [test library](stitch-test-library-design.md) *(unbuilt)* · [mutation testing](stitch-mutation-testing-design.md) *(unbuilt)*
- [Diagrams](diagrams-design.md) — what we draw by hand vs what the code draws. *Built* → [`generated/`](generated/)
- [Framebuffer](framebuffer-design.md) — *milestone 0 shipped; the snitching screen is unbuilt.*

## Designed, not built

- [Manifest](manifest-design.md) — the authority-description language. The highest-fan-out unbuilt design.
- [Shell surface & TUI](shell-surface-and-tui-design.md) · [Shell primitives](shell-primitives-design.md) · [stim](stim-design.md) — the editor.
- [Userland text streams & the actor model](userland-text-streams-and-the-actor-model-design.md) — typed pipes; foundations shipping.
- [Accounts & login](accounts-and-login-design.md) · [Clipboard](clipboard-design.md) · [Physics desktop](physics-desktop-design.md) · [Grafana capture](grafana-capture-design.md)
- [Randomness & entropy](randomness-and-entropy.md) — *"captured now so the decisions aren't re-litigated later."*
- [Arcade & real hardware](arcade-and-real-hardware-direction.md) — the committed post-v1.0 north star.

## Backlogs & method

- [Debt register](debt-register.md) — living; delete an item when it's done.
- [Redesign from scratch](redesign-from-scratch.md) — the method → [`redesign-reviews/`](redesign-reviews/)
- [Software on SnitchOS](software-on-snitchos.md) · [backlog](software-to-explore-backlog.md) · [notes](software-explorations-notes.md)
- [Seven questions](design-explorations-seven-questions.md) · [Cross-cutting axes](cross-cutting-axes-brainstorm.md) — handoff notes; **still cited by live design docs, don't archive.**
- [Scaling SnitchOS down](scaling-down-snitchos.md) — tickless, MCUs, the memory floor.
- [Vocabulary playground](vocabulary-playground.md) — ⚠️ carries its own "massive disclaimer".
- [resources.md](resources.md) — a scrap: one bare URL (a QEMU fix), no context. Fold into [RISC-V boot & SBI](riscv-boot-and-sbi.md) or drop it.

## Historical — true when written, kept as record

- [Roadmap through v0.11](roadmap-historical-through-v0.11.md) — superseded 2026-06-21; the v0.6-era vantage point.
- [v0.1 — Hello, traced world](v0.1-hello-traced-world.md) — the founding milestone spec.
- [v0.7 userspace concepts](v0.7-userspace-concepts.md) · [v0.6 mutex vs SPSC measurements](v0.6-mutex-vs-spsc-measurements.md) — dated evidence.
- [Adversarial kernel review, 2026-07-05](fable-kernel-review-2026-07-05.md) — findings against a snapshot; check what's still open.
- [Parked — Memory model & HAL](parked-memory-model-and-hal.md) — ⚠️ its parked-ness expired; memory model shipped at v0.4.
