# kernel-core — host-testable carve-out

Move the pure-data parts of the kernel out of `kernel/` and into a new
`kernel-core/` library crate so they can be unit-tested on the host
(`cargo test -p kernel-core`). The `kernel` binary stays exactly where
it is — `no_std`, `no_main`, RISC-V-only — and depends on
`kernel-core` the same way it already depends on `protocol`.

## Why this milestone

Today the kernel has zero tests. Logic-heavy code (intern table, span
nesting, pre-init buffering, scause decoding) ships into QEMU
untested. We refactored that code last session and got lucky that
nothing broke; next time we won't.

`kernel-core` is the smallest-possible step toward changing that. It
doesn't introduce a QEMU test harness, doesn't try to test asm or
MMIO. It carves out the parts that have no business depending on
RISC-V in the first place and puts a `#[test]` boundary around them.

End state: any future bug in slot allocation, span parent-restoration,
pre-init overflow accounting, or scause bit-twiddling is caught by
`cargo test` in 50 ms instead of by visual inspection of frames in the
collector log.

## Big-picture step list

| # | step | size |
|---|---|---|
| 1 | Create `kernel-core/` crate skeleton, add to workspace | small |
| 2 | Move `decode_scause` + `TrapCause` (lowest-coupling target) | small |
| 3 | Move `Clock` trait (keep `SstcClock` in kernel) | small |
| 4 | Introduce `FrameSink` trait — the seam for wire emission | small |
| 5 | Move intern table (`InternEntry`, `InternTable`, `lookup_or_insert`, `register_or_lookup`, `register_metric`) | medium |
| 6 | Move span machinery (`SpanId` counter state, `Span`, `span_start`) | small |
| 7 | Move pre-init buffer (`PreInit`, append + flush logic) | small |
| 8 | Wire kernel-side `FrameSink` impl that fans out to virtio_console + pre-init buffer | small |
| 9 | Write tests for everything moved (the actual payoff) | medium |

Steps 2 and 3 are pure code-motion — no API changes, no new
abstraction. They prove the crate boundary works before we touch
anything load-bearing.

Step 4 is the only design decision: how to abstract the wire emit
without forcing the kernel to indirect every frame through dyn
dispatch.

## What moves vs. what stays

### Moves into `kernel-core` (testable on host)

- **`decode_scause` + `TrapCause` enum** (`trap.rs:156-195`). Pure
  bit-twiddling on a `u64`. Test: known scause values → expected
  variant; unknown codes preserve the raw cause bits.
- **`Clock` trait** (`trap.rs:28-40`). Just the trait. Tests for
  consumers (handler logic) substitute a `FakeClock` that returns
  scripted ticks.
- **Intern table machinery** (`tracing.rs:42-169`). `InternEntry`,
  `InternTable`, `MAX_INTERNED`, `lookup_or_insert`,
  `register_or_lookup`, `intern_count`, `register_metric`. The
  pointer-equality lookup, slot allocation, table-full panic, and
  metric-registered idempotency all live here. The wire emit is
  abstracted via `FrameSink` (see step 4).
- **Span machinery** (`tracing.rs:227-281`). `SPAN_ID_COUNTER`,
  `CURRENT_SPAN`, `Span` struct + Drop impl, `span_start`. The Drop
  emits `Frame::SpanEnd` through `FrameSink`; no asm. The `time` field
  on the frame comes from `Clock::now()`, injected the same way.
- **Pre-init buffer** (`tracing.rs:283-322` + the pre-init arm of
  `emit_frame`). `PreInit` struct, `PRE_INIT_BUFFER`, append logic,
  dropped-count accounting, `flush_pre_init`. Fully testable: feed
  bytes in, assert buffer state and dropped count; on flush, assert
  what gets handed to the sink.

### Stays in `kernel/` (touches hardware)

- **`SstcClock`** (`trap.rs:42-60`) and the `CLOCK: SstcClock`
  constant. Both touch the `time` CSR / `stimecmp` CSR via `asm!`.
- **Trap entry / exit assembly** (`trap.S`, `entry.S`).
- **`trap_handler` dispatcher** (`trap.rs:105-115`). It reads `scause`
  via asm and calls `handle_timer`. The dispatcher is one match
  expression over `TrapCause` — almost nothing left in it once
  `decode_scause` moves out.
- **`handle_timer`, `init_timer`, `set_trap_vector`,
  `enable_timer_interrupts`** — all CSR-touching.
- **`TrapFrame` struct** (`trap.rs:62-103`). Layout matches asm
  offsets; lives next to the asm. Not load-bearing for tests.
- **All of `uart.rs`, `console.rs`, `virtio_console.rs`**. MMIO and
  static-mut queue regions. The wire-format encoding is already
  testable via the `protocol` crate.
- **All of `dtb.rs`**. Could move, but the `fdt` crate parses bytes
  from a fixture and we don't currently have a kernel-side reason to
  test DTB walking that the `fdt` crate itself doesn't cover.

### Static instances stay in `kernel/`

The `static INTERN_TABLE`, `static PRE_INIT_BUFFER`, `static
SPAN_ID_COUNTER`, `static CURRENT_SPAN` declarations stay in the
kernel binary, even though the *types* live in `kernel-core`. Why:

- Tests need to construct fresh instances per test (no shared state
  across tests — see CLAUDE.md testing rules). A `static` defeats
  that.
- The kernel binary has exactly one of each. Constructing them at the
  binary level keeps the singleton-ness visible at the boundary
  instead of buried in library state.

Pattern: `kernel-core` exposes constructors and methods that take
`&mut self`. Kernel wraps each in a `spin::Mutex<T>` static and
defines small free functions that lock + call through.

## Architectural decisions

### A. `FrameSink` trait — the seam for wire emission

The intern table, span machinery, and pre-init buffer all need to
"emit a frame somewhere." Today that's hard-coded to
`virtio_console::send` or the pre-init byte buffer. To move the logic
out, we abstract:

```rust
pub trait FrameSink {
    fn emit(&mut self, frame: &Frame<'_>);
}
```

- **Kernel impl**: `KernelSink` (in `kernel/`). Encodes via postcard,
  branches on `virtio_console::CONSOLE.get().is_some()`, sends or
  appends to pre-init buffer.
- **Test impl**: `Vec<OwnedFrame>` collector. After exercising the
  unit under test, assert on the captured sequence.

Trade-off considered: pass `&mut dyn FrameSink` everywhere vs.
generics. Going with `&mut dyn` — the kernel is monomorphizing to one
impl anyway and dyn keeps the trait-object boundary clear in test
output. Hot-path concern is nil; we already pay for `spin::Mutex` +
postcard encode on every emit.

### B. `Clock` injection follows the same pattern

`Span` and metric emission read `Clock::now()` for the `t` field. Two
options:

- Make every function take `&dyn Clock` alongside `&mut dyn
  FrameSink`. Verbose but explicit.
- Hide both behind a `Context` struct: `pub struct Ctx<'a> { sink:
  &'a mut dyn FrameSink, clock: &'a dyn Clock }`.

Going with `Ctx`. One parameter to thread through, easier to extend
later (e.g. when we add hart-id for SMP), and the call sites read
naturally: `span.end(&mut ctx)`.

Test fixture: `FakeClock { ticks: AtomicU64 }` with a `tick(n)` method
to advance time deterministically.

### C. No `unsafe` migration

None of the code we're moving uses `unsafe` today. (The asm is in
`SstcClock` and the trap handler, both of which stay.) `kernel-core`
should be `#![forbid(unsafe_code)]` from the start to keep the
boundary honest — anything that needs `unsafe` was a kernel-side
concern by definition.

### D. Crate stays `no_std`

`kernel-core` is `#![no_std]`. Production code uses only `core` +
`spin` + `protocol` + `postcard`. Test code (`#[cfg(test)]`) can
freely use `std::vec::Vec`, `std::sync::Mutex`, etc. — the test build
links std even though the crate itself doesn't.

This means we *can't* use `Vec` to back the intern table or the
pre-init buffer. Both stay fixed-size arrays. That's fine; they
already are.

### E. What we're not doing

- **No QEMU integration tests.** Separate plan, higher cost.
- **No mocking of `virtio_console::send`.** We mock at the
  `FrameSink` layer, one level up.
- **No splitting the kernel binary further.** Just one new crate. If
  later we want `kernel-trap-core` / `kernel-tracing-core` separation,
  fine — but speculative now.
- **No moving `protocol` or `postcard` boundaries.** The wire format
  is already a separate crate; we're using it as-is.

## Step-by-step

### Step 1: crate skeleton

- `kernel-core/Cargo.toml` — `edition = "2024"`, deps: `spin`,
  `protocol`, `postcard` (default-features = false).
- `kernel-core/src/lib.rs` with `#![no_std]` and
  `#![forbid(unsafe_code)]`.
- Add to workspace `members`.
- `kernel/Cargo.toml`: add `kernel-core = { path = "../kernel-core" }`.

Verify: `cargo build -p kernel-core` (host) and `cargo build -p
kernel --target riscv64gc-unknown-none-elf` both succeed.

### Step 2: move `decode_scause` + `TrapCause`

Pure copy. `kernel-core::trap::{TrapCause, decode_scause}`. Update
`kernel/src/trap.rs` to re-export or `use` from `kernel-core`.

Tests:
- `decode_scause(0x8000_0000_0000_0005)` → `SupervisorTimerInterrupt`.
- `decode_scause(0x3)` → `Breakpoint`.
- Unknown interrupt / exception preserve raw code in payload.

### Step 3: move `Clock` trait

Just the trait. `SstcClock` and the `CLOCK` constant stay in `kernel`.
`kernel/src/trap.rs` writes `impl kernel_core::Clock for SstcClock`.

No new tests yet — the trait has no impls inside `kernel-core` to
test.

### Step 4: introduce `FrameSink` trait + test impl

- `kernel-core::sink::FrameSink` trait.
- `kernel-core::sink::CapturingSink` (cfg(test) or always pub) — a
  test helper that records frames into a fixed-size array or, in test
  builds, a `Vec`.

No production code changes yet — nothing depends on the trait. This
step is just defining the contract.

### Step 5: move intern table

Move `InternEntry`, `InternTable`, `MAX_INTERNED`, and methods
`lookup_or_insert`, `register_or_lookup`, `register_metric`,
`intern_count`. The methods take `&mut self` and `&mut dyn
FrameSink` instead of locking a static and calling `emit_frame`.

Kernel side: `tracing.rs` keeps `static INTERN_TABLE:
spin::Mutex<InternTable>`, and the wrapper functions become:

```rust
pub fn register_or_lookup(name: &'static str) -> StringId {
    let mut sink = KernelSink;
    INTERN_TABLE.lock().register_or_lookup(name, &mut sink)
}
```

Tests:
- New name → returns id 0, emits `StringRegister`.
- Same name twice → returns same id, no second emit.
- Different `&str` with same content but different pointer → two
  separate ids (documents pointer-equality choice).
- Filling to `MAX_INTERNED` then one more → panics.
- `register_counter("foo")` then `register_gauge("foo")` → still
  Counter (documents the programmer-error mode).
- `register_or_lookup("foo")` then `register_counter("foo")` → emits
  `StringRegister` once, `MetricRegister` once on the second call.

### Step 6: move span machinery

Move `Span`, `span_start`, and the parent-tracking logic. The
`SPAN_ID_COUNTER` and `CURRENT_SPAN` state moves into a struct
(`SpanRegistry` or similar) that the kernel holds in a static.

`Span` no longer emits on Drop directly — Drop would need to find the
sink. Two options:

- Make `Span` hold `&mut Ctx` for its lifetime (lifetime-tied span
  guards). Awkward in nested-span code.
- Make span end explicit: `span.end(&mut ctx)`. Loses RAII.
- Keep RAII but route the Drop emit through a thread-local-ish
  "current sink" that the kernel installs at boot.

Recommend: **option 3** with a `spin::Once<&'static dyn FrameSink>`
in `kernel-core`. The kernel installs it once at boot. Span Drop
reads from it. This is the only place we deviate from clean injection
— justified because Drop can't take parameters.

Tests:
- Single span: start → drop emits SpanEnd, restores `CURRENT_SPAN` to 0.
- Nested: outer.start, inner.start, inner.drop → CURRENT_SPAN back to
  outer's id; outer.drop → back to 0.
- Sibling spans get distinct ids.

### Step 7: move pre-init buffer

Move `PreInit` struct, `PRE_INIT_BYTES` const, append-on-encode logic,
flush logic. The buffer takes `&[u8]` (already-encoded frame bytes)
rather than a `Frame` — keeps the encoding step on the kernel side
and matches how `emit_frame` works today.

Tests:
- Append until full → exactly the right slice is stored.
- One byte too many → that frame is dropped, counter increments.
- Flush emits the buffered bytes (assert on sink), then resets.
- Always emits a `Dropped` frame on flush.
- `count == 0` on the Dropped frame when nothing was lost.

### Step 8: wire kernel-side `FrameSink`

`kernel/src/tracing.rs` gains a `struct KernelSink` that implements
`FrameSink`:

```rust
impl FrameSink for KernelSink {
    fn emit(&mut self, frame: &Frame<'_>) {
        let mut buf = [0u8; 128];
        let Ok(bytes) = postcard::to_slice(frame, &mut buf) else { return };
        if virtio_console::CONSOLE.get().is_some() {
            virtio_console::send(bytes);
        } else {
            PRE_INIT_BUFFER.lock().append(bytes);
        }
    }
}
```

`emit_frame` becomes a one-liner that constructs a `KernelSink` and
calls `.emit`. The branch logic moves into the sink impl.

### Step 9: tests

Listed inline under each move step above. Roughly:

- 4 tests for scause decoding
- 6 tests for intern table
- 3 tests for span nesting
- 5 tests for pre-init buffer
- ~18 tests total

Each test constructs a fresh `InternTable` / `SpanRegistry` / `PreInit`
via a factory helper (per CLAUDE.md: no shared mutable state across
tests, no `static mut`).

## Risks and known weaknesses

- **The Span Drop static-sink wart.** Acknowledged above. Cleanest
  alternative is generic `Span<'a, S: FrameSink>` everywhere, but that
  infects every span-using call site. The static sink stays
  encapsulated.
- **`Frame<'a>` lifetime in the capturing test sink.** `Frame`
  borrows `&'a str` for names. The capturing sink either needs to
  copy strings (heap, but tests have std) or own a `Vec<OwnedFrame>`
  type we define for tests. Either is fine; pick whichever reads
  better when we get there.
- **Drift between kernel-core and kernel statics.** If a future change
  adds a new field to `InternTable` and the kernel's static
  initializer doesn't update, tests pass but kernel doesn't build.
  Acceptable — the kernel build is part of CI and catches it
  immediately.
- **Scope creep into `dtb.rs` or `protocol`.** Both tempting. Resist
  for this plan. `dtb` doesn't have testable logic today (it walks
  parsed nodes); `protocol` already has its own test suite.

## Done state

- `cargo test -p kernel-core` passes ~18 tests on host.
- `cargo build -p kernel --target riscv64gc-unknown-none-elf` still
  builds clean.
- `kernel/src/tracing.rs` is ~half its current size; the
  non-CSR-touching logic lives in `kernel-core`.
- `trap.rs` shrinks by ~30 lines (TrapCause + decode + Clock trait
  moved out).
- No behavioral change to the kernel's runtime output. Frames on the
  wire are byte-identical.

## As-built notes (post-implementation)

What actually happened, and where it deviated from the plan above.

### Deviations

- **Span: no static-sink wart.** Plan §B and step 6 proposed a
  `spin::Once<&'static dyn FrameSink>` so `Span::drop` could find a
  sink. The actual implementation makes `kernel_core::span::SpanRegistry`
  purely about bookkeeping (id allocation, parent stack) and leaves
  `Span` + its `Drop` impl on the kernel side, where it already has
  access to `emit_frame`. Cleaner — no static-sink discovery, no
  lifetime gymnastics, and `kernel-core` stays sink-free for spans.
  The registry exposes `open() -> SpanOpen { id, parent }`, `close(&SpanOpen)`,
  and `current() -> SpanId`.

- **No `Ctx { sink, clock }` struct.** Plan §B proposed bundling
  `&mut dyn FrameSink` and `&dyn Clock` into a single threaded
  parameter. Turned out neither move required it: the kernel-core
  types (intern table, span registry, pre-init buffer) don't need a
  clock to do their bookkeeping. Timestamps are read at the kernel-side
  call sites (via the existing `CLOCK` constant) and passed into the
  frame as a plain field. The plan over-specified this; remove on next
  edit.

- **CapturingSink shape.** Plan §A waved at "an `OwnedFrame` type for
  tests." Actual implementation in `kernel_core::sink::capture` is
  simpler: store the postcard-encoded bytes (`Vec<Vec<u8>>`, gated
  `#[cfg(test)]`), and decode at the assertion site via
  `postcard::from_bytes`. Tests get back a typed `Frame<'a>` borrowed
  from the captured bytes. Avoids defining a parallel owned type.

- **Pre-init buffer: `drain` takes a `FnOnce(&[u8])`, not a sink.**
  Plan step 7 said "flush emits the buffered bytes (assert on sink)."
  In practice, the buffered bytes are already postcard-encoded and go
  straight to the wire, not back through a sink. So `drain` hands the
  contiguous slice to a caller-supplied callback. The kernel side
  passes `|bytes| virtio_console::send(bytes)`. Tests pass a `Vec`
  extender. The `Dropped` frame still emits through the sink after
  drain returns, on the kernel side.

### Step 4 detail

The `FrameSink` trait and `CapturingSink` test helper landed exactly
as planned. The trait has no production impls inside `kernel-core` —
all impls live in the kernel binary (or tests). `CapturingSink` has
two self-tests (capture in order, captures emitted frame) so later
modules can lean on it.

### Test count

26 host tests landed in `kernel-core` (estimate was ~18). Breakdown:

- `trap`: 7 (timer / software / external / breakpoint / U-mode ecall /
  S-mode ecall distinguished / unknown-interrupt + unknown-exception
  preserve raw code)
- `sink::capture`: 2
- `intern`: 7
- `span`: 6
- `preinit`: 7 (+ 1 lightweight smoke for saturating add)

### Dead-code annotations

When `TrapCause` moved from a private kernel enum to a `pub`
kernel-core enum, the existing `#[expect(dead_code)]` on it
immediately fired `unfulfilled_lint_expectations` — exactly the
self-cleaning behavior we set up in the earlier session. Removed the
annotation. Pattern worth repeating: when promoting a private type to
`pub` across a crate boundary, expect any `#[expect(dead_code)]` on it
to become stale.

### What we did NOT do

- **Step 9 as a separate phase.** Tests were written inline with each
  move (steps 5–7), so step 9 collapsed into the earlier steps. No
  bulk test-writing pass at the end.
- **Move `dtb.rs` or `protocol`.** Kept in scope per the plan.
- **QEMU integration tests.** Still future work; see the plan's
  "What we're not doing" section.
