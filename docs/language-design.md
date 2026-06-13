# 🗣️ Language design

_A small, Java-shaped managed language for SnitchOS. Immutable by default, concise, with capabilities and telemetry as first-class language constructs. A learning vehicle as much as a feature._

Exploratory. Not on the milestone roadmap — a **parallel side project** that can move independently of the kernel track. Its first-class concern is the _implementer's_ education: how a Java-like language is actually built (front end → tree-walk → bytecode VM → generational GC). The novelty that earns it a place _on SnitchOS specifically_ is the capability and telemetry integration; the runtime techniques are deliberately conventional and well-trodden.

> **Status:** design only, nothing built. This page records the decisions made so far and the open questions still on the table, so the spine is written down and can be interrogated before any code exists.

# Primary goal: learn how Java-likes are implemented

The motivating goal is education, not novelty in the runtime. Concretely, the implementer wants hands-on familiarity with:

- A real front end (lexer → parser → AST) feeding two interchangeable back ends.
- A **tree-walking interpreter** (semantics-first), then a **bytecode VM** (implementation-technique-first).
- A **generational garbage collector** — the JVM-shaped target.

This frames every decision below. Where there's a choice between "exotic but interesting" and "conventional but exactly how the mainstream does it," we pick conventional. The interesting risk budget is spent on syntax, capabilities, and telemetry — not on the execution strategy or the collector.

# Decisions made

## Execution: host compiler → on-target runtime; staged tree-walk → bytecode VM

The compiler is a **normal Rust binary that runs on the dev host** — it does the heavy lifting (parse, typecheck, capability-effect analysis) in `std`, and never has to run `no_std` on the target. SnitchOS only ever sees the compiled artifact (AST or bytecode). This keeps the on-target footprint tiny and keeps the analysis-heavy front end out of the kernel/userspace constraints.

The runtime is **staged**, because the two stages teach two different things:

- **Stage 1 — tree-walk interpreter.** Recursively evaluate the AST. Mirrors the language's _semantics_ directly; fastest path to a working language. This is the platform on which we design and prove out the _interesting_ parts — syntax, capabilities, telemetry — end-to-end and early. Cost: slow (pointer-chasing, per-node re-dispatch, name-keyed variable lookups).
- **Stage 2 — bytecode VM.** Compile the same AST to a flat instruction array, run a tight dispatch loop over a **stack machine**. Removes exactly the costs above: linear cache-friendly instruction stream, no re-traversal, variables resolved to **stack-slot offsets at compile time**. _This is the shape of the JVM_ — building it is the core "how Java runs" learning objective.

The front end (lexer/parser/AST) is shared; only the back end is swapped. Mirrors the kernel's own `kernel-core` (pure, host-tested) vs `kernel` (target-only) split: the runtime core is host-testable Rust; only the syscall bridge is target-only. TDD discipline carries straight over.

## Memory: generational GC, grown from a simple collector

"Implicit allocation" means managed memory. The target is a **generational tracing GC** — young/old generations, collect the young generation frequently and cheaply (most objects die young). This is what Java's collectors are, so it's the right target for the stated goal.

Staged the same way as the runtime: start with a **simple correct collector** (mark-sweep or copying semispace), then **grow it into generational**. Correctness first, Java-shape second.

GC belongs to the **VM stage**, not the tree-walk stage. Rationale: in the tree-walk stage the host language is Rust, which has no GC, so we lean on `Rc` (or deliberately leak — demo programs are short-lived). The real collector arrives with the VM, because that's the first point where _we_ own the object heap layout and can find our own roots (walk the operand stack + call frames). Writing a real GC under the tree-walker would fight Rust's ownership for little benefit.

Immutable-by-default is a tailwind for the collector: immutable data forms cycles far less often, and write barriers (the generational GC's bookkeeping for old→young pointers) fire rarely.

## Process model: single process, conventional

The whole compiled program runs as **one SnitchOS userspace process** with one kernel `CapTable`. Conventional threads/tasks for concurrency (mapping onto the existing scheduler). No internal process isolation.

Consequence to stay honest about: capability typing _inside_ the language is therefore enforced by the **compiler and VM**, not the kernel. It's _language-level_ least-privilege — real and useful, but a soft boundary (a VM bug or `unsafe` escape voids it). The kernel still enforces the hard boundary at the process edge.

## Deferred: the actor language

An **actor model** — where the concurrency primitive _is_ the isolation unit _is_ an IPC endpoint, share-nothing message-passing, each actor its own kernel process — is recognized as the most elegant fit for SnitchOS (concurrency + isolation + capabilities collapse into one mechanism; messages are already traceable IPC frames). It is **explicitly deferred to a possible second language**, because (a) it's fully gated on IPC (v0.9, unbuilt) and (b) it's a whole-language identity commitment, not a runtime bolt-on. Filed here so the option isn't lost.

# Open questions — the interesting surface

These are where the design risk budget is deliberately spent, and the next things to work out. Each will get its own pass.

- **Capabilities as effects.** Functions declaring the authority they need (`fn log(msg: Str) uses TelemetrySink`), the compiler tracking the `uses` set up the call graph, startup caps arriving from `a0`/`a1` and threading down, affine/linear cap values so authority can't be forged or duplicated. The strongest reason the language exists on _this_ OS. How much of this is checked at compile time vs reified in the VM?
- **Telemetry as syntax.** Spans and metrics as first-class constructs (`span foo { ... }` auto-emitting SpanStart/SpanEnd over the existing `Frame` protocol; declared counters). Plus the reflexive win: the VM narrates _its own_ execution — GC pauses, allocation rate, cap checks, dispatch — as spans in the same Grafana as the kernel.
- **Syntax & surface semantics.** Immutable by default (`let` immutable, explicit `mut`/`var`), concise, Java-like but without the ceremony. Concrete grammar TBD.

# References

- [docs/observability-design.md](observability-design.md) — the `Frame` wire format the language's telemetry will target.
- [docs/capability-system-design.md](capability-system-design.md) — the kernel cap model the language's cap-effects sit on top of.
- [docs/ipc-design.md](ipc-design.md) — what the deferred actor language would ride on (v0.9).
- _Crafting Interpreters_ (Nystrom) — the jlox→clox arc this staging deliberately follows.
