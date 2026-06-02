# Post 8 — Making the kernel testable

- the kernel had zero tests. seven posts in, and the only way to know anything worked was to run it in QEMU and squint at the wire. this post is two refactors that fix that: **carve out a `kernel-core` library** so the data logic gets unit tests, **build an xtask integration harness** so the boot path gets QEMU-driven assertions.

## the one specific refactor that started this

- `dtb::timebase_hz` was returning `u32` and defaulting to `0` if the DTB lacked the property. zero is never a meaningful clock frequency. the downstream code would divide by zero or worse.
- changed it to `Option<u32>`, expect at the call site with a clear message. five-line refactor.
- then: are there other small refactors like this in the kernel? yes, several. did them. then: should we test any of this? no tests exist. why?

## why "no tests for the kernel" was actually a confusion

- the kernel binary is `no_std` + `no_main` + builds for `riscv64gc-unknown-none-elf`. you can't `cargo test` it — `cargo test` builds for the host triple, and the host doesn't have `_start` or a linker script telling it where the stack goes.
- but **`no_std` is a crate attribute, not a target.** any `no_std` library can be built for whatever target Cargo asks for. when the kernel binary depends on a `no_std` lib, the lib compiles for riscv. when `cargo test -p the-lib` runs, the lib compiles for the host. and `#[cfg(test)]` blocks inside it can freely use `std::*` because tests build with the host's full toolchain.
- two corollaries:
  - the kernel **binary** is forever untestable on the host. fine.
  - any **logic** that doesn't touch CSRs / MMIO / asm can live in a sibling library that *is* host-testable. that's the seam.

## the carve-out

- new crate `kernel-core/`. `#![no_std] #![forbid(unsafe_code)]`. compiles for riscv when the kernel uses it, for the host when tests run it. inverts the testing story without changing what ships.
- what moved in:
  - **`decode_scause`** + the `TrapCause` enum. pure bit-twiddling on a u64.
  - **`Clock` trait** (the impl, `SstcClock`, stays kernel-side — it touches the `time` CSR).
  - **intern table** — pointer-keyed string→id, slot allocation, table-full panic, metric-registered idempotency.
  - **span registry** — id allocation, parent stack via two `AtomicU64`s.
  - **pre-init buffer** — fixed-size byte buffer with `append` + `drain` + dropped-count saturation.
  - **`FrameSink` trait** — abstracts "where does an encoded frame go." tests collect to a Vec; kernel encodes + dispatches to virtio-console or pre-init.

- what stayed in `kernel/`: the asm, the static instances (kernel has exactly one of each), the `KernelSink` adapter that branches on `virtio_console::CONSOLE.get().is_some()`.

## the Drop puzzle

- `Span` is RAII — its `Drop` emits `SpanEnd`. fine when emit is a free function in the same module. but the moved `SpanRegistry` doesn't know about emits — it only does bookkeeping. so does `Drop` reach into a `spin::Once<&'static dyn FrameSink>` to find a sink?
- considered it. wart.
- realised: **the RAII boundary is upstream of the abstraction line.** `kernel-core::span::SpanRegistry` does the parent-stack math; the kernel-side `Span` keeps Drop where it already had access to `emit_frame`. registry returns `SpanOpen { id, parent }`, kernel wraps it in a newtype, Drop calls `SPAN_REGISTRY.close(...)` + emits.
- one wart removed; library stays sink-free.

## one tiny rust-2024 win

- previously had `#[allow(dead_code)]` scattered over things we expected to come back to (TrapCause variants only read via Debug, three virtio descriptor flags we don't use yet, the Clock trait until we wired it through).
- rustc 1.81 stabilised `#[expect(lint, reason = "...")]`. acts like `allow` while the lint *would* fire, but emits `unfulfilled_lint_expectations` (warn-by-default) if the suppression became stale.
- converted all five `#[allow(dead_code)]`s. paid off the same session: when `TrapCause` moved into a `pub` enum in `kernel-core`, the variants stopped being dead, and rustc flagged the now-redundant `expect` immediately. self-cleaning lint suppression.

## the test ROI

- 29 host tests after the carve-out:
  - 7 for scause decoding (timer / software / external / breakpoint / U-mode ecall / S-mode ecall distinguished — same numeric code, different branches — unknown variants preserve raw bits)
  - 7 for intern table (new name → id 0 + StringRegister; same pointer = same id, no re-emit; **same content / different pointer = distinct ids** — pins the pointer-equality decision; table-full panics; `register_counter` then `register_gauge` keeps Counter — pins the documented programmer-error mode; lookup-then-register emits MetricRegister only)
  - 6 for span registry (root has no parent; first id is 1; nested open records outer as parent; close restores; siblings share parent; ids monotonic across open/close)
  - 7 for pre-init buffer (within capacity; drain ships in order; drain resets; empty drain doesn't fire callback; oversize is **atomically dropped, not partially written**; counter accumulates until drain resets; saturating add)
  - 2 for the capturing test sink itself
- `cargo test -p kernel-core` runs in ~50 ms. each one of these pins a documented design decision; if a future refactor breaks one, the message explains why it matters.

## the integration tests

- carve-out doesn't cover the actually-boots question. unit tests say the intern table works in isolation; they don't say anything about whether the kernel reaches `kmain`, whether virtio-console handshakes, whether Hello is byte 1 on the wire, whether the timer IRQ fires.
- so: second test layer. boot the kernel in QEMU, read the virtio-console socket from the host, decode `Frame`s, assert on the sequence.

## three pieces of plumbing

- **promoted `decode_stream` to `protocol::stream`**, feature-gated on `std`. collector and xtask both consume it. `protocol` itself stays `no_std` for the kernel.
- **`OwnedFrame` enum** — sibling to `Frame<'a>` with the borrows replaced by `String`. the reader thread can't ship a `Frame<'a>` through a channel (it borrows from a buffer the read loop owns); converts to `OwnedFrame` on the way in.
- **harness** — spawn QEMU as the chardev server (`server=on,wait=on`), connect as client, reader thread runs `decode_stream(socket, |f| tx.send(f.to_owned()))`, main thread polls with `recv_timeout` for the wallclock budget. `Drop` always kills QEMU + unlinks the socket so a panicking test still cleans up.

## the chardev-direction gotcha

- first attempt: I run a `UnixListener`, QEMU connects to it (`server=off`). plausible. didn't work.
- swapped to **QEMU is the listener, harness is the client.** identical to what `cargo xtask up` does in production — `server=on,wait=on` blocks QEMU at startup until we connect. wire path is now byte-for-byte the same one the collector uses.
- moral: when integration-testing, **don't invert the production socket roles for convenience.** match the real setup or you risk testing different code paths than ship.

## three scenarios

- **`boot-reaches-heartbeat`** — Hello first → `kernel.boot` SpanStart → `Dropped(0)` after pre-init flush → first `kernel.heartbeat` SpanStart. proves DTB-parse + virtio handshake + pre-init flush + timer IRQ all work, in order. ~3s wallclock.
- **`heartbeat-cadence`** — two consecutive heartbeat spans with monotonic timestamps. proves the IRQ keeps firing across multiple ticks. (initially wanted "interval ± tolerance" but the test doesn't know the timebase without parsing Hello — monotonicity is enough.)
- **`pre-init-order`** — first `StringRegister` on the wire is for `kernel.boot`, and every `SpanStart` we walk past resolves through an earlier `StringRegister`. proves the pre-init buffer drains *in order* — if it dequeued out of order, we'd see SpanStarts referencing unknown ids.

- full run: ~5 seconds. one QEMU process per scenario, per-pid socket path so parallel runs don't collide.

## what each layer earns

| layer | command | what would catch this |
|---|---|---|
| kernel-core unit | `cargo test -p kernel-core` | broke the intern-table allocation logic; broke parent-stack restoration; mis-decoded scause |
| protocol unit | `cargo test -p protocol --features std` | broke wire encoding round-trip |
| collector unit | `cargo test -p collector` | broke span state machine, prom/otlp encoding |
| **xtask integration** | `cargo xtask test` | **boot doesn't reach `kmain`; virtio handshake regression; pre-init buffer drains out of order; timer IRQ doesn't fire; first frame isn't Hello** |

- the integration row is the one we couldn't test before. now we can.

## hardening the collector with mutation testing

- with tests in place across all four layers, the next question was: **are the tests strong enough to catch real bugs?** code coverage says "this line ran." mutation testing says "if I introduced a bug here, would your tests notice?"
- `cargo mutants` via `cargo xtask mutants` — scoped to `collector`, `protocol`, and `kernel-core` (the bare-metal `kernel` binary excluded; it can't build for the host). first run scored **56% — 32 of 82 mutants escaped**.
- categorised the survivors:
  - **genuine I/O** (`main`, `serve`, `export`): can't unit-test without mock servers. marked `#[mutants::skip]` with explanations.
  - **untested logic**: `Histogram::observe`, `format_metrics` (including cumulative Prometheus bucket conversion), `clamp_u128_to_u64`, `Exporter::new` endpoint normalisation. added targeted tests for each.
  - **dead code**: the `else` branch in `tick_to_wall_ns` handled `t < first_t`. but `advance_anchor` guarantees `first_t` is always ≤ any observed tick value — the case is structurally impossible. mutation testing flagged it because no test could kill those mutants. right response: deletion, not more tests.
  - **absolute timestamp gap**: `advance_anchor` mutations all escaped because tests only checked relative *duration*, which cancels out the `first_t` offset regardless of what value it holds. to catch these, you need absolute timestamp assertions — which requires controlling the wall clock.

## the WallClock seam

- `State::new()` was calling `SystemTime::now()` directly — untestable, and the reason `advance_anchor` mutations couldn't be killed.
- fix: inject the clock. `State::new` now takes `impl WallClock + 'static`:

```rust
pub trait WallClock: Send {
    fn now_ns(&self) -> u128;
}
```

- `SystemWallClock` wraps `SystemTime::now()`. `FakeWallClock(u128)` is the test double — `pub(crate)` under `#[cfg(test)]`, shared across `state.rs` and `prom.rs` tests.
- with a pinned wallclock, tests can now pin absolute timestamps:

```rust
let mut s = State::new(FakeWallClock(1_000_000_000)); // wallclock = 1 s at Hello
// ... SpanStart t=100, SpanEnd t=10_100 at 10 MHz ...
assert_eq!(span.start_time_ns, 1_000_000_000); // exactly at anchor
assert_eq!(span.end_time_ns,   1_001_000_000); // 1 ms later
```

- now `advance_anchor` mutations are caught — if `first_t` is set incorrectly, the absolute start timestamp is wrong and the assertion fails.
- one documented equivalent mutant remains: `< vs <=` in `advance_anchor`. `first_t = t` when `t == first_t` is a no-op either way. recorded in `.cargo/mutants.toml` with an explanation rather than silently ignored.
- final mutation score: **100% across `collector`, `protocol`, and `kernel-core`**.

## what i learned

- **`no_std` is a crate attribute, not a target.** the same crate compiles for riscv (when the kernel uses it) and for the host (when tests run it). this is the single most useful Rust fact for kernel testability.
- **the RAII boundary belongs above the abstraction line.** `Drop` can't take parameters, but you can keep the Drop in the binary that has access to whatever it needs to emit, and only put the data structures behind the boundary. moves the wart from the library to the call site, where the call site already had the context anyway.
- **`#[expect(lint)]` self-cleans.** every `#[allow(dead_code)]` is technical debt waiting to go stale; `#[expect]` makes the staleness rustc's problem.
- **don't invert production socket roles for tests.** match the real wiring. otherwise you're testing a different code path than ships.
- **per-test wallclock budget, not per-frame timeout.** flakiness on CI under load is real; budgets are easier to reason about than per-step deadlines.
- **integration tests are unblocked by a one-line skip-on-missing-qemu check.** `which qemu-system-riscv64` → exit 0 if not present. CI without QEMU shouldn't fail the suite; document the dependency instead.
- **mutation testing finds gaps that coverage misses.** a test that only checks duration is invulnerable to mutations in the absolute timestamp math. coverage shows green; mutation score shows the gap.
- **inject the clock.** any function that reads real time is an untestable boundary. a one-method `WallClock` trait with a `FakeWallClock(u128)` test double makes the seam explicit and costs almost nothing.
- **dead code from invariants is best deleted, not tested.** the `t < first_t` branch in `tick_to_wall_ns` was "defensive" code for a case the rest of the module made structurally impossible. mutation testing found it because no test could kill those mutants. the right answer was deletion.

## status

| ✓ | thing |
|---|---|
| ✓ | `kernel-core` lib crate carved out (`#![no_std] #![forbid(unsafe_code)]`) |
| ✓ | 29 host unit tests covering intern table, span registry, pre-init buffer, scause decoding, sink |
| ✓ | `protocol::stream` with `OwnedFrame` + decoder, behind opt-in `std` feature |
| ✓ | `xtask test [scenario]` harness — spawn QEMU, decode frames, deadline-driven assertions |
| ✓ | 3 integration scenarios passing in ~5 s wallclock |
| ✓ | README + project CLAUDE.md document the four test layers and how to add scenarios |
| ✓ | `cargo xtask mutants` — mutation testing across `collector`, `protocol`, `kernel-core` |
| ✓ | `WallClock` injection seam — `FakeWallClock` enables absolute timestamp assertions |
| ✓ | 100% mutation score (all viable mutants caught; one equivalent mutant documented) |

## what's next

- **v0.4 (memory)**: page tables, higher-half kernel, frame allocator, kernel heap. allocator instrumented — heap pressure visible in Grafana.
- **more scenarios**, once we have a kernel-side debug command channel: induced panic test, trap-on-ebreak test, long-cadence stability test.
- everything we build from here lands with both a unit test (in `kernel-core` where it can) and an integration scenario (when it changes the wire).
