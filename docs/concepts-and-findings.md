# 💡 Concepts & findings

*Cross-cutting conceptual ideas that emerged from exploration and quizzing. Not milestone plans — understanding that informs the design.*

# The recurring pattern: identity vs. name
The single most repeated idea across the whole design. An *object* (the real thing, its substance and identity) is consistently separated from a *name/handle* (a label that refers to it). The separation is what enables flexibility, and it shows up at least five times:

- **Capabilities:** the kernel object vs. the opaque `u32` handle that names it. Forgery is impossible because a handle is meaningless except as a lookup in a per-process table only the kernel can write.
- **String interning:** the string vs. the `u32` id that refers to it.
- **Filesystem inodes:** the inode *is* the file (metadata + block pointers); the filename is just a directory entry pointing at an inode. Decoupling is what allows hard links (many names, one inode).
- **Sockets:** the kernel object holding connection state vs. the file-descriptor handle.
- **Content-addressed GC:** a block's *name* (its hash) tells you nothing about whether it is alive; only its *reachability* in the graph does.

Whenever a design question involves "how do I refer to X," expect this pattern.

# Determinism vs. speed
A deep systems distinction that came up repeatedly. The instinctive framing of a problem is often "X is slow" when the real issue is "X is *unpredictable*."

- **Audio and allocation:** an audio hot path avoids heap allocation not mainly because allocation is slow on average, but because its worst-case time is unbounded and hard to even characterize. Real-time means *deterministic* — every hot-path operation needs a known worst-case bound (WCET). Audio pre-allocates everything before entering the real-time path.
- **Caches:** a cache improves the *average* but the system's real limits live in the *uncached worst case*. A cache can even be misleading — testing shows the fast number. For hard real-time you must budget for the cold-cache worst case.
- Same shape at distributed-systems scale: a cache absorbing 95% of load means the backing store is provisioned for 5% — a cache failure is a sudden ~20× load spike (thundering herd).

# Layered claims that lie for speed
"The operation returned" and "the effect is durable/complete" are different claims, separated by caching layers each lying to the one above it for performance.

- **`write()` vs. durability:** `write()` puts data in the kernel page cache (RAM) and returns. `fsync()` forces it toward the device — but data can *still* hide in the disk's own write cache. True durability needs the flush to reach physical media. CoW filesystems make this tractable: durability becomes one well-defined moment (is the new root committed).
- General lesson: trace a claim down through the layers before trusting it.

# Concurrency vs. parallelism
- **Concurrency** is a *structuring* property: the program is written to handle interleaved logical flows. True even on a single core (interrupts alone create it).
- **Parallelism** is an *execution* property: flows literally run at the same instant on different cores.
- A single-core kernel is concurrent but not parallel.
- Consequence: concurrency bugs (races) exist the moment you have concurrency — so locks are needed *before* SMP. This is the "SpinLock and PerCpu from line one" decision.

# Locks: spin vs. block
- A **SpinLock** loops, burning CPU, when it fails to acquire. Reasonable only when the critical section is *extremely short* (a few instructions). Terrible when the section is long or might block.
- A **blocking lock / Mutex** puts the waiting thread to *sleep* (off the run queue, woken when free).
- **An interrupt handler may only use spinlocks.** "Block" means "park the current thread and run another" — an interrupt handler is not a schedulable thread, so it has nothing to put to sleep. The constant question in kernel code: *what context am I in, and is it a schedulable thread?*

# Preemption
- A **preemptive** scheduler switches a process out without its consent; a **cooperative** one only regains control when the process yields.
- Preemption requires the timer interrupt — without a periodic interrupt the kernel never gets control back from a running process.
- Cooperative scheduling's catastrophic failure mode: one process that never yields (bug, infinite loop) freezes the whole system. Preemption is what makes a buggy process survivable.

# Priority inversion
Low-priority thread L holds a lock high-priority thread H needs — so H waits on L. Then a *medium*-priority thread M (needing no lock) becomes runnable; M outranks L, so the scheduler runs M instead of L; L cannot release the lock; H is now effectively stuck behind M, a lower-priority thread. The priority order is inverted. Fix: priority inheritance (L temporarily inherits H's priority). This is the Mars Pathfinder bug.

# Floating point in the kernel
**Policy: kernel code is integer-only. Floats are a userspace concern.**

- Mechanically the kernel *could* do float math (RISC-V F/D extensions; `f64` works in `core`). But the FPU has its own large register set. If the kernel never uses floats, the trap path can skip saving/restoring FP registers — a real saving on the hottest path. One float op in the kernel kills that optimization for all code forever.
- Real kernels (Linux) forbid kernel FP by policy; using it requires explicit, discouraged bracketing.
- The kernel rarely needs floats anyway — scheduling, tables, addressing, allocators are all integer work.
- Places a number looks float-ish, already designed around: timestamps (kernel emits raw u64 cycles, host converts — already decided); metrics/rates (use fixed-point integer arithmetic, or emit raw numerator/denominator and let the host compute).
- **Userspace uses floats freely** — especially audio (DSP). So the kernel's context-switch code *must* correctly save/restore FP registers *for userspace threads*: the kernel does not *use* floats but must *preserve* them. Optimization worth knowing: **lazy FP save** — save/restore FP state only when a thread actually touches the FPU (first-use trap), so threads that never use floats never pay the cost. A real trap/scheduler design choice the audio work may pull in.

# Text rendering (conceptual — not a planned feature)
Text rendering is a microcosm of the "friendly API over a stack of specs" trap, same lesson as the browser. Clean tiers:

- **Bitmap font + fixed grid** — each glyph is a pre-drawn pixel grid; rendering is blitting grids into the framebuffer. A weekend feature. This is what a *terminal* needs (grid of fixed cells). SnitchOS-appropriate level.
- **Vector rasterization** (TrueType outlines = Bézier curves) — scalable, but each step is hard: rasterization (curves → pixels), anti-aliasing (per-pixel coverage), hinting (per-font bytecode snapping features to the grid), subpixel rendering. A Rust crate like `fontdue` does this tier without full shaping. A real but bounded project.
- **Shaping + bidi + all scripts** — one character ≠ one glyph (ligatures, Arabic positional forms, Devanagari reordering); bidirectional text; line breaking; Unicode normalization and grapheme clusters. Years of work; the industry wrote *one* library (HarfBuzz) so nobody repeats it. You would port it, not write it.

Honest call for SnitchOS: bitmap fonts for console/terminal; anything past that is a deliberate, scoped GUI milestone, not something that sneaks in.

# A browser, conceptually
Not a program — an OS's worth of subsystems in a trench coat. Four large independent parts: (1) network stack including TLS and HTTP/2-3/QUIC; (2) engine — HTML parsing (a spec'd error-recovery state machine) + CSS cascade, producing the DOM; (3) layout + rendering — box model, flexbox/grid constraint solving, text layout, rasterization (the decade-of-work part — why only three engines exist); (4) a JS engine wired to the DOM. The pragmatic MVP is *port Servo* (Rust, embeddable). A from-scratch toy engine (restricted CSS subset, no JS, own TLS+HTTP/1.1) is a good scoped capstone and great blog content — it just renders simple pages, not the real web. The multi-process-tabs-as-capabilities framing would make it uniquely a *SnitchOS* browser.

# The recurring pattern: the danger lives where the checks can't reach

The second most repeated idea, and unlike the first it was learned rather than
designed. Stated once, plainly, in
[post 69](../posts/post-69-the-bug-was-where-no-test-could-reach.md):

> the compiler, the review, the test suite — none of them check what you've put
> out of their reach. **so the danger lives exactly there.**

Every serious bug below is the same shape: *a claim standing somewhere nothing
could refuse it.* Not a mistake in reasoning — an assertion placed outside the
reach of the thing that would have caught it. They are collected here because
each was rediscovered independently, which is the tell that it is a property of
the work and not a run of bad luck.

## Three ways a claim gets out of reach

**1. Told to a checker that believes you.** A checker verifies only what you let
it see; anything you *assert*, it takes on trust.

- The kernel's build script declared a directory it **writes into** as an input,
  so it invalidated itself forever and rebuilt the kernel on every invocation.
  Cargo cannot see that cycle — it believes the declaration.
  ([67](../posts/post-67-the-build-watched-what-it-wrote.md))
- `in("a1")` on an SBI `ecall` promised the compiler that a register survives a
  call which firmware overwrites. The optimizer believed it completely and parked
  a live pointer there.
  ([68](../posts/post-68-two-promises-the-hardware-never-made.md))
- A syntax reference wrote `cond => a | b`, meaning `cond` as a metavariable. The
  model read it as a keyword and emitted `cond overlap(b1: Booking) -> Bool`.
  **An omission or ambiguity in a reference does not produce a blank — it
  produces a confident wrong guess.**
  ([66](../posts/post-66-cond-is-not-a-keyword.md))

**2. Placed where no test runs.** Not asserted — simply unreachable.

- `kernel-core` meant "the bits that happen to build for the host," which is not
  a concept. Pure logic stranded in the kernel binary was untestable *because* of
  where it sat — and the instinct to find it by grepping for `unsafe`-freedom was
  the wrong test, since most of it is glue touching statics or `TrapFrame` that
  could not run on the host either.
  ([69](../posts/post-69-the-bug-was-where-no-test-could-reach.md))
- The ELF loader had a writable-executable page one `static mut` away, with no
  test in a position to notice.
  ([69](../posts/post-69-the-bug-was-where-no-test-could-reach.md))
- snemu shouted its most useful diagnostic into a discarded `Result`.
  ([74](../posts/post-74-the-emulator-was-shouting.md))

**3. Checked by something that shares the defect's assumption.** The worst case,
because a check *is* running and reporting success.

- Stitch's typing is **gradual**, so its diagnostics are advisory. Across the
  first five generated candidates, parse caught two and the programs' own tests
  caught three; the type checker caught **none**. A stage that cannot fail is not
  a gate.
- Generated candidate 006 parses, type-checks and passes all five of its own
  tests **while being wrong** — it compares only the head of a list against the
  tail. Code and tests agree because both were written from the same
  misunderstanding. This is the concrete argument for scoring a suite by
  *mutants killed* rather than by passing.
- Self-repair, handed a broken program with no diagnostic, fixed 2 of ~9 errors —
  both syntactic, precisely the class a grammar mask makes impossible anyway.
  **The diagnostic is the load-bearing half of a repair, not the model's
  introspection.**

## The mirror image: plausible stories the measurement refused

The same bug pointed inward. An explanation that is *reasonable* and *untested*
is also a claim out of reach, and believing it costs real work:

- Splitting `xtask` into sibling crates "for compile speed" moved the number by
  **nothing** — the cost is clap-derive expansion and dependency
  monomorphisation, which stay in the binary however you carve up your own
  modules. Kept as real factoring; retracted as a speed fix.
  ([67](../posts/post-67-the-build-watched-what-it-wrote.md))
- Splitting `kernel-core` for speed, likewise: the tests already ran in 0.01s,
  and more crates means *more* per-test-binary floors, so the honest full-suite
  number is slightly **worse**. Done anyway, on the real reason — a crate
  boundary is enforced, a module boundary is discipline.
  ([69](../posts/post-69-the-bug-was-where-no-test-could-reach.md))
- "Framing is the hard part" for network telemetry. The wire had *always* been an
  unframed byte stream; virtio's message boundaries were never load-bearing. The
  real problem was resynchronisation, which is a transport property.
  ([70](../posts/post-70-the-wire-was-never-load-bearing.md))
- Eyeballed verdicts on five generated programs were **40% wrong**, discovered
  the moment a twenty-line gate function existed to check them.
  ([66](../posts/post-66-cond-is-not-a-keyword.md))

## The fix is always the same move

**Drag the claim to where something can refuse it.** Every instance above was
resolved not by thinking harder but by relocating the question:

| the claim | what could finally refuse it |
|---|---|
| "the kernel rebuilds because of feature flags" | `CARGO_LOG=…fingerprint=info` |
| "`a1` survives the call" | release codegen under a deterministic emulator |
| "this module is layered correctly" | a crate boundary — a `Cargo.toml` line a reviewer sees |
| "this generated program is fine" | `stitch::gate::run` |
| "the new corpus is better" | a control arm holding token count fixed ([75](../posts/post-75-it-was-the-volume.md)) |
| "these tests cover it" | mutation scoring |

Two corollaries worth keeping:

- **Build the ruler earlier than feels necessary.** The eval harness was written
  before the corpus it measures, and the first thing it produced was that the
  baseline had never been measured either
  ([73](../posts/post-73-the-floor-was-in-the-wrong-place.md)). The gate function
  was written after eleven hand-read candidates and immediately overturned two of
  five recorded verdicts.
- **A guard that names itself is worth building even when it looks unnecessary.**
  The one-FP-process refusal was written against "a case nobody has" and had a
  customer within two days — announcing itself in one attributable line instead
  of corrupting silently
  ([72](../posts/post-72-the-unit-was-already-on.md)).
