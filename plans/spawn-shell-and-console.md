# Spawn, the explicit-authority shell, and console input — design

**Status:** **Design / plan only (2026-06-18).** No code yet. Captures the
architecture and a phased build order. Builds on v0.9 IPC + v0.9c badges and the
v0.10 FS (`fs-core`/`ramfs`/`fs-proto` shipped; `user/fs` front-end in progress).

**The vision:** a shell where authority is explicit — it reads a command, and
launches each program holding **exactly** the capabilities that program needs,
nothing ambient. Every grant is a `CapEvent` on the wire, so you can _watch
least-authority happen_ in the traces. (See memory: explicit-authority shell idea;
prior art: Plash, the powerbox/CapDesk, Genode, Fuchsia, Capsicum.)

---

## The reframe: Spawn is the heart, console is plumbing

A shell's job is _read line → launch a program with delegated caps_. The
capability story lives **downward** — what the shell hands its children — not in
how it reads keystrokes. So the build order is **Spawn-with-caps first, console
input later** (chosen 2026-06-18). The two are orthogonal; console work does not
block Spawn.

## Current state (verified 2026-06-14)

| Piece                                    | State                                                                                                 |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Userspace spawn                          | ❌ none. Processes created only at boot via hardcoded `sched::spawn_on` + `user::run(ELF)`.           |
| Cross-AS copy                            | ✅ `CopyFromCaller`/`CopyToCaller` (syscalls 13/14) exist — option-D primitive landed.                |
| Process exit / wait / join               | ❌ tasks are `-> !` (v0.5); no exit-to-parent, no join.                                               |
| Console input                            | ❌ UART is TX-only; virtio-console RX is dead weight + dedicated to telemetry; no input syscall.      |
| External interrupts (PLIC)               | ❌ unwired — an external interrupt currently `panic!`s. Only timer (sstc) + IPI (software).           |
| Notifications (async kernel→user signal) | ❌ none (the deferred v0.9d item).                                                                    |
| Device capabilities / userspace MMIO     | ❌ none — all devices are ambient kernel-driven.                                                      |
| Process caps at creation                 | hardcoded per workload in `Process::bootstrap` (telemetry + span; IPC workloads add an endpoint cap). |

---

## Phase 1 — `Spawn`-with-caps (the milestone heart)

Generalize the boot-only creation path (`new_user_root` → `Process::bootstrap` →
`load` → `enter`) into a userspace-invokable syscall that **delegates a chosen
subset of the caller's own capabilities** to the child.

### Proposed syscall

```
Syscall::Spawn = 15   // append-only

a0 = program selector
       Phase 1a: an embedded-program id (the kernel holds the ELFs today)
       Phase 1b: an executable File cap handle (ELF read from the FS; needs EXEC)
a1 = pointer (in caller's AS) to a [Handle; N] array of caps to delegate
a2 = N
→ a0 = child task id  (or an error)
```

The kernel:

1. resolves the program (embedded id → ELF bytes; later: `read` the File cap, gated on `EXEC`),
2. `CopyFromCaller`s the `[Handle; N]` array (reuses syscall 13),
3. **resolves every handle in the _caller's_ `CapTable`** — if any fails, `SyscallRefused` (no partial spawn, no forging: you can only delegate caps you hold),
4. `new_user_root` + `load`, builds a `Process` whose `CapTable` is **exactly the delegated caps** (this is `spawn(program, caps)` literally),
5. spawns the task, returns the child id.

### Decisions to make (flagged, not yet decided)

- **Q-A: Does Spawn auto-grant telemetry/span caps, or must the parent delegate them?**
  Auto-grant = every process is observable by default (serves the observability
  pillar) but is a sliver of ambient authority. Require-delegation = pure least
  authority, but a child the parent forgot to grant a sink to is invisible.
  _Lean:_ auto-grant span/telemetry (observability is the project's whole point;
  a telemetry sink is not a security-sensitive authority), document it as the one
  deliberate ambient grant.
- **Q-B: Is Spawn itself ambient or cap-gated?** Ambient = any process can spawn
  (simplest). Cap-gated = need a "spawn authority" cap (seL4 gates spawn behind
  TCB/CNode/Untyped). _Lean:_ ambient for Phase 1; gating spawn is a resource-control
  refinement (see Q-D), not an authority one.
- **Q-C: Copy or move semantics for delegated caps?** _Lean:_ copy (caller keeps
  its caps; child gets its own table entries naming the same objects). Attenuation
  = mint a narrower badged cap first, then pass that handle.
- **Q-D: Resource quota.** Userspace Spawn lets a process create unbounded children
  → exhaustion. This is the _resource_ axis the FS doc flags as "not free." Needs a
  spawn/memory quota eventually (seL4: untyped memory). Out of scope for Phase 1;
  note the hole.

### The exit/wait gap (prerequisite for a _usable_ shell)

The shell runs `cat /foo` and must regain control when `cat` finishes. But tasks
are `-> !` today — no exit-to-parent, no join. So Phase 1 also needs:

- **`Exit`** to actually tear down (it exists as syscall 1 but with no teardown/notify),
- a **join/wait** path so the parent is woken on child exit (an IPC notification, or
  a blocking `Wait(child_id)`).

This interlocks with the **notification primitive** below — child-exit is a natural
first consumer of "async kernel→user signal," independent of devices.

---

## Phase 2 — the shell program (`user/shell`)

A userspace process init spawns, holding: its **session File/dir caps** (its
"world", granted by init), a **console-input** path (scaffold or cap, see below),
and the ability to `Spawn`. Loop:

```
read line  →  parse (command + args)
           →  for each path arg: lookup via the shell's dir cap to mint a
              narrowed File cap (READ for `cat`, etc.)   ← the explicit delegation
           →  Spawn(program, [those caps])
           →  Wait(child)   ← needs the exit/wait gap closed
           →  repeat
```

`cat /foo` ⇒ shell mints `(foo_inode, READ)` and spawns `cat` holding _only_ that.
`cat` cannot reach anything else — and the grant is a `CapEvent::Transferred` span.
That trace **is** the demo.

---

## Console input — two tiers (deferred; does not block Phase 1)

Driving the shell by hand needs input, but interactive input can lag Spawn (drive
the shell from a hardcoded command first). Two tiers:

### Tier 0 — scaffold (cheap, get typing working)

- **Polled UART RX + an ambient `ConsoleRead` syscall** (mirrors `DebugWrite`).
  Read `LSR` bit 0 (data-ready), read `RBR`. **Zero new interrupt infrastructure**
  (no PLIC). Matches the "UART = the human channel" convention.
- Explicitly labeled scaffold: the shell is the trusted session root, so it
  legitimately holds its terminal somewhat directly; the interesting delegation is
  downward (to children), not the shell's own keyboard read.
- Line discipline (echo, backspace, enter) lives in userspace.

### Tier 1 — principled (its own milestone: the userspace-driver framework)

A **userspace virtio driver** for a _new_ virtio device dedicated to interactive
console (kernel keeps UART for its own debug logging; telemetry virtio-console
stays the postcard stream — don't mix human input into it). Why virtio over UART:
QEMU `virt` has ~8 virtio-mmio slots vs one ns16550a, and virtio is built for
**notify, not poll** (interrupt on used-ring fill) — right for bursty input.

This needs **four** new kernel mechanisms (notifications are only the gateway):

1. **Async notification primitive** (kernel→user wakeup) — the v0.9d deferred item; seL4's Notification object.
2. **PLIC + external-interrupt path** (currently `panic!`s) + an IRQ→notification binding (seL4's `IRQHandler` cap).
3. **Userspace MMIO** — map the device registers + queue memory into the driver's AS (a device-memory cap / `MapDevice`).
4. **DMA buffers** — the virtqueue needs _physical_ addresses; user VAs aren't device-visible. This is the `TX_STAGING` gotcha (`va_to_pa` only handles kernel-range VAs) moved into userspace.

**Non-goal / honest caveat: without an IOMMU, a userspace driver that programs DMA
addresses can read/write _all_ physical RAM** — it bypasses page-table protection
and is therefore a **trusted** component, not an isolated one. (seL4 says the same
about driver VMs sans IOMMU.) For a learning/observability project on QEMU this is
fine, but it must be written down: the userspace driver buys **modularity +
observability, not isolation**, until an IOMMU exists. This is the one place the
project's isolation-by-capability thesis genuinely leaks.

**Payoff:** the keyboard driver becomes a normal process holding exactly an MMIO
cap + an IRQ/notification cap — a great standalone post ("the driver is just a
process; watch it hold two caps"). But it's a _separate milestone_, sequenced after
Spawn + the shell, motivated by "drivers in userspace," not by the shell.

---

## Sequencing

```
1. Notification primitive (v0.9d)        ← gateway; first consumer = child-exit/wait, NOT devices
2. Exit + Wait/join                       ← makes the shell usable
3. Spawn-with-caps (Phase 1a, embedded)   ← the heart; the explicit-authority demo
4. Shell program (hardcoded command first, then Tier-0 UART input)
5. Spawn Phase 1b (load ELF from the FS via a File cap + EXEC)   ← needs user/fs front-end
6. (separate milestone) Tier-1 userspace virtio console driver: PLIC + MMIO caps + DMA
```

Notifications float to the top not for devices but because **child-exit/wait needs
the same async-signal primitive** — and that's the shell's real blocker, not input.

## Observability angle (the post)

Every delegation Spawn performs is a `CapEvent::Transferred`. So `cat /foo` emits a
visible chain: shell mints `(foo, READ)` → grants it to a fresh `cat` → `cat` reads
→ exits. _"I didn't build sandboxing; I just stopped handing out authority — and
here's the trace proving cat could only ever touch one file."_

## Open items (consolidated)

- Q-A telemetry/span auto-grant vs delegate (lean: auto-grant).
- Q-B Spawn ambient vs cap-gated (lean: ambient for now).
- Q-C copy vs move delegation (lean: copy; attenuate via mint-then-pass).
- Q-D spawn/memory resource quota (deferred; the resource axis).
- Notification primitive shape (payload-free signal vs a value; one-shot vs latching).
- Exit/Wait semantics (blocking `Wait(child)` vs an exit notification on an endpoint).
- Program source progression: embedded id → FS File cap + EXEC enforcement (~v0.11).
- Tier-1 driver: IOMMU caveat is an explicit non-goal; revisit if isolation is ever required.

```

```
