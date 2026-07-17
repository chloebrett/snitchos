# Scaling SnitchOS down — tickless, MCUs, Tock, and the memory floor

An exploration sparked by a small question — "is 50 ms our preemption quantum?" — that
branched into: how our timing compares to Linux, whether we should go tickless, where
SnitchOS could plausibly *run* (browser, SBC, battery handheld, bare MCU), and why it
needs megabytes when Tock runs in 64 KB. It's a notebook of the reasoning and the
verdicts, not a plan. Nothing here is committed work; the actionable residue is a short
"future directions" list at the end.

## 1. Our timing knobs: 50 ms tick, 200 ms quantum

Two separate things, easily conflated:

| Knob | Definition | Value |
|---|---|---|
| **Timer tick** | `timebase / TICKS_PER_HEARTBEAT` = 10 MHz / 20 | **50 ms** |
| **Preemption quantum** | `QUANTUM_TICKS` | **200 ms** |

The timer fires every 50 ms (draining RX, evaluating preemption, and — in the pending
timed-`WaitAny` design — draining a timeout queue). Preemption only *fires* once a task
has held the CPU ≥ 200 ms, sampled at each 50 ms tick. So 50 ms is the sampling rate;
200 ms is the actual slice. (The doc comment on `QUANTUM_TICKS` that used to say "the
QEMU timer fires every ~1 s" was stale and has been corrected.)

### vs Linux

- **Timer tick**: Linux `CONFIG_HZ` is 100/250/1000 Hz → **1–10 ms**, on top of which
  modern kernels are tickless (`NO_HZ`). Our 50 ms is ~5–50× coarser.
- **Time slice**: Linux CFS/EEVDF don't use a fixed quantum — they target
  `sched_latency` (~6 ms, scaled by runnable count) with a ~0.75 ms floor, so real
  slices are **single-digit milliseconds, dynamic**. Our fixed 200 ms is ~30–200×
  coarser (closer to the old O(1) scheduler's ~10–100 ms slices).

**Why coarse is fine here**: SnitchOS is cooperative-first. Preemption (v0.8) is a
*safety net* against a task that won't yield, not the scheduling mechanism. Cooperative
tasks re-enter the runqueue far sooner than 200 ms, so they never hit the quantum — it
only bites a hog or a wedged spinner. Linux's fine slices exist for low-latency fairness
across many contending tasks under load, which SnitchOS doesn't optimize for yet.

## 2. Tickless (NO_HZ) — how it works

The periodic tick is a *choice*, not a necessity. The hardware timer is really a
one-shot comparator (write a future time, it fires once); a "periodic tick" is just
software re-arming it to `now + interval` every fire. Tickless kernels instead arm it to
**the next moment something actually needs to happen**. Linux has three regimes:

1. **Periodic (HZ)** — a CPU with **2+ runnable tasks**. You need the tick to preempt
   between them and rebalance.
2. **`NO_HZ_IDLE`** (default) — when a CPU goes **idle**, stop the tick; program the
   one-shot timer to the earliest armed event and sleep until then or an interrupt. Pure
   power win (don't wake a sleeping core 1000×/s to find nothing to do).
3. **`NO_HZ_FULL`** — even a CPU running **exactly one task** stops the tick (nothing to
   schedule against). For HPC / real-time / low-jitter. Costly: RCU callbacks and
   timekeeping must be **offloaded** to housekeeping CPUs (`rcu_nocbs`).

Main technical consequence: **time accounting becomes on-demand** — instead of
incrementing `jiffies` per tick, a tickless kernel reads the clock and advances time by
the elapsed delta ("catch-up") when it next runs.

## 3. Should SnitchOS go tickless? — No (for now)

The verdict, after weighing it against *this* system specifically:

- **The core justification is absent.** NO_HZ_IDLE's payoff is **power** — stop waking a
  battery/datacenter CPU needlessly. SnitchOS runs in QEMU/an emulator; a 50 ms idle
  wake is unmeasurable and there's no battery to save.
- **It fights the point of the OS.** The **heartbeat rides the periodic tick**
  (`TICKS_PER_HEARTBEAT = 20` → metrics every ~1 s). A tickless-idle guest goes *quiet
  when idle* — exactly when an observability OS might want to show "idle, nothing wrong."
  An always-on snitch and a sleep-through-idle timer are in tension.
- **The useful sliver is narrower and separable.** The only part with real value here is
  arming the timer **to the next real deadline** so a `wait_timeout` wakes *precisely*
  instead of within 50 ms. That's "arm-to-earliest," and it can be done with a
  **periodic heartbeat floor** — arm to `min(next_heartbeat, next_deadline)` — which
  gets exact timeouts *without* going fully tickless and *without* silencing idle
  telemetry. And even that isn't needed yet: 50 ms granularity is fine for hung
  detection (deadlines of 100s of ms +).

So: interesting to understand, worth a future-directions line, but building it now would
be yak-shaving — optimizing a cost that's zero here against a feature the telemetry model
doesn't want.

## 4. Where SnitchOS could run — and whether it changes the calculus

- **QEMU virt** (today): fast-forward-friendly, no power concern.
- **Browser** (via snemu → WASM, the "SnitchOS in a tab" goal): here every guest tick is
  *host* work, so an idle tab could pin a core. **But snemu already handles this well**:
  on `wfi` with nothing pending a hart goes `HartState::Idle`, and the machine loop
  **fast-forwards the emulated clock to the earliest armed `stimecmp` deadline** across
  idle harts (there's a test: `an_all_idle_machine_fast_forwards_to_the_earliest_armed_timer`).
  So it does *not* busy-step idle instructions. The one gap for a real-time browser
  device: it fast-forwards *emulated* time (runs flat-out) rather than **wall-clock
  pacing** (host-sleeping until the deadline so 1 guest-second ≈ 1 wall-second and the
  tab truly idles). That pacing is a small snemu-side addition on top of machinery
  that's already there — and it's the *right* layer, not the guest.
- **VisionFive 2** (StarFive JH7110, quad C910): a mains-powered dev board. A 50 ms idle
  tick is noise; doesn't justify tickless. It's a reason to test the timer path on real
  silicon, and it would force **un-hardcoding the QEMU-`virt` MMIO layout** (real UART
  base, real PLIC, real DTB parsing — the `collect_mmio_regions` path parked behind
  `#[expect(dead_code)]`, and a real telemetry transport since there's no virtio-console).
- **Battery-powered RISC-V handheld** (a Game-Boy-shaped thing): *this* is the scenario
  that would finally justify tickless — battery, idle sleep matters. Realistic silicon
  is **application-class but low-power**: **Allwinner D1/D1s** (T-Head XuanTie C906,
  RV64GC, ~1 GHz, has an Sv39 MMU — MangoPi MQ-Pro is thumb-sized, ~$10–30), Milk-V Duo
  (SG2002), or Kendryte K230. Note the C906's quirks: T-Head **custom PTE bits**
  (cacheability attributes in the high PTE bits, needed mainline patches) and a *draft*
  (0.7.1) vector extension — neither bites SnitchOS (no vectors; basic Sv39 works). SoC
  designs (framebuffer-that-snitches + physics desktop) already sketched in the repo make
  a handheld genuinely on-brand: a device whose UI *is* the system watching itself.

## 5. Silicon dividing line: MMU vs MCU/PMP

- **Application-class** RISC-V (D1, JH7110, most "runs Linux" chips): implements S-mode +
  an **MMU** (Sv39/Sv48) — virtual memory, page tables, `satp` switching. SnitchOS needs
  this today.
- **MCU-class** RISC-V (ESP32-C3/C6, GD32V, CH32V, most "RISC-V MCU" parts): M-mode
  (± U-mode) with at most **PMP** (Physical Memory Protection — coarse hardware R/W/X
  regions on *physical* addresses). **No MMU**, no virtual memory, no page tables.

SnitchOS-as-written can't run MCU-class — its entire isolation model is Sv39. (Earlier I
overstated this as "nothing to run on"; the accurate claim is "not as written.")

## 6. Could SnitchOS run on an MCU? — Yes, via PMP, but it's a re-architecture

What SnitchOS uses the MMU for splits in two:

- **Conveniences** (higher-half kernel, per-process identical VA layout, growable virtual
  heap window): nice but inessential — you can run at physical addresses.
- **Load-bearing**: exactly one thing — **process isolation**, so a buggy/hostile process
  can't scribble over the kernel or a sibling and forge authority the caps were meant to
  gate. Caps are a *software* construct; they only *mean* something if memory access is
  also constrained.

On an MCU that constraint comes from **PMP** instead of Sv39: give each process a
contiguous physical region + shared read-only kernel + its MMIO grants (~3–4 PMP entries),
reprogrammed on context switch. Hardware-enforced isolation, **zero translation memory**,
arbitrary region sizes. This is exactly how **Tock** and some seL4 configs do it — real
precedent.

**What survives**: caps still mediate, PMP still isolates, telemetry is still frames out a
UART — so SnitchOS's *identity* transfers.

**What it costs** (an isolation-layer rebuild, not a port):
- Rip out per-process page tables + `satp`; replace with PMP region assignment.
- The three-address-space model (higher-half / linear map / heap window) collapses to
  physical; `MapAnon`-style arbitrary virtual growth goes away (fixed physical regions).
- Processes become position-independent or fixed-loaded.
- **Brutal RAM shrink** — the 4 MiB heap, 16 KiB per-task stacks, intern table, telemetry
  buffers all assume comfort a real MCU (tens–hundreds of KB SRAM) doesn't have.
- 16 PMP entries caps how many / how fine the isolation domains get.

So "SnitchOS on an MCU" is a **different kernel wearing the same ideas**. Arguably *more*
on-brand than a D1-Linux-board, though — "observability-first, capability-mediated MCU
RTOS" is an unoccupied niche (Tock is the only close neighbor, and it has no telemetry
story). Two different bets: **the D1 keeps the kernel and re-does the peripheral layer;
the MCU keeps the ideas and re-does the kernel core.**

## 7. Tock — the closest existing system

An embedded OS in **Rust** for low-power MCUs (Cortex-M + RISC-V, no MMU), out of academia
(Levis et al.; SOSP 2017 *"Multiprogramming a 64 kB Computer Safely and Efficiently"*).
Real deployment: **Tock is the OS for OpenTitan**, Google's open silicon root-of-trust
(RISC-V Ibex).

**Signature design — two isolation tiers with different trust:**
- **Capsules** — in-kernel Rust driver/service modules, cooperatively scheduled, isolated
  from each other **by the Rust type system** (language safety), *trusted* (in the TCB),
  compile-time/static.
- **Processes** — userspace apps in any language, unprivileged, isolated by **MPU/PMP**
  (hardware), *untrusted*, dynamically loadable, preemptively scheduled, syscall-mediated.

**"Capability" means something different than in SnitchOS** — this is the key confusion:
- **Tock**: compile-time **zero-sized type tokens** you must *hold* to call sensitive
  kernel functions (a static discipline for structuring the TCB) + **grants** (kernel
  stores per-process state *in that process's own memory*, so it can't exhaust kernel
  RAM). No runtime handles, delegation, or revocation.
- **SnitchOS**: runtime **object-capabilities** (seL4 lineage) — `Handle`s naming
  `Object`s with `Rights`, validated per-syscall against a per-process `CapTable`, and
  delegable / mintable / revocable *while running*.

Same word, different genus: Tock = compile-time tokens + memory grants; SnitchOS = live
object-caps.

**Overlap**: Rust; small trusted kernel; hardware-isolated untrusted userspace;
preemptive processes; cap vocabulary. **Diverge**: MPU/PMP vs MMU; token-caps vs
object-caps; Tock's language-isolated capsule tier (no SnitchOS analog — our drivers are
just trusted kernel code); and **observability**, which is SnitchOS's whole identity and
absent in Tock.

## 8. Why Tock fits in 64 KB and SnitchOS needs megabytes

Almost entirely design philosophy, not efficiency. SnitchOS's floor is *soft* — dominated
by one tunable choice, the kernel heap **starting at 4 MiB** (`linked_list_allocator`,
grows on demand). The reasons it can't trivially become 64 KB are three structural spends:

1. **The MMU is a RAM tax the MPU doesn't levy.** Sv39 page tables are 4 KB-granular,
   multi-level: every process needs a root + intermediate tables per region, and
   everything you map rounds up to a **4 KB page**. PMP is a handful of *registers* —
   **zero translation memory**, arbitrary-size regions (a 512-byte stack is fine). So
   Tock pays ~0 for isolation and no 4 KB quantum.
2. **SnitchOS assumes a heap; Tock is statically allocated.** We live in Rust's `alloc`
   world — `Vec<Box<Task>>`, `BTreeMap` directories, `String` names, `format!`-minted
   metric names. Tock is `no_std` *and largely no `alloc`*: static compile-time
   allocation + fixed per-process grants, no growing kernel heap, no runtime formatting.
   Dropping the heap + `alloc`-heavy collections is the single biggest lever.
3. **Observability isn't free — it's SnitchOS's RAM.** Intern table, span registry,
   pre-init buffer, virtio TX staging, deferred counters — state a bare RTOS doesn't
   carry, existing precisely because self-narration has a memory cost.

| | Tock (64 KB) | SnitchOS |
|---|---|---|
| Isolation | MPU/PMP registers, **0 translation RAM**, arbitrary regions | Sv39 page tables, 4 KB-quantized |
| Allocation | **static** + fixed grants, mostly no `alloc` | 4 MiB heap + `Vec`/`BTreeMap`/`String`/`format!` |
| Observability | none | intern tables, span registry, frame buffers |
| Stacks | as small as needed | 16 KiB (4 pages), guard-paged |

Reaching 64 KB wouldn't be a diet — it'd be the §6 re-architecture (PMP not Sv39; static
allocation; ring buffers not intern tables; fixed arrays not `alloc`).

## 9. Takeaways / future directions

- **Don't build guest tickless now.** No power to save in an emulator; it fights the
  always-on heartbeat. Revisit only if SnitchOS targets a battery device — and reconcile
  it with the heartbeat first.
- **The one guest-side timer refinement worth eventually doing**: arm-to-earliest-deadline
  *with a heartbeat floor*, for precise `wait_timeout` wakeups — and only if coarse (50 ms)
  timeouts ever measurably hurt.
- **The browser's idle-battery concern is a snemu-side lever**: add wall-clock-paced idle
  sleep on top of the fast-forward that already exists. Guest stays simple.
- **Two portability bets, both real**: (a) **D1-class board** — keep the kernel, make the
  peripheral/MMIO/DTB layer data-driven, add a real telemetry transport; a good forcing
  function for un-hardcoding `virt`. (b) **MCU via PMP** — keep the ideas, rebuild the
  isolation + allocation core; the more novel, more *ours* niche (read Tock's process /
  grant / PMP code as the reference implementation).
- **Keep the framebuffer → physics-desktop path warm** — it's what makes a handheld
  (battery → the tickless scenario) worth the trouble: a device whose UI is the system
  observing itself.
