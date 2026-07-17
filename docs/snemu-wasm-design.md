# 🌐 snemu-wasm — SnitchOS in a browser tab

*The browser is just another embedder. snemu's library core already compiles to
`wasm32-unknown-unknown` unmodified; what's missing is a host shim, not a port. This
document records why that's true, what the browser actually needs, and where the MVP
line falls.*

Status: **DESIGN** (2026-07-17). Companion to [snemu-design.md](snemu-design.md)
(the emulator itself, BUILT) and [framebuffer-design.md](framebuffer-design.md)
(which chose ramfb *for* this path). Implementation plan:
[plans/snemu-wasm.md](../plans/snemu-wasm.md).

## The finding that reframes the work

The browser goal has been named in this repo for a long time — the roadmap has a
[WASM milestone](roadmap-and-milestones.md), the arcade doc's §11 works through the
forks, and
[snemu-08](../posts/snemu-08-zero-to-a-hundred-in-two-seconds-flat.md) states the bet
outright:

> the entire reason backend A is a portable, `unsafe`-free interpreter and not a
> native JIT is so it can run inside a wasm sandbox.

The bet paid. **`cargo build -p snemu --lib --target wasm32-unknown-unknown` succeeds
today, with no changes.** Verified live, not assumed. The consequences are worth
stating precisely, because they invert the expected shape of this work:

- **`jit.rs` excludes itself.** Its *inner* attribute
  `#![cfg(all(target_arch = "aarch64", target_os = "macos"))]` empties the module off
  that platform, and `cpu.rs` carries a paired
  `#[cfg(not(...))] fn run_block_native(..) -> Option<u64> { None }` that falls back to
  Backend A. wasm gets the portable interpreter automatically. (The doc comment at
  `jit.rs:5` claims the gate is `cfg(not(wasm))` — it isn't; that comment is stale.)
- **`libc` and `minifb` are already scoped** under
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`.
- **The lib has no host coupling at all.** No `fs`, threads, sockets, entropy, or
  clock. The only `std::time` reference in the crate is `Duration` in `bench.rs` — a
  plain data type, not `Instant`.
- **The clock is `instret`-driven** — one tick per retired instruction. The browser
  build is deterministic *for free*; no `Date.now()` is needed or wanted.
- **`Machine` is already pull-based**: `step()` (one step, non-blocking, no internal
  run loop), `uart_output() -> &[u8]`, `virtio_tx_output() -> &[u8]`,
  `framebuffer_pixels() -> (Vec<u32>, w, h)`, `push_console_input()`. No device writes
  to stdout, a socket, or a file — devices accumulate into `Vec`s and the embedder
  drains them at its own cadence.
- **The kernel arrives as bytes**: `loader::load_machine(image: &[u8], ram_size, dtb,
  harts)`.

The decisive evidence is not any of the above individually, but that
**`xtask/src/itest/harness.rs` is already a second embedder**: it holds a
`snemu::machine::Machine` as a field and drives `step()` in a loop, in-process. A
browser host plays the identical role. The core/host split isn't aspirational — it's
load-bearing in the suite that gates every commit.

> [snemu-ramfb-model.md](../plans/legacy/snemu-ramfb-model.md) said snemu has "*zero*
> wasm/canvas/postMessage groundwork". That was accurate about the **shim** and remains
> so. It undersold the **core**, which was already there.

## What this actually costs

The work is a shim and a web page. The emulator does not change.

| Concern | Status |
|---|---|
| Interpreter, MMU, devices, traps, ELF load | **Done** — compiles to wasm today |
| Determinism / clock | **Done** — instret is the clock |
| Console output, telemetry bytes, framebuffer pixels | **Done** — all pull-based accessors |
| Console input | **Done** — `push_console_input()` exists |
| `cdylib` + `wasm-bindgen` + JS shim | **The work** |
| Step budgeting (don't freeze the tab) | **The one design decision** |
| Wall-clock pacing | Deferred — only matters for an idle interactive tab |
| virtio-input device | Deferred — UART carries console input already |

### The 128 MiB question, answered

The instinct is that a `vec![0; 128 * 1024 * 1024]` is a problem in a browser. It
isn't: wasm32 has a 4 GiB address space, this is a single `memory.grow`, and browsers
routinely allow far more. Further, `RAM_SIZE` is a **`main.rs` constant, not a lib
one** — `Memory::new(size)` is parameterized, so the host picks. And `high_water`
right-sizing already exists (the guest's real footprint is a few MiB of a much larger
machine). Non-issue.

### Perf, bounded

The whole 111-scenario suite is ~245 M guest instructions in ~2 s natively. A single
boot-to-heartbeat is a few million. Even at wasm's typical 2–3× interpreter penalty
versus native, that is comfortably sub-second in a tab. Backend B stays host-only and
is not missed: startup, not throughput, is what the browser demo is bounded by — the
same argument [snemu-design.md](snemu-design.md) makes for the itest suite.

## Decisions

### A separate `snemu-wasm/` crate, not a `cdylib` on `snemu`

Making `snemu` itself a `cdylib` would force feature-gating `clap` (a `main.rs`-only
dep) and drag windowing/CLI concerns into a crate whose whole virtue is that it has
none. A separate workspace member depending on `snemu` keeps the core untouched and
the boundary honest — the same instinct that split `kernel-*` out of `kernel/`.

### A pure core with a thin `#[wasm_bindgen]` shell

This is the repo's existing doctrine applied one layer out. `kernel/` holds the
statics and asm; the `kernel-*` crates hold the decisions and are host-tested.
`framebuffer.rs` already models it in miniature: `to_minifb_buffer` is a pure,
host-tested function, and `machine.rs` only wraps it.

So: **every non-trivial behaviour in `snemu-wasm` lives in pure functions tested with
`cargo test` on the host** (the drain cursor, the RGBA pack, the status encoding). The
`#[wasm_bindgen]` layer is a shell too thin to hide a bug. This keeps TDD intact for a
target that is otherwise awkward to test, and it is why the crate is worth its own
`src/` rather than being a pile of glue.

`cargo xtask test` picks it up **automatically** — `run_unit_tests` derives its list
from `workspace_members()` (cargo metadata) minus the `NOT_HOST_TESTED` opt-out. There
is no list to remember to update, which is exactly the trap that made
`kernel-devices` silently untested during the kernel-core split.

### Text first. Not the canvas.

The instinct is that "SnitchOS in a browser tab" means pixels. The MVP shouldn't.

1. **The default boot is `init`** — a userspace root that draws nothing.
   `enable_fwcfg_ramfb()` is opt-in, so a canvas MVP *also* needs a drawing workload
   wired up and selected. That's a second project riding along.
2. **The telemetry is the product.** SnitchOS's first-class concern is observability;
   a boot log plus a live span tree *is* the thing worth showing. The arcade doc names
   precisely this as the portfolio showpiece — "the **SnitchOS boot log + live span
   tree as the loading screen** (latency-as-spectacle) … the page shows its own live
   heap/scheduler."
3. **`protocol` is already a dependency with `features = ["std"]`.** The wasm module
   can decode its own `Frame`s in-process via `protocol::stream` and hand JS structured
   spans — no collector, no socket, no Tempo. That's the "trace-within-a-trace" from
   [snemu-design.md](snemu-design.md), running in a tab, for free.

The counterintuitive call: **the cheapest MVP is also the most SnitchOS-shaped one.**
Pixels are increment 2, and by then ramfb's device model is already proven by the PPM
dump.

### `requestAnimationFrame` with an instret budget; a Worker later

`step()` is *one instruction*. A naive `while !done { step() }` freezes the tab
forever — this is the only way to get the architecture wrong.

Two options: budgeted stepping on the main thread under rAF, or a Web Worker with
`postMessage`. **rAF for the MVP** — simpler, directly debuggable, no marshaling, and
the budget knob (~2 M instret/frame) is exactly the kind of thing you want to tune by
hand while learning what the page feels like. A Worker is the upgrade once jank
actually bothers someone; it changes the shim's edges, not its core.

### Fetch the kernel; don't `include_bytes!` it

The release kernel is 1.8 MB (debug is 12.8 MB — the browser build wants release).
Fetching it keeps the wasm module small and cacheable, and lets the page **swap
kernels without a rebuild**.

That last point is worth more than it looks. `dtb.rs` already patches bootargs in a
firmware role — which means the page can drive `workload=<name>` selection at runtime,
exactly as `cargo xtask boot --workload smp` does. A `<select>` that reboots the
machine into a different workload falls out of machinery that already exists.

### Determinism is preserved, and it's the differentiator

Nothing in this design introduces a host clock, entropy, or thread. The browser build
is as deterministic as the itest suite: same kernel, same bootargs, same instret →
same run. `Machine::state_hash()` works in a tab. This is what makes the
scrub-backwards / replay ideas in the cross-cutting-axes brainstorm reachable later
rather than fantasy — and it is not a property jslinux-style demos have.

## Deferred, with reasons

- **Wall-clock pacing.** [scaling-down-snitchos.md](scaling-down-snitchos.md) names the
  gap exactly: on `wfi` with nothing pending, snemu fast-forwards the emulated clock to
  the earliest armed deadline rather than *host-sleeping* until it. Emulated time races
  ahead; a genuinely idle tab would pin a core. The fix is a small snemu-side addition
  on machinery that already exists — and it is irrelevant to a demo that boots, prints,
  and stops. Revisit when the tab is meant to sit open.
- **virtio-input.** Console input already works via the UART's `push_input`. A real
  input device is for the physics desktop, not the boot log.
- **Backend B in the browser.** Out of scope by construction; wasm gets Backend A. If
  throughput ever bounds the demo, the answer is a wasm-targeted Tier 3, which is a
  different project.
- **Convergence with `viz/`.** [snemu-itest-packing-viz-design.md](snemu-itest-packing-viz-design.md)
  plans a React/Vite `viz/` workspace (not yet on disk) explicitly "extensible into
  other snemu viz … later". The MVP page deliberately stays a no-build static page
  (`wasm-pack --target web` emits an ES module that `<script type="module">` loads
  directly). If both exist and the duplication chafes, merge then — on evidence.
- **Sessions / relay / networking.** The arcade doc's §11 wants virtio-net → WebSocket
  and a session relay. Far-future; needs a network stack first.

## A correction to the record

[arcade-and-real-hardware-direction.md](arcade-and-real-hardware-direction.md) §11
sketches Option B as wiring "virtio-gpu → `<canvas>`". That predates
[framebuffer-design.md](framebuffer-design.md)'s call, which chose **ramfb** over
virtio-gpu *specifically* on the browser axis ("~50 lines: model as a guest-RAM region
+ a pixel format; each present, copy → `ImageData` → `putImageData` on a `<canvas>`").
The ramfb stance is newer and more specific; §11 is stale on that detail. Its larger
framing — **B ports the *guarantees*, B′ ports the *experience*** — still stands, and
this document is squarely Option B.

## Milestones

1. **Boot log in a tab** — fetch kernel, boot, stream UART text, decode telemetry
   frames to a live span/metric view. No canvas, no input, no pacing.
2. **Pixels** — `enable_fwcfg_ramfb()` + a drawing workload; `framebuffer_pixels()`
   (`0x00RRGGBB`) → `ImageData` (RGBA). ~10 lines of conversion apart.
3. **Input** — wire keystrokes to `push_console_input()`. The Stitch shell in a
   browser tab.
4. **Pacing** — wall-clock-paced idle so the tab can sit open honestly.
