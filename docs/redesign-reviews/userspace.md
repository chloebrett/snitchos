# Redesign review: Userspace (`user/`)

*Outcome of the [redesign-from-scratch](../redesign-from-scratch.md) exercise —
2026-07-04.*

**Status:** #5 (honest `std`) and #4 (runtime auto-instrumentation — per-process
root span) shipped 2026-07-04. #5 fully closed, incl. the timebase→`Instant`
follow-up: a new `ClockFreq` syscall plumbs the DTB timebase to userspace so
`std::time::Instant` converts ticks to a real `Duration` with no hardcoded rate
(and `stitch_repl` dropped its `TICKS_PER_MS = 10_000` hardcode). #4 also surfaced
+ fixed a latent build-staleness bug (`kernel/build.rs` now derives its
`rerun-if-changed` set from `cargo metadata` + `USER_PROGRAMS`). #1–#3 (the big
cluster) outstanding. #6 (supervision tree) has a design —
[supervision-design.md](../supervision-design.md) — but is unbuilt.

Central framing: this isn't really a userland yet — it's a museum of
kernel-feature demos wearing userland clothes. ~35 binaries, nearly all existing
to prove one wire-frame sequence to one itest. That framing drives the list.

Ranked changes:

1. **Programs are data in the FS, not statics baked into the kernel.** Adding a
   program today touches ~6 places (`build.rs` env var, `include_bytes!` static,
   `ProgramSpec`, `LAYOUTS` row, `WorkloadKind` variant, `bootargs` parse arm) —
   the kernel is a program registry. Redesign: kernel embeds one initramfs blob
   and starts one program (`init`); everything else is a file loaded from the FS.
   The `spawn-image-demo` (loads an ELF off ramfs and spawns it) is the seed of
   *the* mechanism, currently used for one demo. Two-stage boot resolves the
   FS-server-is-a-program chicken-and-egg.

2. **Typed, named startup ABI instead of positional handles.** Programs hardcode
   `delegated_handle(0)`; startup set is "telemetry@0, span@1, delegated@2+" — a
   fragile positional contract, the exact thing caps should abolish, reintroduced
   at the boundary. Redesign: born with one bootstrap namespace-of-caps, ask by
   role/type (`bootstrap.get::<Endpoint>("fs")`). Ties into the existing
   `user.iface`/`hitch` manifest work: a process *declares* required authorities;
   the spawner satisfies them by name and type, all-or-nothing. **Designed:**
   [manifest-design.md](../manifest-design.md) (2026-07-05) — highest-fan-out item;
   five consumers (spawn, shell `~>`, supervision, checkpoint, Stitch `uses`).

3. **Collapse the demo bins into one scriptable conformance program.** The ~30
   test-fixture ELFs are the tail wagging the dog — each is load-bearing for an
   itest, so the userland can't be refactored. Redesign: one `probe` program that
   runs a *script* of syscalls (data, from the FS); the itest corpus becomes a
   directory of scripts, not ELFs. The real userland then shrinks to `init`, a
   shell, the FS server, and a couple of services.

4. **Instrument in the runtime, not by hand at each call site.** `fs-client` had
   `{ let _s = tracer().span("fs.write"); cap.call(…) }` copy-pasted 9× (factored
   into a `traced_call` helper on 2026-07-04 — a band-aid). For an
   observability-first OS the syscall layer should open the span + emit the
   `CapEvent` automatically; programs emit only *domain* spans. Opt-in-per-call
   observability means a forgetful program is invisible — the opposite of the
   thesis.

5. **One programming model; make `std` honest.** `snitchos_std` is schizophrenic:
   POSIX-shaped `println!`/`thread::yield_now` over a capability substrate, plus
   `todo!()` stubs (`thread::spawn`, `sleep`, `Instant`, `Mutex`, `hashmap`)
   advertising a non-existent API. Redesign: commit to capability-shaped
   all-the-way; `std` is thin and *complete*, nothing aspirational in the surface.

6. **Supervision as a first-class tree.** Lifecycle is ad-hoc (`reaper` loops
   `Wait`, `supervisor` does `WaitAny`, `init` reaps one child). Redesign: `init`
   is a real service supervisor — services declare dependencies + restart policy,
   brought up in order, crash → restart, every transition a span. This is the
   `userland-actor-model-design` doc made structural; it's what makes a live
   supervision tree visible in Grafana.

**Keep:** capabilities as the security primitive, rendezvous IPC with badges, the
`no_std` + talc runtime, and above all the observability-first stance. Don't touch
the foundations — push them *deeper* (2, 4, 6 all take a good idea and make it
structural instead of per-program).

**One-thing pick:** #1. Programs-as-files dissolves #3's registry problem for free
and is the precondition for a manifest-driven #2.

**Caveat:** the sprawl is what a working, honestly-tested bring-up actually looks
like. The redesign is the second pass, once you know which programs were
scaffolding and which were the building.
