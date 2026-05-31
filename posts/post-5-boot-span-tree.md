# Post 5 — kernel.boot { }

- v0.1 finale. real span tree at boot. heartbeat loop after. the killer feature finally working as advertised: every kernel init phase appears as a span on the host with parent/child relationships, timestamps, and the kernel's name table.

## refactoring virtio_console first

- before adding tracing: threaded `virtio_base` through `kmain` and `send_hello` as a parameter. ugly. tracing wanted to emit from anywhere; threading the base through every emit site would be miserable.
- new shape: `virtio_console::CONSOLE: spin::Once<spin::Mutex<usize>>` — global handle, same pattern as `console::UART`.
- `virtio_console::init(&Fdt)` does discovery + handshake + sets the static.
- `virtio_console::send(&[u8])` reads the static, locks, transmits. callable from anywhere; silently no-ops pre-init (matches the println-fallback pattern).
- the lock is overkill today (`usize` is immutable post-init) but lets multiple emitters serialize later when we have interrupts or SMP. zero cost when uncontested.

## send(bytes) vs send_frame(frame)

- briefly tempted to make `send` take `&Frame` for the typed API.
- pushed back on it: **no allocator in v0.1**, so the encode has to live somewhere. either `send` carries a fixed worst-case buffer internally (bad — every send pays the cost of the biggest frame) or the caller passes a sized buffer.
- design choice: **caller encodes into a stack-sized buffer, calls `send(&[u8])`**. small frames use small buffers, huge frames use bigger ones, the transport stays buffer-agnostic.
- when alloc lands (v0.4), add `send_frame(&Frame)` as a thin wrapper using `postcard::to_allocvec`. both APIs coexist long-term. plan for it; don't implement until needed.

## timestamp: a CSR read, not an SBI call

- `time` CSR is a 64-bit monotonic counter clocked at the DTB's `timebase-frequency` (10 MHz on QEMU virt). readable from U/S/M mode — no privilege transition. designed precisely as the fast path for "what time is it."
- could call `sbi_get_timer` (M-mode trap, slow). using the CSR directly: one `rdtime` instruction, no trap, no overhead.
- inline asm: `core::arch::asm!("rdtime {t}", t = out(reg) t)`. named operand, output register, that's it.
- rv64 reads all 64 bits in one instruction. rv32 would need `rdtime` + `rdtimeh` and worry about wraps between the two reads.

## the conversion-overflow trap

- noodled on ticks-to-seconds: `ticks * 10^9 / timebase_hz` for nanoseconds.
- the *result* fits comfortably in u64 (centuries of nanoseconds). the *intermediate* `ticks * 10^9` overflows after ~30 minutes of uptime at 10 MHz tick rate.
- fixes: do the division first (loses precision when timebase doesn't divide 10^9 cleanly), or use u128 intermediate.
- the design doc's answer is more elegant: **don't convert in-kernel.** wire format carries raw u64 cycles; host does the math with f64 where overflow isn't a concern.

## string interning

- 64-slot static table, `[Option<&'static str>; 64]`, plus an atomic-counter-y `next_id`. wrapped in `spin::Mutex`.
- equality by **pointer**, not value: `&'static str` literals have stable addresses within a compilation unit, so `s.as_ptr() == name.as_ptr()` is O(1) per slot.
- new string → assign id → store in slot → emit `Frame::StringRegister` → return.
- known weakness: cross-crate identical literals could have different addresses → duplicate entries. fine for v0.1 single-crate kernel; revisit when userspace registers names.
- lock-during-emit is the load-bearing choice: hold the intern table's mutex while sending the StringRegister, so two threads can't both decide "I'll register this" and double-emit. lock ordering: intern → virtio_console::CONSOLE; documented because the moment another lock enters this graph the wrong direction wedges everything.

## span machinery — counters and a stack

- two atomics: `SPAN_ID_COUNTER` (starts at 1, `SpanId(0)` reserved as "no parent"), `CURRENT_SPAN` (the innermost open span, initially 0).
- new span: read CURRENT_SPAN → that's the parent. mint id from the counter. write id back to CURRENT_SPAN. emit SpanStart. **return a guard that remembers the previous parent.**
- the guard's `Drop`: emit SpanEnd, store the saved parent back to CURRENT_SPAN.
- nesting falls out of Rust scopes for free. inner `let _g = span_start("b")` drops before outer at scope exit; SpanEnds come out in reverse-of-start order. no explicit stack data structure needed — the rust call stack *is* the span stack.
- got the counter off-by-one wrong: had `fetch_add(1) + 1` against a counter starting at 1, yielding ids 2, 3, 4. fixed to just `fetch_add(1)`. the doc comment got there first; the code lagged.

## RAII + the macro

- `span_start` returns a `Span` value. naked use: `let _g = tracing::span_start("foo");` — the `_g` binding keeps the guard alive until end of scope.
- problem: `tracing::span_start("foo");` (no binding) drops the guard immediately at end of *statement*. Span closes before the body it was supposed to wrap. silent zero-length span on the wire.
- fix: the `span!` macro emits a `let _span = ...` binding directly, so the guard outlives the caller's scope automatically.
- **key macro property: emits a statement, not a block.** if I'd written `{ let _g = ...; }` the guard would drop at the end of the macro's own block. macros that affect the surrounding scope can't wrap in `{}`.
- this is one of macro_rules' real powers — functions can't do this; macros can.

## boot tree wiring

- DTB parse runs **before** kernel.boot opens. tracing needs the UART address discovered before any println would work; needs virtio-console up before frames can be sent. parsing is bootstrap; the meaningful init phases are what kernel.boot covers.
- inside kernel.boot:
  - sub-span `console_init` wraps `console::init(uart_addr)` (NS16550 setup).
  - sub-span `telemetry_init` wraps `virtio_console::init(&dtb)` (the handshake we built last post).
- after kernel.boot closes, enter the heartbeat loop.

## the pre-init buffering problem

- kernel.boot opens *before* virtio-console is ready. its SpanStart goes to `virtio_console::send` → no-op → frame dropped silently.
- accepted this as v0.1 weakness at first. user said: more work, but worth it. agreed.
- added a 1 KiB **pre-init buffer** in tracing. `emit_frame` checks `virtio_console::CONSOLE.get().is_some()` — if yes, send directly; if no, append the encoded bytes to the buffer.
- after `virtio_console::init` succeeds, kmain calls `tracing::flush_pre_init()` which dumps the buffer as one big `send(&[u8])` blob. host-reader's stream decoder sees a sequence of postcard frames back-to-back and unpacks them all.
- subtle thing: the bytes in the buffer are *already postcard-encoded*. they don't need re-encoding. concatenated wire bytes are exactly what the host expects.

## the Dropped checkpoint

- buffer might fill up (it didn't, but it could).
- first design: emit `Frame::Dropped { count }` only if overflow happened.
- counter-proposal: emit `Dropped { count: 0 }` even when clean. positive confirmation to the host: "I flushed the pre-init buffer, here's the loss count (0 = perfect)."
- agreed. asymmetric "report problems only" semantics make absence-of-problem indistinguishable from "I never ran this codepath." emit always; count = 0 is the good-path signal.
- also switched the overflow tracking from `bool` to `u32` so the count is real, not a yes/no flag.

## heartbeat loop

- after boot, busy-spin on `timestamp() < next_deadline`. when reached, emit a `kernel.heartbeat` span (just StringRegister-once + SpanStart + SpanEnd), bump deadline by `timebase_hz` (= 1 second).
- v0.1 has no real timer interrupts; the busy-spin is the cheapest "approximately periodic" thing we can do. CPU at 100% but the kernel has nothing else to do.
- the host sees ~one frame pair per second forever. that's the demo: visible periodic activity, fully observable.

## what i learned

- **Drop-based RAII + scope = automatic correctness for nesting.** I expected to need an explicit stack. Rust's scope-based lifetimes are the stack.
- **macro_rules emits statements, not expressions.** the `span!` macro relies on this; the difference between `let _g = ...` (binds to surrounding scope) and `{ let _g = ...; }` (drops inside macro) is everything.
- **report success, not just failure.** the host can't tell "buffer flushed clean" from "kernel never ran the flush" without a positive signal. `Dropped { count: 0 }` is small, makes the wire self-describing.
- **caller-allocated buffers are honest in no_std.** the "right" API depends on whether you have an allocator. don't pretend you do.
- **monotonic counters are cheap; SBI calls aren't.** read CSRs directly when the spec allows.
- **pre-init order is a real problem.** anything that wants to observe boot has to either be initialized before what it's observing, or buffer until it can flush. the kernel observability story can't dodge this — it has to address it directly.

## v0.1 status

| ✓ | thing |
|---|---|
| ✓ | kernel boots in QEMU on RISC-V |
| ✓ | NS16550 UART driver |
| ✓ | DTB parse |
| ✓ | virtio-console driver (mmio + handshake + virtqueue + TX) |
| ✓ | protocol crate (7 frame types, hosted TDD) |
| ✓ | host-reader (decode_stream, --pretty) |
| ✓ | xtask (cargo xtask up / reader) |
| ✓ | tracing module (timestamp + intern + span machinery + span! macro) |
| ✓ | pre-init buffer + Dropped checkpoint |
| ✓ | boot span tree + heartbeat loop |

v0.1 is functionally done. all that's left is polish:
- the `probe_all_slots` dead-code warning
- the missing `gp` global pointer setup we filed earlier
- README screencast
- the README "Status" section (which is what this table is)
- decide whether to keep the `host-reader` retry-connect, the `find_console_base`/`init_handshake` pub-ness, etc.

## next

- polish + screen recording + commit.
- then v0.2 (Grafana). first time we'll write a real collector daemon. the existing host-reader either gets a `--otlp` flag or becomes the wire-level layer below a real OTLP exporter.
