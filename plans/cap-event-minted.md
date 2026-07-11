# Plan: `CapEvent::Minted` — self-minted authority as a first-class provenance kind

**Branch**: main (all work lands on main)
**Status**: Active

## Goal

Add a `CapEventKind::Minted` wire kind meaning *"the holder created this capability itself via a syscall"* — emitted by `EndpointCreate` and `NotifyCreate` — so the capability derivation tree distinguishes self-minted authority from kernel-installed bootstrap authority (`Granted`) and derived/delegated authority (`Transferred`).

## Why this shape (decision record)

The genesis event is **not** missing — `CapEventKind::Granted` already fires when a cap is created from nothing (`parent_cap_id == 0`), and the tree is fully reconstructable today (`diagram::caps::derivation_tree`). So `Minted` is not "the missing birth frame"; it's a **re-carving of `Granted`** along a sharp, kernel-observable predicate.

`Granted` currently conflates two provenances that split along *different* axes:
- **Object-creation axis** (was a new kernel object born?) and
- **Self-service axis** (did the holder create it, or was it installed at spawn?)

These agree on `EndpointCreate`/`NotifyCreate` but **disagree on the `run_ipc` bootstrap endpoint** (a new endpoint object, but installed at spawn). Choosing the *object* axis leaves that case undefined. We chose the **self-service axis**: `Minted` = *the process minted it itself via syscall*. The `run_ipc` bootstrap endpoint stays `Granted` (installed at birth); only the two Create syscalls become `Minted`. Every emission site then lands on exactly one row — the predicate is just *which handler emits it*, so there are no judgment calls.

Resulting provenance lifecycle:

| kind | provenance | sites |
|---|---|---|
| `Granted` | born holding it (kernel-installed at spawn) | bootstrap sinks, `run_ipc` endpoint |
| `Minted` | **made it myself via syscall** | `EndpointCreate`, `NotifyCreate` |
| `Transferred` | handed / derived to me | delegation, `MintBadged`, reply-cap |
| `Revoked` | reclaimed | `Revoke` |

The birthday framing lives in the **renderer**, not the wire: `Minted` frames carry a timestamp, so "a capability's birthday" is a Stitch/collector rendering concept. The protocol stays sober.

## Acceptance Criteria

- [x] `CapEventKind::Minted` exists as the **last** discriminant (append-only; postcard is positional) and roundtrips through postcard intact. *(discriminant `03`; `round_trips_a_minted_cap_event_over_postcard`)*
- [x] The whole workspace compiles and all host tests pass with the variant added but before any kernel emission changes (known-good intermediate). *(`cargo xtask clippy` + `cargo xtask test` green; kernel still emits `Granted`)*
- [x] The collector's cap aggregator treats `Minted` as opening a holding (like `Granted`), tagged with a distinct `"minted"` event name. *(`minted_event_opens_holding_named_minted`; 21/21 mutants caught)*
- [x] `EndpointCreate` emits `Minted` (not `Granted`); the endpoint an IPC program creates for itself is snitched as `Minted{Endpoint}`. *(`init-brings-up-fs-server`, `init-runs-fs-client`, `revoke-reclaims-a-minted-cap` green)*
- [x] `NotifyCreate` emits `Minted` (not `Granted`); a self-created notification is snitched as `Minted{Notification}`. *(`notify-signal-wakes-waiter` — new assertion, green)*
- [x] Bootstrap grants (telemetry/span sinks, `run_ipc` endpoint) remain `Granted` — unchanged on the wire. *(`userspace-cap-granted-event`, `userspace-spansink-granted`, `spawn-delegates-to-child` green)*
- [x] The wire-stability golden test covers `Minted`. *(golden `.snap` updated — `Minted 03` appended, existing arms undisturbed)*

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without a failing test.

### Step 1: Add `CapEventKind::Minted` and make every consumer handle it (host-only; no kernel emission change)

Inherently coupled — adding the variant breaks the exhaustive match at `collector/src/caps.rs:53` (`Granted | Transferred` / `Revoked`, no wildcard), so the variant and that arm must land together to keep the workspace compiling. This is the planning-skill's documented "coupled change" exception. The kernel still emits `Granted` everywhere after this step; it is purely additive on the wire.

**Acceptance criteria**:
- `Frame::CapEvent { kind: CapEventKind::Minted, .. }` roundtrips through postcard byte-for-byte (mirrors the existing `Revoked` roundtrip test in `protocol/src/lib.rs` / `stream.rs`).
- `protocol/tests/wire_stability.rs` includes `Minted` in its per-kind golden coverage.
- Feeding a `Minted` `CapEvent` to the collector's `caps` aggregator opens a holding with a `"minted"` span event (not `"granted"`); a subsequent `Revoked` closes it.
- `diagram::caps::derivation_tree` renders a `Minted` cap (`parent_cap_id == 0`) as a root node (lock-in test; no code change expected).
- Workspace compiles (`cargo xtask clippy`) and all host unit tests pass.

**RED**:
- `protocol`: a roundtrip test asserting a `Minted` `CapEvent` survives encode→decode equal.
- `collector`: a `caps` test feeding `Minted` then `Revoked`, asserting one closed span carrying a `"minted"` event.
- These fail to compile / fail until the variant + collector arm exist.

**GREEN**:
- `protocol/src/lib.rs`: append `Minted` to `CapEventKind` (after `Revoked`); doc it; bump the wire-format version note at `lib.rs:36` ("7: appended `Minted` to `CapEventKind`").
- `collector/src/caps.rs`: extend the arm to `Granted | Transferred | Minted`, mapping `Minted => "minted"` for the event name.
- Add the `Minted` arm/case to `wire_stability.rs`'s kind loop.
- Confirm `stream.rs` `OwnedFrame::from_borrowed` needs no change (kind is passed through) — compiler confirms.

**MUTATE**: `cargo xtask mutants` on the touched `protocol` + `collector::caps` surfaces; kill survivors on the new `"minted"` mapping and the roundtrip.

**KILL MUTANTS**: strengthen the collector test if the `"minted"` string mutant survives (assert the exact event name, not just span count).

**REFACTOR**: assess whether the collector event-name `match` is worth a small table; only if it adds clarity.

**Done when**: all criteria met, workspace green, mutation report reviewed, human approves commit.

### Step 2: Flip `EndpointCreate` and `NotifyCreate` to emit `Minted`

The behavior change. TDD drives at the itest layer (kernel has no host `#[test]`s): flip the scenario assertions to expect `Minted` (RED against the current kernel), then change the emit sites (GREEN).

**Acceptance criteria**:
- `EndpointCreate` snitches `CapEvent::Minted{Endpoint}` (was `Granted`). Scenarios asserting init's / `ep_maker`'s self-created endpoint updated: `scenarios.rs:3302` (`ep_maker`), `3358`, `3411` (init) now expect `Minted`.
- `NotifyCreate` snitches `CapEvent::Minted{Notification}` (was `Granted`). If no notify-create assertion exists, add one (`endpoint-create` / `notify-create` scenario) asserting `Minted{Notification}`.
- Bootstrap `Granted{TelemetrySink}` / `Granted{SpanSink}` matchers (`itest/matchers.rs`) and the `run_ipc` bootstrap endpoint are **unchanged** — regression check that they still assert `Granted`.

**RED**: edit the endpoint scenarios to match `CapEventKind::Minted`; run `cargo xtask itest ep-creates-endpoint` (and the init scenarios) — they fail because the kernel still emits `Granted`.

**GREEN**:
- `kernel/src/obs/tracing.rs`: add `emit_cap_minted` (clone of `emit_cap_granted`, `kind: Minted`).
- `kernel/src/syscall/ipc.rs:414`: `emit_cap_granted` → `emit_cap_minted`.
- `kernel/src/syscall/notify.rs:38`: `emit_cap_granted` → `emit_cap_minted`.
- Leave `trap/user.rs:806` and `:943` (bootstrap) as `emit_cap_granted`.

**MUTATE**: kernel emit paths aren't `cargo-mutants` reachable (bare-metal); rely on the itest assertions as the effectiveness check. Confirm the flipped scenarios fail if the kind is reverted (they do — that's the RED we saw).

**KILL MUTANTS**: n/a (itest-covered); ensure at least one scenario pins `Minted{Notification}` so both flipped sites are guarded, not just the endpoint one.

**REFACTOR**: consider whether `emit_cap_minted`/`emit_cap_granted`/`emit_cap_transferred` should collapse to one `emit_cap_event(kind, …)` — only if it reads better; they currently differ only in `kind` + `parent_cap_id: 0` defaulting. Assess, don't force.

**Done when**: `cargo xtask itest` green (run `--repeat 10` per the commit gate before proposing commit), bootstrap matchers still assert `Granted`, mutation/itest effectiveness reviewed, human approves commit.

## Pre-PR Quality Gate

Before each PR:
1. Mutation testing — `cargo xtask mutants` on host-reachable surfaces (Step 1). *(done: 21/21 caught on `collector::caps`.)*
2. Refactoring assessment (Step 2's emit-helper collapse question). *(done: declined — named helpers self-document.)*
3. `cargo xtask clippy` (whole workspace) + host tests + `cargo xtask snemu-itest` (deterministic — replaces the old `itest --repeat 10` flake-gate; itest/QEMU is being retired in favour of snemu-itest). *(done: 108/110; the 2 failures are the standing snemu FS-read fidelity gap — `bytes_read=0` in `viewer-reads-delegated-file` / `shell-view-command-revokes-cap` — not `CapEvent`-related. All 4 flipped + 3 regression scenarios pass.)*
4. Docs: update `docs/capability-system-design.md` (the four-kind provenance table) and the `CapEventKind` doc comment. *(done.)*

## Out of scope (explicitly not this plan)

- Renaming `Granted` → anything, or splitting `MintBadged` out of `Transferred` (a separate, also-defensible carve we rejected in favor of the self-service axis).
- The renderer-side "capability birthday" visualization (Stitch tree / Grafana). Tracked separately; the wire change here is the enabler.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
