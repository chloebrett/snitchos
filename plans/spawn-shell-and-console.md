# Spawn, the explicit-authority shell, and console input — design

**Status:** **Reconciled 2026-06-27 — most of the critical path has SHIPPED; this
revision re-plans from the current state toward a _terminal_ shell.** The original
design (2026-06-18) said "no code yet"; since then `Spawn`, `Exit`+`Wait`, the
blocking-wait primitive, `ConsoleRead` + polled UART RX, and the FS-over-IPC stack
have all landed. The remaining work for an interactive terminal shell is small and
named below.

**What shipped since this was written (verified 2026-06-27):**

- **`Spawn` (syscall 15)** — `kernel/src/syscall/process.rs:69`. All-or-nothing
  delegation of a subset of the caller's caps (copy semantics, `kernel_core::cap::delegate`),
  child auto-granted bootstrap telemetry/span (Q-A lean taken), ambient (Q-B lean
  taken). Phase 1a embedded-id done; the registry is `SPAWNABLE` at
  `kernel/src/trap/user.rs:387` (today: `spawnee`, `memhog`) — the extension point
  for new programs.
- **`Exit`/`Wait` (1, 18)** — `kernel/src/syscall/process.rs:13,39`. The exit/wait
  gap is **closed**: `Exit` records the zombie + wakes a blocked parent; `Wait`
  blocks (`block_current`/`wake`), then `reap_task` frees the child's AS, `Process`,
  and kernel stack. v0.12 same-hart. (The design's "notification vs blocking Wait"
  fork resolved to **blocking `Wait`**; a general Notification object is still
  deferred to Tier-1 devices.)
- **`ConsoleRead` (14) + polled UART RX** — `kernel/src/syscall/console.rs`,
  `kernel/src/device/console.rs`. The timer drains `RBR`→`CONSOLE_RX` ring;
  `ConsoleRead` is ambient, non-blocking. **Tier-0 console input is done** (was
  `[D]`); the `console_echo` demo proves it. Line discipline still lives in userspace.
- **Cross-AS copy** `CopyFromCaller`/`CopyToCaller`, **IPC** (Send/Recv/Call/Reply/
  ReplyRecv/MintBadged), and the **FS-over-IPC** stack (`fs-core`/`ramfs`/`fs-proto`
  + `user/fs` server+client, 7 itests) — all shipped and tested.

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

## Current state (verified 2026-06-27)

| Piece                                    | State                                                                                                 |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Userspace spawn (with cap delegation)    | ✅ `Spawn` (15), all-or-nothing delegation, `SPAWNABLE` registry (2 programs).                        |
| Process exit / wait / reap               | ✅ `Exit` tears down + wakes parent; `Wait` (18) blocks + `reap_task` frees AS/Process/stack.         |
| Cross-AS copy                            | ✅ `CopyFromCaller`/`CopyToCaller`.                                                                    |
| Console **input**                        | ✅ polled UART RX ring (timer-drained) + ambient `ConsoleRead` (14), non-blocking. Tier-0 done.       |
| Console **output to the terminal**       | ❌ userspace output is `DebugWrite`→telemetry only. No userspace→UART write. **The terminal gap.**    |
| `init` / first-process bootstrap         | ❌ no init; programs are per-workload `kmain` spawns + a 2-row `SPAWNABLE` registry.                  |
| Cap-delegation observability             | ❌ `Spawn` delegates but emits no `CapEvent::Transferred` — the "watch least-authority" trace.        |
| External interrupts (PLIC)               | ❌ unwired (only needed for Tier-1 virtio console — a later, separate milestone).                    |
| Notifications (general async signal)     | ◐ child-exit covered by blocking `Wait`; general Notification object deferred (Tier-1 devices).      |
| Device capabilities / userspace MMIO     | ❌ none (Tier-1 only).                                                                                 |

---

## Phase 1 — `Spawn`-with-caps (the milestone heart) ✅ SHIPPED

_Implemented as `Spawn` (15) — `kernel/src/syscall/process.rs:69`. The design below
is now documentation of what was built; the leans on Q-A/Q-B/Q-C were all taken
(auto-grant telemetry/span, ambient, copy semantics). The exit/wait gap is closed
(`Exit`/`Wait`/`reap_task`). What remains for a usable terminal shell is the
terminal-output primitive, `init`, the shell program, and the delegation trace —
see "Remaining work" and "Sequencing" below._

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

## Component inventory + critical path

_Status (2026-06-27): groups **A** (Spawn + cap-transplant + startup-cap ABI), **B**
(Exit + Wait), and **C** (the wait primitive) are **SHIPPED**. Group **D** (init,
shell, cat/ls) and group **E** (now reframed — input is done; output via
`ConsoleWrite` is the remaining terminal primitive) are what's left. See "Remaining
work" above for the actionable list; the inventory below is kept for the full map._

Every architectural piece, grouped by subsystem. **[CP]** = on the critical path to
the first demo (_shell spawns `cat` with a delegated file cap; the trace proves
`cat` could only touch that one file_). **[D]** = deferred (needed eventually, not
for the minimal milestone). Note: **polled UART RX is [D]** — a leaf on the deferred
console branch, not load-bearing for the demo.

### A. Process creation & delegation (the heart — all CP)

1. **Spawn syscall** `[CP]` — generalize the boot path (`new_user_root → Process → load → enter`) to a userspace-invokable syscall.
2. **Cap-delegation / transplant** `[CP]` — resolve the caller's handles, insert copies into the child's `CapTable`. Prior art: `reply_with_cap` (v0.9c) already moves a cap between tables; this is the N-cap, new-table version.
3. **Startup-cap convention** `[CP, underestimated]` — how a spawned program _finds_ its delegated caps. Today hardcoded (`a0`=telemetry, `a2`=endpoint); delegated caps need a defined startup layout (known handle ordering or a cap-array the runtime exposes). Small but foundational — every spawned program's ABI.
4. **Program source** `[CP→D]` — 1a: an embedded-program registry selected by id (ELFs already embedded). 1b: load the ELF from the FS via a File cap + `EXEC` (needs `user/fs`).

### B. Process lifecycle (CP for a _usable_ shell)

5. **Exit + teardown** `[CP, underestimated / gnarliest]` — `Exit` (syscall 1) must reclaim the user page table + frames + `CapTable` + `Box<Task>` + scheduler slot. Tasks are `-> !` today with zero teardown. Reclaiming a live address space leak-free is the fiddliest single item.
6. **Wait / join** `[CP]` — parent blocks until child exits, woken on exit. This is what makes the shell _loop_.

### C. Notifications (the gateway — CP, but for processes, not devices)

7. **Async notification primitive** `[CP]` — wake a blocked task without a full message rendezvous. **First consumer is Wait (B6), not devices** — which is why it floats to the top of the sequence. Forks: payload-free vs valued; one-shot vs latching; a Notification object/cap vs a plain blocking `Wait(child)` syscall. (Child-exit alone may only need a blocking `Wait`; the general Notification object is what Tier-1 devices later need.)

### D. The shell & its world (CP)

8. **init / first-process bootstrap** `[CP]` — a real `init` holding root caps that spawns the shell and grants it its **session caps** (dir/file caps = its "world", a console path, the ability to Spawn). The root of the delegation graph; generalizes today's hardcoded per-workload `kmain` spawns.
9. **The shell program** (`user/shell`) `[CP]` — read → parse → resolve → delegate → spawn → wait.
10. **The programs it runs** (`cat`, `ls`, …) `[CP]` — small bins that take authority from delegated caps; depend on (3).

### E. Console input — Tier 0 scaffold (D, smaller than it looks)

11. **Polled UART RX** `[D]` — read `RBR` when `LSR` data-ready.
12. **ConsoleRead syscall** `[D]` — ambient, mirrors `DebugWrite`.
13. **Who polls, and when?** `[D, wrinkle]` — with no interrupt, bytes typed while the kernel isn't looking are lost unless something drains `RBR` periodically. Needs either a busy-wait in `ConsoleRead` (blocks the hart — bad) or a **periodic kernel poll** (piggyback the heartbeat) draining into a small **RX ring**. The ring + poller is the real Tier-0 work, not the register read.
14. **Line discipline** `[D]` — echo / backspace / enter → lines; lives in userspace.

### F. Console input — Tier 1 principled (D — its own milestone)

15. **PLIC + external-interrupt path** (scause 9, claim/complete) `[D]`
16. **IRQ→notification binding** (IRQHandler cap) `[D]` — _here_ the Notification object (C7) earns its general form.
17. **Userspace MMIO mapping** (device-memory cap / `MapDevice`) `[D]`
18. **DMA buffer primitive** (device-visible physical memory — the `TX_STAGING` problem in userspace) `[D]`
19. **Userspace virtio driver + console server** `[D]` — with the IOMMU non-goal caveat (above).

### G. Cross-cutting (D)

20. **Resource quota** (spawn/memory) — the resource axis; deferred.
21. **Spawn/Exit telemetry** — likely a `Spawn`/`Exit` frame + reuse of `CapEvent`/`ThreadRegister`; mostly free.

### The critical-path core is smaller than the inventory

The leanest demo needs only **1, 2, 3, 5, 6, 7, 8, 9, 10** + the FS read path. It can be shaved further for a _first_ milestone:

- **Skip FS lookup-minting:** `init` hands the shell a ready `(foo, READ)` File cap; the shell just _delegates_ it to `cat`. Demonstrates the whole delegation story without `lookup`-mints-a-cap yet.
- **Skip interactive input entirely:** drive the shell from a hardcoded command first — **all of E and F vanish** from the first milestone.
- **Possibly skip Wait at first:** a one-shot "spawn `cat`, see the `CapEvent`" demo doesn't strictly need join; a _looping_ shell does.

Irreducible core: **Spawn + cap-transplant + startup-cap ABI + Exit/teardown + (Wait→notification) + an init that grants the shell its world.** Everything console is genuinely off the critical path.

---

## Terminal output — `ConsoleWrite` (decided 2026-06-27)

The original design routed userspace output to telemetry (`DebugWrite`→`Log` frame)
and never gave userspace the UART. For a **terminal shell** that's wrong: you'd
type in the QEMU console but read the prompt/output in the collector. **Decision: a
terminal shell, so add a `ConsoleWrite` syscall** — the mirror of `ConsoleRead`,
writing user bytes to the UART `THR`. The kernel already owns the TX path (the
`UART` mutex behind `println!`, `kernel/src/device/console.rs`); `ConsoleWrite` just
exposes it. Ambient like `ConsoleRead` (Tier-0 convention: "UART = the human
channel; the shell is the trusted session root, so it holds its own terminal
directly — the interesting delegation is _downward_ to children"). Could become a
console **cap** in Tier-1; ambient for now.

This keeps the two human-facing channels clean: **UART = interactive terminal**
(shell I/O), **virtio-console = the postcard telemetry stream** (don't mix typed
input/echo into the decoded frame channel).

## Remaining work for the terminal shell (the from-here plan)

The heavy lifting (Spawn / Exit / Wait / ConsoleRead / cap-transplant) is **done**.
What's left, in build order:

1. ✅ **`ConsoleWrite` syscall (19)** — ambient, mirror of `ConsoleRead`.
   `abi` variant + `from_usize` (host-tested); `kernel/src/syscall/console.rs::
   handle_console_write` copies user bytes (range-validated, UTF-8) and reuses the
   kernel `print!` UART path (shell shares the one terminal with the kernel log,
   distinct from the `DebugWrite` telemetry channel); runtime `console_write`
   chunks to `DEBUG_WRITE_MAX` (the kernel refuses an over-long single write).
   **Exercised live by `workload=stitch-repl`** (`user/hello/src/bin/stitch_repl.rs`):
   the ported no_std Stitch interpreter runs as a userspace REPL, printing its
   banner + self-test (`1 + 2 => 3`) + `stitch>` prompt out the UART via
   `ConsoleWrite`, reading input via `ConsoleRead`. Both console primitives now
   proven on the metal. (Gotcha found: the 16 KiB user stack silently overflowed
   the recursive interpreter — bumped `user/runtime/user.ld` to 512 KiB.)
2. **`init` (first-process bootstrap)** `[CP]` — a real first process that holds
   root caps, `Spawn`s the FS server, holds the FS endpoint cap, and `Spawn`s the
   shell granting it its **session caps** (the FS cap, console access). Generalizes
   today's per-workload `kmain` spawns; root of the delegation graph. Add the new
   programs (`init`, `shell`, `cat`, `ls`) to the `SPAWNABLE` registry.
3. **`user/shell`** `[CP]` — the loop: `ConsoleRead`-poll a line (line discipline —
   echo via `ConsoleWrite`, backspace, enter — in userspace) → parse → dispatch →
   `Wait`. **Milestone A:** builtins only (`help`, `echo`, and `ls`/`cat` by the
   shell itself talking to the FS server over IPC) — a breathing terminal shell.
   **Milestone B:** the capability demo — `cat <file>` mints a narrowed `(inode, READ)`
   File cap and `Spawn`s a separate `cat` holding _only_ that.
4. **`cat` / `ls` programs** `[CP for Milestone B]` — small bins that take a File
   cap and read through it; rely on the startup-cap ABI (handles 2.. = delegated).
5. **Cap-delegation trace** `[CP for the payoff]` — emit `CapEvent::Transferred` per
   delegated cap in `Spawn`, so `cat /foo` produces the visible "shell minted
   `(foo, READ)` → granted to `cat` → `cat` read → exited" chain. _That trace is the
   demo._ (Item 21; "mostly free.")

Deferred (unchanged): Spawn **Phase 1b** (load ELF from the FS via a File cap +
`EXEC`); the **Tier-1** userspace virtio console driver (PLIC + MMIO + DMA caps),
its own milestone; resource quotas (Q-D).

## Sequencing (original, mostly DONE — kept for the record)

```
1. Notification primitive (v0.9d)        ✅ resolved as blocking Wait
2. Exit + Wait/join                       ✅ shipped
3. Spawn-with-caps (Phase 1a, embedded)   ✅ shipped
4. Shell program                          ← NEXT (see "Remaining work" above)
5. Spawn Phase 1b (ELF from FS + EXEC)    ← deferred
6. Tier-1 userspace virtio console driver ← deferred (separate milestone)
```

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
