# Porting SnitchOS to the VisionFive 2 (StarFive JH7110)

**Status:** 🎉 **M1 FIRST LIGHT ACHIEVED ON HARDWARE — 2026-07-24.** SnitchOS boots
on the VisionFive 2, brings up all four U74s, realises userspace, and heartbeats.
All four blockers shipped and are now **hardware-proven**, not just QEMU-green:
**B4 (console) ✓, B1 (SBI timer) ✓, B6 (multi-hart) ✓, B2 (RAM base) ✓.**

Board console at first light:

```
memory: 0x40000000 (4294967296 bytes)   ← B2: RAM base + 4 GiB, higher-half OK
timebase: 4000000 Hz                    ← B1: 4 MHz, from the board DTB
uart: 0x10000000                        ← B4: snps,dw-apb-uart @ reg-shift=2
virtio-console: init failed: NotFound    ← expected; no virtio on hardware (→ M2)
I am alive
smp: starting hart 1 (mhartid 1) … up   ← B6: all three secondaries up
smp: starting hart 2 (mhartid 2) … up
smp: starting hart 3 (mhartid 3) … up
entering heartbeat
hb 1 … hb 5                              ← live clock, on real silicon
```

**The boot hartid genuinely varies run to run** (observed mhartid 2 and mhartid 4 as
the boot hart on consecutive boots); `assign_logical` mapped it to logical 0 every
time. The old `1 - hart_id` could not have survived this — B6 was load-bearing.

Delivery: `cargo xtask image` → RISC-V Image (header embedded in `entry.S`) →
TFTP → `booti`. See [Bring-up mechanics](#bring-up-mechanics-m0-in-detail).

**No board-only deviations remain.** `ramfb::init` is still not called on the
board — fw_cfg is a QEMU invention that doesn't exist there — but that is now a
*runtime* answer, not a `cfg(vf2)` gate: `dtb::has_fw_cfg` asks the DTB for a
`qemu,fw-cfg-mmio` node and the board simply hasn't got one. The `unmap_identity`
deviation is **gone** — see the fixed callout below.

> ### ✅ FIXED — latent bug found at first light: `kmain`'s frame straddled the trampoline
>
> `entry.S` enters `kmain` with a **physical** `sp`; the higher-half trampoline then
> shifts `sp` *in the middle of* `kmain`. In a debug build `kmain`'s frame is large,
> so the compiler materialises and **spills addresses of its own locals** — any
> computed pre-trampoline stay **physical forever**. They work only while the
> identity map is live, and fault the instant `unmap_identity` runs (observed:
> `scause=0xf stval=0x40506de0 sepc=…`, a store to a physical boot-stack address on
> the very next `println!`). **This was not board-specific** — QEMU's codegen just
> happens not to spill one that survives, so it shipped green for months.
>
> **Fixed (2026-07-24), hardware-confirmed.** `kmain` is now pre-trampoline only
> (MMIO regions, `mmu::enable`, the trampoline) and hands off to
> `#[inline(never)] kmain_higher_half`, whose frame is allocated *after* the `sp`
> shift — so no post-trampoline local can have a physical address. The DTB is parsed
> in the new frame too. `unmap_identity` is unconditional again on every target, and
> the board boots clean through it to a live heartbeat. `#[inline(never)]` is
> load-bearing: inlining it back would recreate the straddling frame.
>
> **The rule this leaves:** *no cached address, and no stack frame, may span the
> trampoline.* Third instance of the family after the `tp` truncation and the
> `entry_pa` loop-invariant miscompile — fixed structurally, not case-by-case.

Next: **M2 — telemetry over UART** (the `hb` line above is a debug `println!`, not
the frame pipeline; the board has no telemetry transport yet). Of the smaller
debts (2026-07-24): the ~21 `vf2` dead-code warnings are **gone**, and the
`cfg(vf2)` ramfb skip is **replaced by DTB discovery** (`dtb::has_fw_cfg`). The
`ph:` phase markers were dropped and then **deliberately restored** as a `ph!`
macro — deleting them cost us the only diagnostic the board has, and they earned
their keep the same day; they stay with `smp:`/`hb` until M2's real frames replace
them, at which point removing the macro removes all 18 call sites at once. See the
callout below for what that episode actually taught.

> ### ✅ FIXED — the SBI ecall clobber: `a1` is a return register, not an input
>
> Adding the boot banner turned itest from 121/121 green into *every scenario
> failing*, and the board stayed perfectly happy. The banner was not the bug. All
> three SBI wrappers in `kernel/src/sbi.rs` under-declared their clobbers:
>
> | call | was | wrong because |
> |---|---|---|
> | `set_timer` | `a1` not mentioned | compiler assumes a1 survives the `ecall` |
> | `send_ipi` | `in("a1")` | `in` *promises* the register is unchanged |
> | `hart_start` | `in("a1")` | same |
>
> SBI returns `sbiret { error, value }` in **a0 and a1**, so firmware overwrites
> a1 on every call. The compiler, told otherwise, parked the `PER_HART_DATA` base
> in a1 across `sbi_set_timer`, read back `value == 0`, and the trap handler's
> per-hart counter did `amoadd.d` at `0 + idx*8 + 0x40` — the observed
> `scause=0xf stval=0x40`. Fix: `lateout("a1") _` on `set_timer`,
> `inlateout("a1") … => _` on the other two. (a2–a7 stay `in`: SBI preserves
> everything except a0/a1.)
>
> **This was latent on every SBI call since v0.6**, `hart_start` included. The
> banner only perturbed register allocation until a1 drew the short straw. The
> board escapes it because `cargo xtask image` builds a **debug** profile; itest
> builds release. Fourth member of the family after the `tp` truncation and the
> `entry_pa` hoist: *a value the compiler believes it owns across a boundary that
> actually clobbers it.*
>
> It also silently fixed the `heap-oom` failure that looked like a separate bug —
> heap growth is heartbeat-driven, so the corrupted timer path broke it too. Two
> symptoms, one cause.
>
> **How it was actually found**, because none of the intuitions worked: the
> harness was silent for minutes, and four successive theories (a `fw_cfg` bus
> probe, `putchar`/THRE spinning, release-vs-debug, snemu's own optimizations)
> were each disproven. What cracked it was **narrating itest's phase-1 boot task**
> (`boot task starting` / `completed` / `FAILED`), which converted "the harness
> hangs" into "the boot task never reached CHECKPOINT" — and then running that
> workload directly (`snemu boot --release --workload init`), which turned a
> silent hang into a panic with an `sepc` to disassemble. Lesson: when a harness
> goes quiet, instrument the harness before theorising about the guest.
>
> **Still open (minor):** `run_until_uart` (`snemu/src/machine.rs`) rescans the
> entire accumulated UART buffer with `windows(marker.len()).any(…)` every time
> output grows — O(n²) in UART bytes, and the banner made boot output ~14×
> longer. Not the hang (host-side cost, no guest steps), but worth fixing with a
> scan offset that only tests windows overlapping the newly-appended bytes.

The section below is the
original scoping; per-blocker ✓ notes mark what shipped.

> ### ⚠️ Lesson — a stale image looks exactly like a regression
>
> Clearing these debts cost two board flashes and a wrong diagnosis, and the
> actual cause was neither of the code changes under suspicion: **the board was
> booting a stale `snitchos.img`** — `cargo xtask image` hadn't been re-run, so
> both "hangs" were the *previous* image running the *previous* code.
>
> The tell was in the capture the whole time and got missed twice: the banner
> source prints a `====` rule above and below the art, and neither rule appeared
> in either capture. Output that doesn't match the source you're holding is the
> signature of a stale artifact — check that **before** theorising about the
> hardware. On a board there is no compiler to tell you the binary is old.
>
> The wrong diagnosis is worth recording too. A runtime `fw_cfg` **signature
> probe** (select `FW_CFG_SIGNATURE`, read four bytes, compare to `QEMU`) was
> blamed for the hang and reverted. It was never running on the board, so it was
> never shown to be at fault — the confirmation was an artifact of the stale
> image, and "the symptom persisted after I reverted my change" should have been
> read as *exonerating* evidence immediately, not as a second mystery.
>
> **`dtb::has_fw_cfg` is still the right design**, on its own merits rather than
> on that non-existent hardware evidence: a load to an address with no responding
> slave need not fault and need not return garbage — on a fabric with no default
> slave it can simply never complete. "Four reads, no polling loop" bounds
> *instructions*, not *time*. QEMU always answers, so no host test could ever
> exercise the case. Presence is a question firmware already answered in the DTB;
> read that instead of poking the address to find out. Same discipline B4 used
> for the UART. (Board-confirmed: `ph: pre-ramfb (fw_cfg in dtb: false)`.)
>
> Process footnote: the `ph:` markers were deleted in the same change that
> introduced the probe, so the board's only diagnostic was "stops after the
> banner." Restoring them localised the truth in a single flash. Land risky
> changes *while* the instrumentation still exists — and keep board
> instrumentation until something better replaces it (M2/B3).

> B6 was re-scoped during the work: it turned out to be an **M3 (SMP)** item, not an
> M1 gate (the boot hart is hardcoded logical 0, so a non-zero boot hartid never
> OOBs). It was done ahead of schedule as the full multi-hart generalization — see
> [plans/legacy/vf2-b6-multihart.md](legacy/vf2-b6-multihart.md).

## The one-sentence thesis

> The CPU is not the port. SnitchOS is already RV64GC / Sv39 / higher-half /
> SBI-driven, which is exactly what the JH7110's U74 cores are. The port is
> (a) swapping the handful of QEMU-synthetic devices — above all the
> **virtio-console the entire telemetry story rides on** — for real transports,
> and (b) fixing three hardware assumptions baked into boot: the timer
> extension, the RAM base, and how we even get bits onto the board.

Put differently: almost none of the *interesting* kernel (frame allocator,
scheduler, caps, userspace, IPC, RAMfs) is in scope. It's hardware-agnostic and
should run unchanged the moment the machine can keep time and emit a frame. The
work is concentrated at the metal boundary.

## The target, briefly

| | QEMU `virt` (today) | VisionFive 2 / JH7110 |
|---|---|---|
| Cores | `-smp 2`, U54-ish, hartids 0..N | 4× SiFive **U74** (RV64GC) + 1× S7 monitor; boot hart is a U74 |
| RAM base | `0x8000_0000` | **`0x4000_0000`** (2/4/8 GB parts) |
| Timer | **Sstc** (`rdtime`/`stimecmp`) | U74 has **no Sstc** — SBI `set_timer` or M-mode CLINT |
| Console UART | ns16550a @ `0x1000_0000` | `snps,dw-apb-uart` @ `0x1000_0000` (16550-compatible, DesignWare) |
| Telemetry | **virtio-console** (virtio-mmio) | **does not exist** |
| Framebuffer | fw_cfg + ramfb | **does not exist** (real display = JH7110 DC/HDMI driver) |
| PLIC / CLINT | unused (all I/O polled) | SiFive PLIC @ `0x0C00_0000`, CLINT @ `0x0200_0000` (only needed if we go interrupt-driven) |
| Firmware | OpenSBI `fw_dynamic`, S-mode handoff `a0=hartid a1=DTB` | **same** (OpenSBI → U-Boot → payload) — the one big thing that carries over cleanly |
| Delivery | `qemu -kernel elf` | build `Image`, U-Boot `booti` from SD/eMMC/TFTP |

## Ground truth (measured on the board, 2026-07-23)

Read off a live Ubuntu 24.04 boot + the `StarFive #` U-Boot prompt on the actual
**VF2 v1.3B** (JH7110, 4 GiB), post firmware-update to U-Boot 2025.10 / modern
OpenSBI. These retire most of the "measure on the board" unknowns; see
[../notes/visionfive2-first-boot-and-firmware-update.md](../notes/visionfive2-first-boot-and-firmware-update.md).

| Fact | Value | Feeds |
|---|---|---|
| RAM | base `0x4000_0000`, size 4 GiB → `0x1_4000_0000` | B2 |
| OpenSBI M-mode reserved | `[0x4000_0000, 0x4006_0000)` (384 KiB) — keep kernel above | B2 |
| **Kernel load address (LMA)** | **`0x4020_0000`** (`kernel_addr_r`; FIT loads U-Boot here too) = RAM base **+ 2 MiB, same offset as QEMU** | B2 |
| `fdt_addr_r` | `0x4600_0000`; U-Boot passes its DTB via `${fdtcontroladdr}` | handoff |
| **Boot hartid** | U-Boot on **hart 2**, handed Linux **hart 4** — arbitrary in **1..4, never 0** (S7 = hart 0, disabled) | B6 |
| ISA | `rv64imafdc` + `zba zbb zicntr zicsr zifencei zihpm zaamo zalrsc zca zcd` — **no `sstc`** | B1 |
| timebase-frequency | **4 MHz** (`0x3D0900`) | B1 |
| SBI | OpenSBI, spec v3.0; ext: **TIME, IPI, RFENCE, SRST, DBCN, FWFT, HSM, PMU** | B1, M0.5, SMP |
| Console UART0 | `serial@10000000`, `snps,dw-apb-uart`, **`reg-shift=2`, `reg-io-width=4`** (32-bit regs, 4-byte stride), clock 24 MHz | B4 |
| CLINT / PLIC | `0x0200_0000` / `0x0C00_0000` (both < `0x4000_0000`, inside identity MMIO gigapage; same as QEMU) | B5 |
| NIC (future) | 2× `snps,dwmac-5.20` @ `0x1603_0000`/`0x1604_0000`, PHY **Motorcomm YT8531**, RGMII-id, MDIO | M2.5 |
| QSPI layout | `spl@0` `[0,0xf0000)`, `uboot-env@0xf0000`, `uboot@0x100000` | firmware |

**Identity-map consequence for B2:** the RAM identity gigapage moves from QEMU's
root entry 2 (`[0x8000_0000, 0xC000_0000)`) to **entry 1** (`[0x4000_0000,
0x8000_0000)`); the linear-map base shifts to `0x4000_0000`. The MMIO identity
gigapage `[0, 0x4000_0000)` is unchanged and already covers UART/CLINT/PLIC. (Full
4 GiB needs more than one leaf, but first-boot only touches the low 1 GiB.)

## Gap analysis, by subsystem

Ordered by how much it blocks a first boot. Each cites where the assumption
lives so the eventual plan has anchors.

### B1 — Timer: Sstc → SBI `set_timer` *(hard blocker)* — ✓ SHIPPED

> Went SBI-only (deleted the Sstc `stimecmp` path): `SbiClock` arms via
> `sbi_set_timer`, reads via `rdtime`. Required **teaching snemu the SBI TIME
> extension** (it serviced only IPI/HSM) so the one code path stays tested on both.

`SstcClock` reads `rdtime` and arms `stimecmp` (CSR `0x14d`) directly
(`kernel/src/trap/mod.rs:95-113`). That's the Sstc extension. QEMU implements
it; the JH7110 U74 cores do not. Without a timer there is no heartbeat, so the
OS has no pulse on the board at all.

- **Work:** a `Clock` impl behind the same trait that arms the timer via SBI
  `sbi_set_timer` (legacy EID `0x0`, or TIME extension). `rdtime` for *reading*
  the clock is fine (U74 has the `time` CSR); only the *arm* has to change.
- **Nice property:** the timer is already trait-shaped (`SstcClock` is one impl),
  and timebase-frequency already comes from the DTB, not a constant
  (`kernel/src/main.rs:153-156`, `kernel/src/dtb.rs:29-43`). So this is a
  swap-in, not a rework.
- **Risk:** low-medium. SBI timer is the oldest, most universally-supported SBI
  call. Main unknown is whether we keep `Sstc` for QEMU and pick at runtime, or
  just move everything to SBI (SBI works on QEMU too — probably simplest to go
  SBI-only and delete the Sstc path, keeping one code path tested on both).

### B2 — RAM base `0x8000_0000` → `0x4000_0000` *(hard blocker)* — ✓ SHIPPED

> Single `kernel_mem::mmu::RAM_BASE` (feature-gated); `build.rs` generates
> `linker.ld` from `linker.ld.in`. `KERNEL_OFFSET` needed no change. QEMU binary
> byte-identical; the `vf2` feature flips LMA/VMA + the identity/linear gigapages.

The load address, the identity gigapage, and the linear map all hardcode RAM at
`0x8000_0000` (`kernel/linker.ld:3,10`; `kernel/src/mem/mmu.rs:194-262`). On the
JH7110, DRAM starts at `0x4000_0000`, so the S-mode payload lands there and every
one of those constants is wrong.

- **Work:** parameterise the RAM base. Cleanest is a single `RAM_BASE` constant
  the linker script, `pa_to_kernel_va`, and the identity/linear gigapage setup
  all derive from. The higher-half offset math is base-relative already; it's the
  *identity* mappings and the LMA that pin `0x8000_0000`.
- **Risk:** medium. Touches the most delicate boot code (pre-MMU, higher-half
  trampoline). Read `plans/v0.4-memory-findings.md` **first** — this is exactly
  the address-translation minefield it documents.
- **Open question:** what physical address does VF2's U-Boot actually `booti` the
  payload to? That fixes the LMA. Needs to be measured on the board (or read from
  the U-Boot `booti` convention), not assumed.

### B3 — Telemetry transport: virtio-console → UART framing *(the real design work)*

This is the port's *point*. The postcard `Frame` stream is SnitchOS's entire
reason to exist, and today it rides a virtio-console over virtio-mmio
(`kernel/src/device/virtio_console.rs`). The address is DTB-discovered, so it's
portable *in principle* — but the JH7110 has no virtio-console device to
discover. The frames need a physical wire.

- **Chosen direction:** frame the telemetry over a **physical UART**. The board
  has multiple UARTs; dedicate a second one to the frame stream (keep UART0 for
  the human `println!` log), or multiplex if only one serial line is wired out.
- **Why this is clean:** the sink is already a trait (`FrameSink` in
  `kernel-obs`). A `UartFrameSink` that writes postcard bytes to a UART is a new
  impl, not a kernel rewrite. The host side changes too: `collector` reads a
  **serial port** instead of a Unix socket — a new source adapter behind the
  existing decoder.
- **Risk:** medium. The subtleties are framing (postcard frames need
  length-delimiting or a COBS-style boundary on a raw byte pipe — virtio gave us
  message boundaries for free), backpressure (UART is slow; the heartbeat emits a
  lot), and flow control. This is where the genuinely new design lives.
- **Deferred alternative:** keep virtio-console for QEMU and add UART framing for
  hardware, selected at boot. Probably worth it — don't regress the QEMU/snemu
  path that the whole test gate depends on.
- **No "wait for the collector" on the board.** `x boot` today passes QEMU
  `socket,…,server=on,wait=on`: the hypervisor *freezes the guest until the
  collector connects*, giving lossless capture from frame 0. Real hardware has no
  such lever — the U74 runs on power-up and can't block on a host consumer. So the
  board model inverts: **run the collector first (a persistent serial reader), then
  reset the board** — the reset is the sync point, not a socket connect. Telemetry
  is a **fire-and-forget UART stream**. Consequences for the design:
  - The `UartFrameSink` **must never block on backpressure** (UART is slow, the
    heartbeat is chatty) — drop-and-count, same discipline as the alloc/IRQ
    deferred-emission paths, never stall the kernel waiting for a reader.
  - Startup-window loss is small: the kernel's pre-init buffer flushes once the
    sink is up, so if the host reader is attached by then, early frames still
    arrive (the `Dropped(N)` frame reports any overflow).
  - To *recover* losslessness (some demos want frame 0): an app-level "reader
    ready" handshake — the sink waits for a byte on UART RX before flushing the
    pre-init buffer (~10 lines, adds an RX dependency to boot). Or a `ser2net`
    bridge on the host to get connect-then-read ergonomics over TCP (no
    losslessness, but the collector connects rather than free-reads).

### B4 — UART discovery: match `snps,dw-apb-uart` *(easy, but blocks console)* — ✓ SHIPPED

> Accepts both compatible strings + reads `reg-shift`/`reg-io-width` (default 0/1).
> Driver computes offsets `reg << reg_shift` (host-tested in `kernel_devices::uart`)
> and does width-dispatched MMIO (32-bit for `reg-io-width=4`). Pre-init/emergency
> paths still QEMU-byte-default — a documented board follow-up.

DTB lookup hardcodes `compatible = "ns16550a"` (`kernel/src/dtb.rs:12-18`); the
comment there already flags that boards reporting `snps,dw-apb-uart` won't match
— which is exactly the JH7110. The *driver* is fine: it's polled and
16550-register-compatible (`kernel/src/device/uart.rs`), and it deliberately does
no baud/divisor init, relying on OpenSBI's config — which holds on VF2 too.

- **Work:** add `"snps,dw-apb-uart"` to the accepted compatible strings **and
  handle the register stride** — the board reports `reg-shift=2` / `reg-io-width=4`
  (32-bit registers spaced 4 bytes apart), vs QEMU's byte-spaced ns16550a. The
  driver hardcodes the QEMU stride, so it must parameterize the shift (from the DTB
  `reg-shift`) or it pokes the wrong offsets. So: compatible string **+ register
  stride**, not a one-liner.
- **Risk:** low (measured, mechanical) — but not zero; the stride is easy to get
  wrong and produces garbage or silence.

### B5 — DTB-before-MMU crash / MMIO hardcoding *(latent blocker for portability)*

MMIO regions are hardcoded (`mmu::QEMU_VIRT_MMIO_BASE = 0x1000_0000`,
`kernel/src/main.rs:72-76`) because DTB *iteration* pre-MMU crashes under the
higher-half link — the parser exists but is parked (`collect_mmio_regions`,
`kernel/src/mem/mmu.rs:36-55`, `#[expect(dead_code)]`).

- **Good luck:** the JH7110 UART is *also* at `0x1000_0000`, and it's inside the
  identity MMIO gigapage `[0, 0x4000_0000)`, so the hardcoded MMIO window
  probably still covers what we need for a first boot. We may not have to fix the
  DTB crash on day one.
- **But:** relying on hardcoded QEMU addresses on real hardware is exactly the
  fragility that bites later. Reviving `collect_mmio_regions` (or parsing the DTB
  in the physical identity window *before* the higher-half switch) is the
  principled fix and the thing that makes the port generalise beyond this one
  board. Flag as a fast-follow, not a milestone-0 gate.
- **Risk:** medium, and annoying to debug (silent pre-MMU crash).

### B6 — hart topology: `MAX_HARTS=2` / `1 - hart_id` → real ids *(now a FIRST-BOOT blocker)* — ✓ SHIPPED (as M3)

> Correction: **not** a first-boot blocker — the boot hart is hardcoded logical 0,
> so a non-zero boot hartid never OOBs `PER_HART_DATA`. Done as the full multi-hart
> generalization (DTB `/cpus` enumeration → dense logical ids, drop `1 - hart_id`,
> per-hart secondary stacks, bring-up loop, a 4-hart `smp4` workload). Full writeup:
> [plans/legacy/vf2-b6-multihart.md](legacy/vf2-b6-multihart.md). En route it caught
> a real release-build miscompile (a loop-invariant `entry_pa` read back 0 on the
> 2nd bring-up iteration — fresh-per-iteration `lla` was the fix).

Bringup is proper SBI HSM (`sbi::hart_start`, `kernel/src/main.rs:453-472`) which
is portable — but `MAX_HARTS = 2` (`kernel/src/smp/percpu.rs:80`) and the
`secondary = 1 - boot_hart` arithmetic (`main.rs:139,456,461`) hardwire exactly
two harts with ids `{0,1}`.

**The board disproves the "deferrable" assumption.** Measured boot hartid is **2
or 4** (never 0; U74s are harts 1–4, S7 is 0). `percpu::init` indexes
`PER_HART_DATA[hartid]` — so a boot hartid of 2–4 with `MAX_HARTS=2` reads/writes
**past the array before the kernel prints anything**. This gates M1 (single-hart
first light), not just SMP.

- **Work (minimum, for M1):** size the per-hart arrays for physical hartid ≤ 4
  (`MAX_HARTS ≥ 5`, S7 at 0 included) **or** remap physical hartid → logical index
  before the first per-hart access. Boot only the boot hart.
- **Work (full, later):** iterate the U74 harts from the DTB; drop the `1 -
  hart_id` "other hart" arithmetic. Per-hart arrays, runqueues, exception stacks
  all sized by `MAX_HARTS`.
- **Risk:** medium. The M1 slice (array sizing / remap) is small but **mandatory**
  — a silent OOB at boot is exactly the no-output failure M0.5 exists to avoid.

### Dropped for now — framebuffer (fw_cfg / ramfb)

fw_cfg is a QEMU invention at a hardcoded port (`kernel/src/device/fwcfg.rs:18`,
`0x1010_0000`) and ramfb rides it (`kernel/src/device/ramfb.rs`). Neither exists
on hardware. A real display means writing a JH7110 display-controller + HDMI
driver — a whole project. **Out of scope.** The framebuffer / physics-desktop
work stays QEMU-only until someone wants to write that driver. ✓ Guarded by
`dtb::has_fw_cfg` — a machine whose DTB declares no `qemu,fw-cfg-mmio` node never
calls `ramfb::init`, so no build flag is involved and no MMIO is touched.

## Proposed milestones (risk-ordered)

Each leaves the tree in a known-good state and — critically — **does not regress
the QEMU/snemu test gate**, which everything else in the repo depends on.

- **M0 — Serial console + delivery loop.** ✅ **Mostly done (2026-07-23):**
  CP2102 serial adapter wired to the console UART, `StarFive #` prompt reached,
  stock Ubuntu booted — board + firmware + toolchain proven, and ground-truth
  values read off the live board. **Remaining:** TFTP server on the Mac for the
  `bootelf`/`tftpboot` dev loop. Full mechanics in
  **[Bring-up mechanics](#bring-up-mechanics-m0-in-detail)** below.

- **M1 — First light: boot to the human UART log.** 🎉 **ACHIEVED ON HARDWARE
  (2026-07-24).** B4 (`snps,dw-apb-uart`) + B2 (RAM base) + B1 (SBI timer) — plus
  B6 (multi-hart) for free — all confirmed on real silicon: the boot log *and* a
  live heartbeat over the DesignWare UART, with all four U74s brought up and
  userspace realised. MMU, higher-half, trampoline, and time all work on hardware —
  the scariest 20%, done. Console transcript + the caveats are in the Status block
  at the top. Two board-only deviations carried: `ramfb::init` skipped (no fw_cfg)
  and `unmap_identity` temporarily skipped (pending the `kmain`-frame split).

- **M2 — Telemetry on hardware.** B3: `UartFrameSink` + collector serial source.
  Success = spans and metrics from the real board decoded by the collector and
  landing in Grafana. **This is the milestone that makes the port *mean*
  something** — SnitchOS observing real hardware is the whole pitch.

- **M2.5 (optional, big) — Ethernet-native telemetry.** Replace/augment the UART
  frame sink with a real JH7110 GMAC driver + minimal ARP/IP/UDP, streaming
  frames to the collector over the network. See
  **[Ethernet: three different asks](#ethernet-three-different-asks)** — this is
  its own project (NIC driver + PHY bring-up + DMA rings), *not* a rider on M2.
  Thematically the strongest milestone (an observability OS streaming telemetry
  over the wire = what real production nodes do), but scoped as a deliberate,
  separate step. Boot-over-Ethernet (TFTP) is unrelated to this and comes for
  free at M0.

- **M3 — Multi-hart.** B6: generalise `MAX_HARTS` and hart iteration, bring up
  all four U74s. Success = the SMP itest scenarios' spirit (cross-hart
  producer/consumer) running on real cores.

- **M4 (deferred) — Portability hardening.** B5: real DTB-driven MMIO discovery
  so the kernel isn't leaning on QEMU addresses that happen to coincide.

- **Not planned — framebuffer.** Separate project; needs a native display driver.

## Bring-up mechanics (M0 in detail)

The practical loop, worked out. **No soldering** — the VF2's 40-pin GPIO header
ships pre-populated with pins; you push jumper wires straight on.

**Hardware you need:**

- **USB-to-TTL serial adapter — 3.3V** (CH340 / CP2102 / FTDI, ~$5–10). Your
  console; without it a failed bare-metal kernel is just a dark board. **Must be
  3.3V logic** — the header is 3.3V-only and a 5V adapter can *damage the board*.
- 3× female-female dupont jumper wires (usually bundled with the adapter).
- microSD card (8 GB+) + a host SD reader — for the one-time known-good image and
  as a fallback boot source.
- USB-C power supply (5V/3A).
- Ethernet: **either** a LAN cable from the board to your home router (Mac stays
  on WiFi — same subnet, works; see topology note), **or** a USB-C Ethernet
  adapter for a direct Mac↔board cable. See the Ethernet section for the choice.

A labeled schematic board map + 40-pin UART pinout lives at
[`docs/visionfive2-board-map.html`](../docs/visionfive2-board-map.html) (open in a
browser).

**Serial console wiring** (verified against the VF2 Quick Start): board
**pin 6 → GND**, board **pin 8 (UART0 TX) → adapter RX**, board **pin 10 (UART0
RX) → adapter TX** (TX/RX cross over). Terminal at **115200 8N1**.

**Boot chain:** BootROM → SPL → OpenSBI → **U-Boot** (all in onboard SPI-NOR
flash, preloaded on current boards) → *U-Boot loads SnitchOS*. You live at the
U-Boot prompt; that last arrow is the only part we drive. One-time exception: an
early board rev that didn't ship U-Boot in flash needs it written once via the
UART-recovery path (boot-mode jumpers **Switch_2 = UART = `RGPIO_1,RGPIO_0 =
1,1`**, run StarFive's recovery tool, set jumpers back). Most current boards skip
this.

**The dev loop (TFTP boot — the good one):** U-Boot has its own network stack, so
booting over Ethernet is *free and needs zero kernel code* — it happens before
SnitchOS runs. Mac runs a **TFTP server** (built-in `tftpd`, or `brew install
tftp-hpa` / `dnsmasq`) pointed at a folder; the kernel image lives in that folder.
At the U-Boot prompt:

```
dhcp                                    # or static: setenv ipaddr <board-ip>
setenv serverip 192.168.1.50            # the Mac's IP
tftpboot 0x40200000 snitchos.img        # pull kernel into RAM
booti 0x40200000 - ${fdtcontroladdr}    # boot it; ${fdtcontroladdr} = U-Boot's own DTB
```

`${fdtcontroladdr}` handing over U-Boot's DTB is a freebie: it satisfies
SnitchOS's `a1 = DTB` handoff with the board's *real* devicetree, no DTB of our
own to supply. Save these into U-Boot's `bootcmd` and the board auto-fetches on
reset — rebuild on the Mac → power-cycle → fresh kernel, no SD shuffling.

**Delivery methods — three ways to land `snitchos.img` in RAM at `0x4020_0000`,
then the same `booti 0x40200000 - ${fdtcontroladdr}`.** Build the Image with
`cargo xtask image` (objcopy of the `vf2` kernel; the 64-byte RISC-V Image header
is embedded in `entry.S`, so it's a straight ELF→binary copy — `booti`/`e_entry`
both land on `code0`, a `JAL` over the header).

- **TFTP over Ethernet** *(the iteration loop, above)* — fast, and `bootcmd` can
  auto-fetch on reset. Needs the board and Mac on one network: board→router by
  cable + Mac on Wi-Fi (same subnet, **main** Wi-Fi not guest), or a direct
  Mac↔board cable (USB-C Ethernet adapter + static IPs). `serverip` = the Mac's IP
  on that network (`ipconfig getifaddr en0`). Mac TFTP server: `dnsmasq
  --enable-tftp --tftp-root="$(pwd)" --port=0 --no-daemon` (serves the repo dir +
  logs requests) or the built-in `tftpd` (`/private/tftpboot`).
- **microSD** — no network at all: FAT32-format an SD on the Mac, copy the Image,
  read it on the board:
  ```
  mmc dev 1                                 # microSD is mmc 1; eMMC is mmc 0
  fatload mmc 1:1 0x40200000 snitchos.img
  booti 0x40200000 - ${fdtcontroladdr}
  ```
  Pop the card each rebuild — slower loop, but uses only the SD reader you already
  have and doesn't touch the SPI firmware.
- **Serial Y-modem over the console UART** — needs *only the UART cable*:
  `loady 0x40200000` at U-Boot, send `snitchos.img` from a Y-modem-capable terminal
  (`minicom` `Ctrl-A S`, `lrzsz`), then `booti …`. But ~4 min per boot at 115200 for
  the 2.75 MB debug image. The pure-one-cable fallback.

**With only the UART adapter on hand** (no Ethernet adapter yet, router across the
room), **microSD is the pragmatic first-boot path**; serial Y-modem is the
zero-extra-gear fallback; TFTP is the loop to switch to once the adapter arrives.

## Ethernet: three different asks

"Use Ethernet for everything" splits into three asks at very different layers —
one is nearly free, two are a real project. Don't conflate them.

1. **Boot the kernel over Ethernet — FREE, works today.** Pure U-Boot TFTP (see
   above). Happens *before* our kernel runs; no SnitchOS code. Do this now.
2. **Read logs / telemetry from the running kernel over Ethernet — needs a NIC
   driver.** U-Boot's network stack evaporates at `booti`; the running kernel has
   the bare machine. For SnitchOS to speak Ethernet it must drive the JH7110
   **Synopsys DesignWare GMAC** (`dwmac`): MAC init, **PHY bring-up over MDIO**
   (board-specific reset GPIO + clock config in the JH7110 syscon — where
   bare-metal NIC bring-up eats days), **DMA descriptor rings**, then a **minimal
   ARP/IP/UDP** to address frames at the Mac. This is M2.5 — bigger than the rest
   of the port combined to get the *first* frame out.
3. **Send messages *to* the kernel over Ethernet — NIC driver + RX path + a
   command protocol.** Same driver dependency plus new inbound surface the kernel
   has no notion of today. Furthest out; thematically aligned with the
   actor-model / typed-processes design (processes reachable as network
   endpoints).

**Recommendation: don't couple them.** UART carries telemetry first (M2 — the
`FrameSink` is already a swappable trait, the collector already speaks network on
the host side), Ethernet-native is the M2.5 upgrade once you want speed +
bidirectionality. UART-now isn't throwaway: the sink is abstracted and the B3
framing design mostly carries over.

**One codebase tailwind for the GMAC path:** the DMA-descriptor discipline isn't
foreign — SnitchOS already learned "devices see physical addresses" (the
`TX_STAGING` / `va_to_pa` staging in `virtio_console`) and has the linear-map
machinery to hand PAs to a device. The NIC is just the most DMA-heavy device it'd
have written.

**Pragmatic shortcut for "network logs" before the driver exists:** bridge on the
host — run a serial-to-TCP bridge (`ser2net`) on the Mac so the board's UART
telemetry is reachable over a TCP socket by the collector. "Read logs over the
network" with zero kernel work; just not the board speaking IP natively.

**Host↔board network topology (either works, no crossover cable — modern GbE is
auto-MDI-X):**

- **Board→router by cable, Mac on WiFi.** Simplest, *no USB-C adapter needed* — a
  home router bridges WiFi + wired into one subnet, so they can talk. Router does
  DHCP. Caveat: use your **main** WiFi, not a guest network (guest/"AP isolation"
  blocks device-to-device traffic).
- **Direct Mac↔board cable.** Needs the USB-C Ethernet adapter. No DHCP, so set
  **static IPs** both ends (e.g. Mac `192.168.2.1`, board `192.168.2.2`, `/24`;
  Mac adapter → *Configure IPv4: Manually*, gateway blank; board U-Boot `setenv
  ipaddr` / `serverip`). Mac keeps WiFi for internet on a separate interface —
  macOS routes board traffic out the cable, everything else over WiFi. Fully
  self-contained + deterministic (works with no router anywhere); the cost is the
  adapter + a one-time IP config.

## Biggest unknowns to de-risk early

1. ~~**Delivery + serial loop (M0).**~~ **Mostly done:** serial console is wired
   and working (that's how the ground-truth values above were read), board boots
   stock Ubuntu, `StarFive #` prompt reachable. Remaining M0 bit: stand up the
   TFTP server on the Mac for the `tftpboot`/`bootelf` dev loop.
2. ~~**The `booti` load address on VF2.**~~ **Resolved:** `0x4020_0000` (= RAM
   base + 2 MiB, same offset as QEMU). See ground-truth table.
3. ~~**Does OpenSBI hand off S-mode identically?**~~ **Mostly resolved:** standard
   `a0=hartid`, `a1=DTB` handoff, modern OpenSBI (SBI v3.0, DBCN/TIME/HSM present).
   The *live* wrinkle is the **non-zero, non-deterministic boot hartid (1–4)** —
   see B6, now a first-boot blocker.
4. ~~**How does U-Boot launch a bare kernel?**~~ **Resolved (both, staged):**
   `bootelf` loads our existing ELF and jumps to `e_entry` — **zero build step**,
   but does *not* set `a0=hartid`/`a1=dtb` (no boot protocol). `booti addr -
   ${fdtcontroladdr}` *does* guarantee that handoff but needs a RISC-V **Image**
   (64-byte header, `code0 = j _start`) we must emit. Plan: **M0.5 smoke via
   `bootelf`** (DBCN needs no `a0`/`a1`), **M1 via `booti` + Image header** (for
   the real hartid/DTB handoff). `${fdtcontroladdr}` = U-Boot's live DTB
   (`0xff7105e0` on this board). Both paths require B2 first (kernel linked at
   `0x4020_0000`).
5. **UART framing design (B3).** The one genuinely new design problem —
   boundaries, backpressure, flow control on a raw byte pipe. Worth a small
   design note of its own before M2.

## What this port is *not*

It is not a rewrite, and it is not "make RISC-V run on RISC-V." The kernel's hard
parts already run on the U74's exact ISA. The port is a thin, sharp shell of
metal-boundary work — a timer call, a base address, a compatible string, and one
real transport design — wrapped around a body of code that doesn't need to
change. The risk is concentrated, not spread; M0 and M1 hold almost all of it.

## Reference docs

StarFive's docs are unusually good for an SBC vendor. Mapped to the blockers:

- **JH7110 TRM** — <https://doc-en.rvspace.org/JH7110/PDF/JH7110_TRM_StarFive_Preliminary_V2.pdf>
  — the main one. System memory map (B2 RAM base, B5 MMIO), UART registers (B4),
  CLINT + timer (B1), PLIC, GMAC (M2.5).
- **JH7110 Datasheet** — <https://doc-en.rvspace.org/JH7110/PDF/JH7110_Datasheet.pdf>
  — hart topology (B6), which ISA extensions the U74 actually implements (confirms
  *no Sstc* → B1).
- **VF2 Datasheet** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_Datasheet.pdf>
  — board pinout (which pins the console UART is on).
- **VF2 Quick Start Guide** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_QSG.pdf>
  — debug-pin wiring + boot-mode jumper table (M0).
- **VF2 40-Pin GPIO Header UG** — <https://doc-en.rvspace.org/VisionFive2/40-Pin_GPIO_Header_UG/VisionFive2_40pin_UG/gpio_pinout%20-%20vf2.html>
- **Recovering the Bootloader** — <https://doc-en.rvspace.org/VisionFive2/SDK_Quick_Start_Guide/VisionFive2_SDK_QSG/recovering_bootloader%20-%20vf2.html>
  — the one-time U-Boot-to-flash path.
- **VF2 SDK Quick Start Guide** — <https://doc-en.rvspace.org/VisionFive2/PDF/VisionFive2_SDK_QSG.pdf>
  — boot chain + flashing.

**What the manuals *can't* tell you** (measure on the board / read the board DTS):
the actual `booti` load address (B2's LMA), the exact DTB layout + non-zero boot
hartid (M1 handoff). SBI semantics (B1 `set_timer`, B6 HSM `hart_start`) are the
[RISC-V SBI spec](https://github.com/riscv-non-isa/riscv-sbi-doc), not StarFive's;
the board just ships an OpenSBI that implements them.
