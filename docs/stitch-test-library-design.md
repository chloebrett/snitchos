# Testing Stitch programs — a small test library

**Status:** **Design (captured 2026-06-28). Pre-implementation.** The TDD substrate
for [the Stitch shell](shell-primitives-design.md). The shell is the first
*substantial* Stitch program, and TDD is non-negotiable here — so before building
it we need a way to make assertions about Stitch *code* (not just the interpreter's
Rust internals). This doc is that library's design.

## What already exists (don't rebuild it)

From a map of `stitch/`:

- **Host semantic testing is solved.** `stitch/src/test_support.rs` provides `run`,
  `run_program`, `run_program_events`, `run_modules` — parse + evaluate a snippet
  and get a `Value` (or telemetry events) back. The interpreter's own tests use
  these (`interp.rs` §tests). Asserting "this `.st` evaluates to that value" on host
  *already works*.
- **One effect is injectable: telemetry.** A `Telemetry` trait (`telemetry.rs`)
  with two impls — `RecordingTelemetry` (host: buffers events in memory) and
  `RuntimeTelemetry` (`#[cfg(target_os = "none")]`: routes `emit`/`span` to real
  syscalls). The native `emit` is a static fn that calls `env.span_open()` →
  the injected `Rc<dyn Telemetry>`. **This is the pattern to copy.**
- **`on X` is static + type-directed** (`interp.rs::eval_method_call`): it dispatches
  on a value's *type*, not a runtime string. (Consequence for the shell: parse the
  verb word into a command constructor, then `on` dispatches on the type — see the
  primitives doc §6.)

## The actual gap: the effect seam is too narrow

Natives are a **static `const NATIVES` slice** (`natives.rs`) — not pluggable. Only
*telemetry* effects route through an injectable trait. But the shell's effects —
`readLine`, `write`, `hold`, `spawn`, `lookup` — go far beyond telemetry, don't
exist yet, and have no seam. **That** is what blocks TDD-ing the shell: you can't
run the shell's `.st` against a fake console / fake cap-table / fake spawn and
assert on what it did.

So the test library's load-bearing deliverable is **not** a new assertion harness —
it's **widening the effect seam**, following the telemetry precedent exactly.

## The core move: generalize `Telemetry` → an injectable `Platform`

`Env` already carries `Rc<dyn Telemetry>`. Add `Rc<dyn Platform>` (console, caps,
proc, fs) alongside it (or fold telemetry into one `Platform` trait — open
question). Each new effectful native is a static fn that calls
`env.platform().<op>()`, mirroring how `emit` calls `env.span_open()`. Two impls,
exactly like telemetry:

| impl | where | does |
|---|---|---|
| `RuntimePlatform` | `#[cfg(target_os = "none")]` | wraps the real syscalls (`ConsoleRead/Write`, `CapList`, `Spawn`, `Wait`, FS IPC) |
| `FakePlatform` | host | scripted input, **records** output + every spawn/grant for assertions |

A host shell test then captures the demo's whole claim as a pure assertion — no
QEMU, no frame decode:

```rust
let fake = FakePlatform::with_input("view notes\n");
run_shell(SHELL_SRC, &fake);
assert_eq!(fake.spawned(), &[("view", vec![(notes_inode, READ)])]); // granted ONLY that
assert!(fake.output().contains("touched only notes"));
```

This is the [`WallClock` injection pattern](../README.md) the collector already
uses, applied to the whole effect surface. It is also good *for the shell anyway*:
it forces the native surface to be a clean seam from day one.

## The two-runners-one-corpus question — scoped

A single corpus of `.st` tests (using a tiny Stitch `expect` vocabulary) run by two
runners — a host Rust runner and an emulator workload — is appealing, but the win
is **concentrated, not uniform**. Split the corpus by what it tests:

### Language semantics → one corpus, two runners ✅ worth it
The win is a **host/metal parity oracle**, and it's sharp because of one config
fact: **the host test build pulls in `std`** (insta + `protocol/std`), while the
metal runs **`no_std`**. A test green on host is green against a *different build
than ships*. Running the *same* corpus on both runners catches exactly what that
gap hides — accidental `std` deps, `talc`-vs-host allocator behavior, riscv codegen,
the real vs fake platform diverging. With two *separate* suites you can't diff;
with one corpus, runner divergence *is* the bug signal. Cost is low (assertions are
over plain values). **This is the one-sentence justification: a std/no_std parity
guard for the interpreter.**

### Shell / effect logic → host Rust + `FakePlatform` ✅ don't force into the corpus
The valuable shell assertions are *"what got spawned, which cap was delegated"*. To
assert those *in Stitch* you'd have to expose the fake's recorded effects back into
Stitch as values — extra plumbing to *lose* introspection power. In Rust you just
inspect the fake. So effect tests stay host-Rust; the parity win doesn't apply
(there's no metal-semantics question, just "did the fake record the right thing").

### Layering summary

| Layer | Build | Runs | Tests |
|---|---|---|---|
| **Host semantics** | reuse `test_support` + a Stitch `expect` corpus | `cargo test` (std build) | language behavior |
| **Host shell/effects** | `FakePlatform` + `run_shell` harness | `cargo test` | shell logic, cap-delegation — the bulk of shell TDD |
| **Emulator smoke** | a `stitch-tests` workload running the **same semantics corpus** via `RuntimePlatform` | QEMU itest, asserts via frames | std/no_std parity + real bindings |

Host is the workhorse. The emulator layer is thin and exists for the parity oracle,
not to re-test the language.

## The `Platform` trait (pinned, 2026-06-28)

Lives in `stitch/src/platform.rs`, mirroring `telemetry.rs`. Pure Rust types at the
boundary (not `Value`) — the *native* is the adapter (handle→`Int`,
`CapInfo`→`Data` record), so the trait stays interpreter-agnostic and host-testable
in isolation.

```rust
pub type Handle = u32;   // index into the process's CapTable (abi handle)
pub type Rights = u32;   // abi::rights bits
pub type TaskId = u32;

pub struct CapInfo {
    pub handle: Handle,
    pub kind: ObjectKind,   // TelemetrySink | SpanSink | Endpoint | Notification | File | Unknown
    pub rights: Rights,
    pub badge: u64,
}

pub enum PlatformError { NotHeld, WrongRights, NotFound, Refused }

/// What happens when a Stitch program touches the outside world — decoupled from
/// the natives that trigger it. `&self` + interior mutability so one backend is
/// shared via `Rc` across every Env clone (exactly like `Telemetry`).
pub trait Platform {
    // --- console (slice 1) ---
    fn read_line(&self) -> Option<String>;   // a finished line, no '\n'; None = end-of-input
    fn write(&self, s: &str);

    // --- caps (slice 1) ---
    fn hold(&self) -> Vec<CapInfo>;

    // --- fs (slice 2) ---
    fn lookup(&self, dir: Handle, name: &str, rights: Rights) -> Result<Handle, PlatformError>;

    // --- proc (slice 2) ---
    fn spawn(&self, program: &str, caps: &[Handle]) -> Result<TaskId, PlatformError>;
    fn wait(&self, child: TaskId) -> Result<i32, PlatformError>;
}
```

Three impls, paralleling the telemetry backends:

- **`NullPlatform`** — `Env::new`'s default (no input, discards output, empty
  `hold`, refuses the rest). Keeps existing semantic tests untouched.
- **`FakePlatform`** (host) — scripted input; **records** output + every
  `spawn`/`lookup`/grant for assertions.
- **`RuntimePlatform`** (`#[cfg(target_os = "none")]`) — wraps the real syscalls.

### Decisions baked into the shape

1. **Pure Rust types at the boundary**, not `Value` — the native adapts. Avoids
   pinning the Stitch representation of a cap now.
2. **Line discipline is *not* in the trait.** `read_line` returns a *finished line*.
   The byte-level editing (echo/backspace/enter) is a separate **pure helper**
   `edit_line(state, bytes) -> (line?, echo_bytes)` that `RuntimePlatform` drives —
   so the fiddly logic is host-tested as a pure function, the metal echo loop stays
   thin, and the fake just pops the next scripted line. The single most important
   shape call.
3. **`attenuate` dropped from v1.** The only attenuation the first shell does is
   *file narrowing* = `lookup(dir, name, rights)` (the FS mints at the requested
   rights). Endpoint-mint (`MintBadged`) waits for endpoint re-delegation. YAGNI.
4. **`read_line -> Option`**, `None` = end-of-input — drives the REPL recursion's
   termination, not a sentinel.
5. **One bundled trait; telemetry stays separate** (resolves the two open questions
   below) — simplest to thread through `Env`, one fake; sits *beside* `Telemetry`.

`Env` wiring follows the telemetry precedent exactly: add `platform: Rc<dyn
Platform>`, `with_platform`, accessor methods, and `Rc::clone` it at the four
env-derivation sites (`child`/`globals_only`/… — the same sites that clone
`telemetry`).

## Build list (small, in order)

1. **`Platform` trait + `FakePlatform`** (host) — console first (`readLine`/`write`),
   then caps/proc/fs as the shell needs them. The fake records output + spawns +
   grants. *The load-bearing piece.*
2. **`run_shell(src, &impl Platform) -> RunResult`** — thin harness over the seam +
   existing `test_support`. The shell-TDD entry point.
3. **A Stitch `expect` vocabulary** — a handful of natives/`.st` helpers
   (`expect(x).toEqual(y)`) for the semantics corpus.
4. **`RuntimePlatform`** (`target_os = "none"`) — wraps the real syscalls; mirrors
   `RuntimeTelemetry`. Needed when the shell runs on metal (and for the smoke
   runner).
5. **`stitch-tests` emulator workload + itest scenario** — runs the semantics
   corpus on metal, reports pass/fail via telemetry frames. The parity oracle.

(1) and (2) unblock shell TDD immediately on host. (4)/(5) are additive and can lag.

## Open questions

- ~~One `Platform` trait or several~~ — **resolved: one bundled trait** (see above).
- ~~Telemetry folds into `Platform`?~~ — **resolved: stays separate**, beside it.
- **Stitch representation of a cap** — `Int` handle (v1, matches the ABI) vs an
  opaque cap `Value` (nicer for the enforced-grammar phase). Deferred to the native
  layer; the trait is unaffected either way.
- **Corpus test-reporting convention on metal** — how a `.st` assertion failure
  becomes a wire frame the itest asserts on. Deferred to step 5.
