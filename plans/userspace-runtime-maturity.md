# Userspace runtime maturity — alloc → `main()` → growable heap → std

**Work lands on:** `main` (no feature branches — see CLAUDE.md)
**Status:** Design / staging. The first increment (alloc MVP) is ready to build;
the rest is a sequenced vision, not committed scope.

The `snitchos-user` runtime is currently `no_std` with **no allocator**:
programs use only fixed/static data (e.g. `span-flood` needs a static literal
name table because it can't `format!`). This plan stages the runtime toward
richer programs and, eventually, a `std`-compatible userspace — **without ever
betraying the capability model.**

## Staging

```
1. alloc MVP        (static .bss arena + #[global_allocator])   — no kernel changes
2. fn main() macro  (#[snitchos_user::main] + global Startup)    — ergonomics
3. growable heap    (brk/mmap-style syscall over mmu::map)       — needed before std
4. std (far)        (custom target + WASI-shaped sys-layer port) — v1.x+ arc
```

Each is additive; 1 is the recommended next increment (it also simplifies the
`worker` programs in `userspace-demo-workers.md` — dynamic span names via
`format!` instead of literal tables).

---

## 1. Allocator MVP ✅ DONE

The runtime implements `alloc` with **no kernel changes**: the heap arena is a
static byte array in `.bss`, which the ELF loader already maps (the same way it
maps the stack). A `LockedHeap` (`linked_list_allocator`, the vendored fork)
`#[global_allocator]` is `init`'d over it in `__snitchos_start` before any
program runs. Programs add `extern crate alloc;` and get `Vec`/`String`/`Box`/
`format!`. `hello` now names its span via `format!` (proving the path
end-to-end: `String` → `copy_from_user` → intern → `SpanStart`); 64 KiB fixed
arena; 10/10 flake-clean. The `Heap` supports `extend`, so step 3's growth
needs no allocator swap.

```rust
// snitchos-user:
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];   // .bss → loader-mapped
#[global_allocator]
static ALLOC: LockedHeap = LockedHeap::empty();
// in __snitchos_start, before calling main:
unsafe { ALLOC.lock().init(addr_of_mut!(HEAP), HEAP.len()); }
```

- **Allocator:** the workspace's forked `linked_list_allocator` (free support,
  already a dependency), or a bump allocator if we want dead-simple-no-free.
- **Symmetric with the kernel:** the kernel bootstrapped its heap as a static
  region first, grew later. Userspace gets the same staged story.
- **Payoff:** programs add `extern crate alloc;` and get `Vec`/`String`/`Box`/
  `format!`. `span-flood`/`worker` can build names dynamically.

**Acceptance criteria:** a U-mode program does `format!`/`Vec` work and emits a
result observable on the wire (e.g. a span named from a `format!`ed string).
Fixed-size; running out is a clean alloc-error (abort/panic → spin), not UB.

**Open:** `HEAP_SIZE` (start ~64 KiB); a large `.bss` arena grows the mapped
region but not the file image (`.bss` has `filesz=0`).

## 2. A normal `fn main()` ✅ DONE — *no macro*

Briefly built as a proc-macro (`#[snitchos_user::main]`), then **deleted the
macro entirely** in favour of the runtime calling an `extern "C" fn main()`
directly. On stable `no_std` you can't get a zero-decoration `fn main()` (the
`main` lang item needs std/nightly), so a program writes:

```rust
#[unsafe(no_mangle)]
extern "C" fn main() {
    let _span = snitchos_user::tracer().span("hello.work");
}
```

`__snitchos_start` inits the heap, stores the two startup handles into runtime
atomics, calls `main()`, then `exit`s. The free accessors
`snitchos_user::tracer()` / `telemetry()` read the atomics — the std-like shape
(`main()` takes nothing; you call library fns for your environment). The
`Startup` struct is gone (the caps are just the accessors). The `#[no_mangle]
extern "C"` decoration is the honest no_std entry tax; the trade vs a proc-macro
crate is worth it. All four programs converted; 10/10 flake-clean.

### Original design notes (superseded — kept for context)

Std's trick: `main()` takes nothing; you call library functions for your
environment. Same here — stash `Startup` in a runtime global at
`__snitchos_start`, expose `snitchos_user::tracer()` / `telemetry()` free
functions that read it, and add an attribute macro that generates the
`rust_main(startup)` shim:

```rust
#[snitchos_user::main]
fn main() {
    let _span = snitchos_user::tracer().span("hello.work");
}
```

- Cost: a small proc-macro crate (`snitchos-user-macros`), or a
  `macro_rules! entry!` to avoid the proc-macro dependency.
- Pattern: cortex-m-rt `#[entry]` / `#[embassy::main]`.
- The capability accessors reading a global mirror how `std` exposes the
  environment (you call `std::env::args()`, you don't receive it as a param).

## 3. Growable heap ✅ DONE — *mmap-shaped, not `sbrk`*

Built as a **`MapAnon`** syscall (abi=5), *not* `sbrk` — `sbrk`/`brk` is the
legacy single-break abstraction (musl doesn't use it at all); the modern,
**capability-aligned** primitive is mmap (region-returning, individually
unmappable, eventually a `MemoryRegion` capability — the slot the `cap::Object`
enum already reserves). `MapAnon(bytes) → base`: the kernel maps fresh zeroed
frames into the process root (`mmu::map_in` + local `sfence.vma`), tracks
`Process.heap_top`, caps at `HEAP_MAX = 16 MiB`, refuses with
`SyscallRefused{OutOfMemory}`.

Userspace swapped from `linked_list_allocator` to **`talc`** (multi-region; the
kernel heap keeps `lla`). `talc`'s `OomHandler` is the grow-on-demand hook: on
allocation failure it `map_anon`s a region (request + 64 KiB headroom) and
`claim`s it. **Lazy** — no startup map; the first allocation triggers the
first `map_anon`. `talc` doesn't assume regions abut, so the kernel's
bump-pointer placement can become disjoint + add `munmap` later, no ABI break.

`heap-grow` program allocates 512 KiB (past one region), writes + sums it, and
emits the sum — `heap-grows-on-demand` asserts `524288`; 0/10 flake-clean.

Not yet (vs real libc `malloc`): **demand paging** (real `mmap` is lazy —
frames on first touch via a user page-fault handler; ours is eager),
`munmap`/return-to-OS, and per-thread arenas.

## 4. `std`-compatible userspace — the north star, scoped WASI-shaped

A `std` port (custom `riscv64-snitchos` target + porting std's `sys` layer) is
the integral of the whole roadmap. **Scope it from the start as WASI-shaped**,
not full ambient POSIX std.

### 4a. `snitchos-std` facade ✅ STARTED — the stepping stone

Real std needs a custom *target* + nightly `build-std` + a `sys` port (and only
*that* drops `#![no_std]` / runs external `std` crates). As a stable stepping
stone, **`snitchos-std`** (`user/std`) is a std-*shaped* facade over `core` +
`alloc` + `snitchos-user` (our `sys` layer). Programs still `#![no_std]`, but
can write std-idiomatic code where it's wired. Reading the crate is the "what's
left of std" map: **wired** — `thread::yield_now` (→ `Yield`),
`process::exit`/`abort` (→ `Exit`), and the free `core`/`alloc` re-exports
(`Vec`/`String`/`format!`/`BTree*`/`Arc`/`Duration`); **`todo!("…why…")`** —
`io::println` (needs `DebugWrite`, the iconic next step), `time::Instant`
(read-clock syscall), `thread::spawn`/`sleep`, `sync::Mutex`,
`collections::HashMap`, and — encoding the capability constraint, not POSIX —
`fs`/`net`/`env` as *capability-rooted or unsupported*. `worker` drives its
yield through `snitchos_std::thread::yield_now`, proving the wire end-to-end.
The eventual real-target `sys` backend reuses this mapping.

Next stub to fill: `io::println!` via a `DebugWrite` syscall → snitched `Log`
frame (observable + testable). Then the facade grows one `todo!` at a time.

### The split

- **Non-namespace parts map cleanly:** `alloc` (step 1), `thread` (v0.5 kernel
  threads), `time` (the kernel clock), `sync`, `io` (stdout/stderr → a
  debug-write capability). These are the "easy" half.
- **Ambient-namespace parts must be capability-rooted or unsupported:** `fs`,
  `net`, `env`, `process`. This is the hard half, and the design constraint
  below governs it.

### Design constraint — std's ambient surface is capability-rooted or it errors

**This is the load-bearing rule. `std::fs` may exist only as a capability-rooted
lens, never an ambient one.**

`std::fs::File::open("/abs/path")` is ambient authority by construction — a
global namespace addressed by string, no capability held. That is exactly what
the capability model rejects ("naming a thing is not authority"). Every
capability OS hit this and converged on the same answer:

- **Capsicum**: in capability mode `open()` by absolute path is forbidden; only
  `openat(dir_fd, …)` — the fd *is* the capability.
- **CloudABI**: removed the ambient syscalls from libc; handles at startup,
  `openat` only.
- **WASI** (Rust already ships `wasm32-wasi`): modules get **preopened
  directory handles** at startup and `path_open` only *relative* to them.
  `std::fs::File::open("data/x")` works — resolved against a granted preopen;
  absolute paths and `..`-escapes fail.

So the resolution: **std keeps its types (`File`, `Path`, `Read`/`Write`); it
loses ambient semantics.** A path is a *name within a capability-rooted
namespace* (`openat` on a granted dir cap), not a global address.

**"No POSIX compat" is freeing here:** we owe Rust programs the *types*, not
POSIX *behavior*. So the inherently-ambient ops (absolute paths,
`canonicalize`, `current_dir`, `..`-escape) return `Unsupported`; only the
capability-rooted subset works — exactly as `wasm32-wasi` already does.

### Why this lands cleanly for SnitchOS

1. **The native FS is already capability-first** (v0.10 `Filesystem` trait
   accessed via capabilities). `std::fs` is an *optional veneer* over it, never
   the primary interface. Programs wanting real power (attenuate a dir cap to
   read-only before passing it to a child — POSIX fds can't) use the native
   API.
2. **On-thesis and observable:** a file open is a capability invocation →
   snitched; an "open denied" is a `SyscallRefused`-style event (the mechanism
   already built). The FS firewall is observable, like the U-bit firewall.
3. **Capabilities are *richer* than POSIX** (attenuation, subtree delegation),
   so std::fs is a lossy lens over a more expressive substrate — not a
   dumbing-down.

The pattern generalizes: `net` (a socket is a granted endpoint capability),
`env`, `process` (exec needs a capability to a program/executor) all follow the
same "capability-rooted handle, no ambient namespace" rule. FS is just the
loudest case.

### Recommendation

Lead with the **capability-native `Filesystem` API**; treat `std::fs` as a
deferred, explicitly-sandboxed veneer added only on demand. **Resist any
ambient escape hatch** that lets a process open files it holds no capability
for. "std-compatible" = the cleanly-mapping core + capability-rooted veneers,
never ambient POSIX std.

---
*Delete this file when the staging is complete (or fold the surviving design
notes into a doc under `docs/`). If `plans/` is empty, delete the directory.*
