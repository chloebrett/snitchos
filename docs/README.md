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

# Sub-pages
Detailed plans live in sub-pages, created as we work through decisions:

- [v0.1 — Hello, traced world](v0.1-hello-traced-world.md)
- [Roadmap & milestones](roadmap-and-milestones.md)
- [Randomness & entropy](randomness-and-entropy.md)
- [Observability design](observability-design.md)
- [Capability system design](capability-system-design.md)
- [IPC design](ipc-design.md)
- [Parked — Memory model & HAL (Q27, Q28)](parked-memory-model-and-hal.md)
- [RISC-V boot & SBI — reference](riscv-boot-and-sbi.md)
- [Trap & interrupt model — reference](trap-and-interrupt-model.md)
- [Concepts & findings](concepts-and-findings.md)
- [Filesystem design](filesystem-design.md)
- [Software on SnitchOS — exploration](software-on-snitchos.md)
- [Software to explore — backlog](software-to-explore-backlog.md)
- [Software explorations — notes](software-explorations-notes.md)

# Status
In planning. No code yet.
