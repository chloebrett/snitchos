# The JH7110 GMAC driver — design note

> Status: **design, unbuilt.** This is the driver-level design under M2.5's
> [network-telemetry-design.md](network-telemetry-design.md), which treats the GMAC
> as one row in a table ("**None** — bespoke driver … **the monster — weeks**") and
> defers everything below the `NetDevice` trait to here. The implementation plan is
> [../plans/vf2-gmac-driver.md](../plans/vf2-gmac-driver.md); this note is the thing
> that plan's Phase 0 was supposed to produce before its steps 5–7 could be sized.
>
> **Everything here that is marked 🔍 is desk research against mainline Linux, not a
> board measurement.** The board is the authority. The point of writing it down first
> is that a wrong constant found at the desk costs minutes, and the same constant
> found on the board costs a day.

## Why this note exists

The plan already says the right things about *process* — TDD the pure layers, one
observable bit per board increment. What it does not have is the **map**: which
registers, which clock indices, which of two MACs, what the descriptor looks like,
and — the question that decides whether this is weeks or months — whether this SoC
needs a cache-maintenance layer the kernel does not have.

Those are answerable without the board. This note answers them.

## What is already true

Not restated in detail, because it is built and gated on every run: `kernel-net`'s
packet builder, the `NetDevice` trait, `UdpFrameSink`, the `net=` bootarg, virtio-net,
the snemu device model, the collector's `--udp` source. The deterministic
`net-telemetry-over-udp` itest exercises all of it.

**This is a third `NetDevice` impl beside two working ones.** Nothing above the trait
changes. That is the single most load-bearing fact in the whole estimate.

---

## Finding 1 — there are two GMACs, both are wired, and they are not equally cheap

The plan says "the VF2's RJ45 is wired to one of them. Confirm which, and do not
assume the first node is the connected one." The answer is better than that: **on
v1.3B both are wired, both are Motorcomm YT8531, both are `rgmii-id`.** 🔍 mainline
enables `gmac0` in `jh7110-common.dtsi` and `gmac1` in
`jh7110-starfive-visionfive-2.dtsi`; the board memory note already records "2×
`snps,dwmac-5.20` @ `0x1603_0000`/`0x1604_0000`, PHY **Motorcomm YT8531**, RGMII-id"
read off the board's own DTB.

(Board-revision hazard: **v1.2A is not this board.** There `gmac1` is a YT8512 in
*RMII*, and every delay constant below is wrong for it. The board here is v1.3B.)

So the choice is free, and it should be made on cost. It is not close:

| | **GMAC1** | GMAC0 |
|---|---|---|
| MMIO base 🔍 | `0x1604_0000` | `0x1603_0000` |
| Clock + reset controller 🔍 | **SYSCRG @ `0x1302_0000`** | AONCRG @ `0x1700_0000` |
| Reset assert/status offsets 🔍 | **`0x2F8` / `0x308`** | `0x38` / `0x3C` |
| PHY-mode syscon 🔍 | **`sys_syscon` @ `0x1303_0000`**, offset `0x90`, shift `2` | `aon_syscon` @ `0x1701_0000`, offset `0xc`, shift `0x12` |
| `macirq` 🔍 | 78 | 7 |
| **New `kernel-devices` module needed** | **none** | a whole AONCRG model |
| **New megapages to `insert`** | **one** (`0x1600_0000`) | two (`0x1600_0000`, `0x1700_0000`) |

`kernel-devices/src/syscrg.rs` already encodes SYSCRG's exact model — one control
register per clock at `index * 4`, gate at `BIT(31)`, divider in `[23:0]`, resets at
`0x2F8`/`0x308` with release confirmed by polling status. Its own doc comment says it
is "generic enough for any SYSCRG consumer". **GMAC1 is that consumer, at new
indices.** AONCRG is the same *shape* with different offsets — which means bringing up
GMAC0 first would spend a step building a second module before learning anything about
Ethernet.

And `sys_syscon` at `0x1303_0000` sits inside the **same 2 MiB megapage** SYSCRG
already needs — so GMAC1's PHY-mode write costs zero additional mapping.

> **Decision: bring up GMAC1 first.** GMAC0 is a follow-on that reuses every pure
> module and adds only an AONCRG register model — and there is no reason to ever do
> it unless a second interface is wanted for its own sake.
>
> **To verify on the board (cheap):** which physical RJ45 is GMAC1. Plug the cable
> into one jack and read which PHY reports link. U-Boot's `ethact` / `eth0`/`eth1`
> naming is the other tell.

### Clock and reset indices 🔍

```
JH7110_SYSCLK_GMAC1_AHB        97      JH7110_SYSRST_GMAC1_AXI    66
JH7110_SYSCLK_GMAC1_AXI        98      JH7110_SYSRST_GMAC1_AHB    67
JH7110_SYSCLK_GMAC_SRC         99
JH7110_SYSCLK_GMAC1_GTXCLK    100
JH7110_SYSCLK_GMAC1_RMII_RTX  101
JH7110_SYSCLK_GMAC1_PTP       102
JH7110_SYSCLK_GMAC1_RX        103
JH7110_SYSCLK_GMAC1_RX_INV    104
JH7110_SYSCLK_GMAC1_TX        105
JH7110_SYSCLK_GMAC1_TX_INV    106
JH7110_SYSCLK_GMAC1_GTXC      107
```

Mainline's `gmac1` node takes `stmmaceth`(AXI), `pclk`(AHB), `ptp_ref`, `tx`(TX_INV),
`gtx`(GTXC) — five clocks, two resets. Note `TX_INV` and `RX_INV`: **the RGMII clock
polarity is a clock-tree selection on this SoC**, not a PHY register. That is where a
"link up, frames leave, host sees CRC errors" failure will live.

`GTXCLK` is the one whose *rate* matters: 125 MHz for 1000, 25 MHz for 100, 2.5 MHz
for 10 — set from the negotiated speed after link-up, which is what mainline's
`fix_mac_speed` hook does.

> **Simplification available and worth taking: force 100 Mbit.** Telemetry needs
> ~12 MB/s at 100 Mbit against a measured budget far below that
> ([network-telemetry-design.md](network-telemetry-design.md) §"the VF2's GMAC is
> Gigabit; even 100 Mbit is ~12 MB/s"). Advertising 100-full only removes the
> 125 MHz GTXCLK path, the gigabit RGMII timing margin, and the speed-change
> re-clocking — three of the nastiest bring-up variables — for zero cost to the
> milestone. Take gigabit later, as a separate observable step, if ever.

---

## Finding 2 — the coherency question, which nobody had asked

This is the one that could have doubled the estimate, and the answer is good.

The kernel has **no cache-maintenance operations at all**. There is no `cbo.flush`,
no `sifive` cache-flush glue, nothing. A DMA-capable device on a non-coherent bus
would need that built first — and on RISC-V bare metal that is a genuinely awkward
sub-project, because the U74 has **no `Zicbom`** 🔍 (`riscv,isa-extensions` is
`i m a f d c zba zbb zicntr zicsr zifencei zihpm`, no `zicbom`, no
`riscv,cbom-block-size`), so the standard instructions are unavailable and the
fallback is SiFive's M-mode-only `cflush.d.l1`.

**The JH7110 does not need it.** 🔍 Mainline's `jh7110.dtsi` `soc` node carries **no
`dma-noncoherent`** property. Its predecessor the JH7100 famously did, and got a whole
"non-coherent DMA support via SiFive cache flushing" patch series for exactly this
reason. The JH7110 routes peripheral DMA through a coherent port on the `sifive,ccache0`
L2. That absence is deliberate and is why mainline's dwmac needs no cache maintenance
here while the JH7100's did.

The corroborating evidence lines up: no `Zicbom` in the ISA string **and** no
`dma-noncoherent` in the DTB is a consistent pair. Non-coherent + no cache ops would
be an unusable combination, and the JH7110 ships Linux.

> **Consequence: no cache-maintenance layer is in scope.** Descriptors and buffers are
> ordinary memory; ordering is a `fence` away, not a flush.
>
> **Verify on the board in Phase 0, at a cost of one grep:** `dma-noncoherent` must be
> absent from the board's own live DTB, not just from mainline. If it is present, stop
> and re-scope — that is a different plan.

---

## Finding 3 — no RX ring (design note open question 4, answered)

The design note deferred "does the DesignWare MAC config we need bring up an RX ring
whether we want one or not?" to bring-up. It does not have to.

🔍 In dwmac4 the DMA channel's transmit and receive halves are separately configured
and separately started: `DMA_CHAN_TX_CONTROL` bit 0 is `ST` (start transmit),
`DMA_CHAN_RX_CONTROL` bit 0 is `SR` (start receive), at distinct offsets `+0x04` and
`+0x08` of the channel block. The MAC's `GMAC_CONFIG` likewise has `TE` (bit 1) and
`RE` (bit 0) as independent enables. Nothing couples them.

> **Design: program only the TX half. Leave `SR` and `RE` clear, and never allocate an
> RX ring.** The interface is write-only on the wire, which is what
> [network-telemetry-design.md](network-telemetry-design.md) already scoped
> (egress-only, static ARP-free addressing, no inbound path).
>
> **Cost of the choice, stated plainly:** the board will not answer ARP or ping. It is
> invisible except as a source of UDP datagrams. That is exactly the trade the M2.5
> design already made and it should not be silently revisited here. The
> network-console addendum in that note is where an RX ring gets justified, and it can
> add `SR` + a second ring against this same driver later.

---

## Finding 4 — poll, don't interrupt (open question 3, answered)

The plan asks and says "prefer the poll unless something forces otherwise". Nothing
forces otherwise.

The telemetry path is fire-and-forget, TX-only, and already heartbeat-paced. TX
completion is observable by reading the owning bit out of the descriptor the driver
wrote — no interrupt needed to know a slot is reusable. Routing `macirq` 78 through the
PLIC buys nothing and costs a PLIC route, a handler, and a new source of
interrupt-context re-entrancy next to a `Mutex` — the exact hazard that produced the
"never emit telemetry from inside `GlobalAlloc`" rule.

> **Design: reclaim descriptors by polling the ownership bit, in the heartbeat, from
> ordinary task context. Set `DMA_CHAN_INTR_ENA` to 0. No PLIC route.**

---

## The register map 🔍

`kernel-devices` gets the numbers; `kernel/` gets the volatile accesses. Everything
here is an offset from the MAC base (`0x1604_0000` for GMAC1).

**MAC core**

| Name | Offset | Notes |
|---|---|---|
| `GMAC_CONFIG` | `0x0000` | `TE` = bit 1, `RE` = bit 0, `DM` (duplex) = 13, `FES` (100M) = 14, `PS` (port select, MII/RMII) = 15, `JD` = 17, `BE` = 18 |
| `GMAC_PACKET_FILTER` | `0x0008` | irrelevant while `RE` is clear |
| `GMAC_MDIO_ADDR` | `0x0200` | |
| `GMAC_MDIO_DATA` | `0x0204` | |
| `GMAC_ADDR_HIGH(0)` | `0x0300` | our source MAC, high 16 bits + enable |
| `GMAC_ADDR_LOW(0)` | `0x0304` | low 32 bits |

**MDIO** — `GMAC_MDIO_ADDR` fields: PA (PHY address) at shift **21**, RDA (register
address) at shift **16**, CSR clock range at shift **8**, GOC (operation) at shift
**2** with `WRITE = 1<<2` and `READ = 3<<2`, `C45E` = bit 1, **`GBUSY` = bit 0**.
Data is the low 16 bits of `GMAC_MDIO_DATA`. Mainline polls `GBUSY` at 100 µs
intervals with a **10 ms** ceiling — that is the bound to transcribe.

The **CSR clock range** field is the MDC divider off the AHB (`pclk`) rate and is the
single most likely constant to be wrong on first try. Getting it too fast is the
classic "MDIO reads plausible garbage" failure.

**DMA** — global at `0x1000`, per-channel block at `0x1100 + chan * 0x80`; we use
channel 0 only.

| Name | Offset | Notes |
|---|---|---|
| `DMA_BUS_MODE` | `0x1000` | `SFT_RESET` = bit 0 — self-clearing; poll it |
| `DMA_SYS_BUS_MODE` | `0x1004` | burst config; DT asks `fixed-burst`, `no-pbl-x8` |
| `DMA_CHAN_CONTROL` | `0x1100` | `PBLX8` = bit 16 (leave clear) |
| `DMA_CHAN_TX_CONTROL` | `0x1104` | `ST` = bit 0, TXPBL in `[21:16]` (DT: 16), `OSP` = bit 4 |
| `DMA_CHAN_TX_BASE_ADDR_HI` | `0x1110` | ring PA, high |
| `DMA_CHAN_TX_BASE_ADDR` | `0x1114` | ring PA, low |
| `DMA_CHAN_TX_END_ADDR` | `0x1120` | **the tail pointer — writing it is the "go" kick** |
| `DMA_CHAN_TX_RING_LEN` | `0x112C` | |
| `DMA_CHAN_INTR_ENA` | `0x1134` | 0 |
| `DMA_CHAN_CUR_TX_DESC` | `0x1144` | diagnostic gold: where the engine actually is |
| `DMA_CHAN_STATUS` | `0x1160` | |

**TX descriptor** — four `u32` words, read format:

| Word | Contents |
|---|---|
| TDES0 | buffer 1 address, **low** 32 bits |
| TDES1 | buffer 1 address, **high** 32 bits |
| TDES2 | buffer 1 size in `[13:0]`; `IOC` = bit 31 (leave clear — we poll) |
| TDES3 | `OWN` = bit 31, `CTXT` = bit 30 (clear), `FD` = bit 29, `LD` = bit 28, frame length in `[14:0]` |

On write-back the MAC replaces TDES3; `ES` (error summary) is bit 15.

One frame = one descriptor, so `FD` and `LD` are both set. No TSO, no checksum
offload (`CIC` in `[17:16]` stays 0) — `kernel-net` already computes the UDP and IPv4
checksums and they are host-tested against golden vectors. Offload would replace
tested code with an untested hardware path, for a payload of a few hundred bytes.

**The 64-bit split matters here.** Board RAM is `0x4000_0000` + 4 GiB, so a PA can
exceed 32 bits in general. TDES0/TDES1 and the `_HI`/`_LO` base-address pair exist for
exactly that. Statics live in the kernel image around `0x4020_0000` and fit in 32
bits — but write both halves anyway rather than leaving a latent trap.

---

## Two traps that will bite, named in advance

**1. `va_to_pa` silently passes heap VAs through unchanged.** This is already written
down for the virtio console — `TX_STAGING` exists precisely because a heap-allocated
buffer's VA handed to a device makes the device DMA the wrong physical memory,
silently. The GMAC has *two* address-bearing structures, not one: the **descriptor
ring itself** and the **packet buffers it points at**. Both must live in
`KERNEL_OFFSET`-range statics.

> **Design: the ring and the TX buffers are `static mut` arrays in `kernel/`, never
> heap allocations.** `N` descriptors × one MTU-sized buffer each, `#[repr(align(64))]`
> on the ring (cache-line, and the descriptor block wants natural alignment anyway).
> This is not a performance choice — it is the only address space `va_to_pa` handles.

**2. Ordering: the ownership bit is written last, and needs a fence.** The device must
never observe `OWN` set over a half-written descriptor. On RISC-V, plain stores give no
such guarantee — a `fence w, w` between "descriptor body" and "set OWN", and another
between "set OWN" and "write the tail pointer", is what makes the sequence real. The
pure ring module can enforce the *order of operations*; only the kernel glue can emit
the fence, so the fence belongs in a commented, tested-by-inspection spot in the glue
and the ring's contract must say so.

---

## Module boundary

Follows the established split exactly: **protocol logic pure and host-tested in
`kernel-devices`, volatile accesses and statics in `kernel/`.**

```
kernel-devices/src/
  syscrg.rs        EXTEND — add GMAC1 clock indices + reset ids, and a
                   gmac1_bringup() -> [Op; N] beside pwmdac_bringup().
                   No new module, no generalisation: AONCRG would be the
                   second consumer that justifies extracting a shared
                   shape, and we are deliberately not building it yet.
  gmac/
    regs.rs        Offsets + bit constants. The table above, as consts.
    mdio.rs        read/write sequencing over a GmacTransport, with the
                   bounded GBUSY poll. Pure.
    ring.rs        TX descriptor ring: descriptor encoding, index advance,
                   wrap, reclaim, TxFull. Pure `core`, no alloc, no MMIO.
    bringup.rs     MAC + DMA init as an ordered Op/sequence: soft reset,
                   bus mode, ring base, ring len, MAC address, TE, ST.
    phy/yt8531.rs  PHY register model: ID constant, BMCR/BMSR, the
                   advertise/restart-autoneg sequence, link/speed decode.

kernel/src/device/gmac.rs
                   The thin unsafe glue: GmacTransport impl over volatile
                   read/write at BASE, the static ring + buffers, va_to_pa,
                   the fences, and `impl NetDevice for Gmac`.
```

`GmacTransport { fn read32(&self, off: usize) -> u32; fn write32(&mut self, off: usize, v: u32); }`
is the seam, matching `MmioTransport` / `PlicTransport` / `FwCfgTransport`.

**The test double does the work a device model would.** A `FakeGmac` in
`kernel-devices`' test module that clears `OWN` on descriptors when the tail pointer
is written, and answers MDIO reads from a table, exercises the ring and the MDIO
sequencer end to end on the host — the same benefit a snemu dwmac device model would
give, at a fraction of the cost, in the idiom the crate already uses
(`FakeVirtioDevice`).

> **On a snemu dwmac model: not before the board works.** It would prove the driver
> matches *our model of* the hardware, which is the same document that produced the
> driver — a wrong instrument agrees with you rather than erroring. Its real value is
> as a deterministic regression guard **after** T4 below has confirmed the model
> against real silicon. Sequenced that way it is cheap and worth it; sequenced before,
> it is confident fiction.

---

## The tracer-bullet ladder

The pure layers are where the *code* is; the board is where the *time* is. Board time
is only tractable if every rung has an oracle that is **independent of the rungs above
it being right** — otherwise every failure looks like silence and bisection is
impossible.

Six rungs. Each is one observable bit. Each names what it proves and what it *cannot*
prove.

| | Rung | Oracle | Proves | Does **not** prove |
|---|---|---|---|---|
| **T0** | **The MAC answers.** After clock ungate + reset release, read a register with a known non-trivial reset value (`GMAC_MDIO_ADDR`'s CSR field, or `DMA_CHAN_TX_RING_LEN`). | UART breadcrumb with the raw word | megapage mapped, clock ungated, reset released | anything about Ethernet |
| **T1** | **The PHY answers.** MDIO read of PHY ID1/ID2 (regs 2/3). YT8531's OUI is a **known constant** — a value that cannot arise from noise. | UART breadcrumb with both words | MDC divider, PHY address, PHY out of reset, MDIO pads | that the PHY is configured |
| **T2** | **Link up.** Advertise 100-full, restart autoneg, poll BMSR link + speed. | UART breadcrumb `link up 100/full` | RGMII pads, PHY config, cable, switch | anything about the DMA engine |
| **T3** | **The DMA engine moves.** Submit **one** descriptor — contents irrelevant, deliberately — set `OWN`, write the tail pointer, poll until the MAC clears `OWN`. | UART breadcrumb `own cleared, tdes3=…`, plus `DMA_CHAN_CUR_TX_DESC` | descriptor PA translation, ring base regs, `ST`, coherency, the fences | that the frame is well-formed |
| **T4** | **Bytes leave the board.** Transmit one **broadcast** frame with a custom ethertype (broadcast so no switch can drop it, custom ethertype so nothing else claims it). | `tcpdump -i … -XX` on the host | `TE`, RGMII TX timing, frame layout, our source MAC | the UDP/IP layer |
| **T5** | **A decodable datagram arrives.** `net=` pointed at the host, one real `Frame` through `UdpFrameSink`. | `cargo xtask collect` decodes it | the whole stack, once | that it survives |
| **T6** | **`net=` sustained.** Heartbeat-paced telemetry into Grafana, minutes not seconds. | Grafana | ring wrap, reclaim under real load, no leak | — |

Three things this ordering buys:

- **T3 separates DMA from Ethernet.** The single most valuable rung, and the one most
  easily skipped by going straight for a real frame. `OWN` clearing means the engine
  read the descriptor *and* fetched the buffer — it validates the entire address-
  translation story with a frame that does not have to be correct. If T3 is folded
  into T4, then "nothing on tcpdump" has a dozen causes instead of three.
- **T4's oracle is a raw capture, not the collector.** At that moment the question is
  "did well-formed bytes leave the board", and involving the decode path adds
  suspects. `kernel-net` is already host-tested; do not put it on trial here.
- **T1 and T2 are separate.** "I can talk to the PHY" and "the PHY has a link" fail for
  disjoint reasons — MDC divider versus RGMII pads — and merging them merges their
  hypothesis sets.

**Everything below T0 is rehearsable on the host.** The clock/reset `Op` sequence, the
MDIO sequencer with its bound, the ring's encode/advance/wrap/reclaim, the PHY register
model — all pure, all TDD'd, all mutation-tested against a `FakeGmac` before the board
is ever powered on. The board should be met with code that is already believed correct,
so that a failure is information about hardware rather than about us.

**Instrument the ladder, don't just run it.** This kernel's whole thesis is that a
system should explain itself; the breadcrumbs above should be `Frame`s over the UART
telemetry channel (M2, shipped), not ad-hoc `println!`. Then the bring-up log is
decodable, diffable between attempts, and the board-bridge can assert on it unattended.

---

## Failure-mode table

Weeks of board time is mostly hypothesis management. This is the lookup that makes it
cheaper — symptom to *first* hypothesis, not the full list.

| Symptom | First hypothesis |
|---|---|
| No UART output at all after the first GMAC touch | Bus hang reading a gated peripheral. Clock not ungated, or the `0x1600_0000` megapage never `insert`ed. |
| Register reads `0x0000_0000` or `0xFFFF_FFFF` | Reset still asserted, or wrong base — check the `PollUntilSet` on reset status actually ran. |
| `GBUSY` never clears | CSR clock range wrong (MDC too fast for the PHY), or the PHY is held in reset. |
| MDIO reads `0xFFFF` | Wrong PHY address, or MDIO pads not routed. `0xFFFF` is the bus floating high. |
| PHY ID correct, no link | Cable/switch, PHY reset GPIO, or autoneg never restarted. |
| Link up, `OWN` never clears | Descriptor PA wrong — **suspect `va_to_pa` on a non-`KERNEL_OFFSET` address first**. Then: ring base not written, `ST` not set, tail pointer not kicked. |
| `OWN` clears, `tcpdump` silent | `TE` not set, or RGMII TX delay/polarity — the `GMAC1_TX_INV` clock. |
| `tcpdump` sees frames with CRC errors | RGMII delay: `rgmii-id` internal delay vs the TX_INV clock selection. The classic. |
| Frames arrive, collector silent | Config, not code — `kernel-net`'s checksums are host-tested. Check the `net=` addresses and the collector's bind. |
| Worked yesterday, not today | A missed `cargo xtask image`, until proven otherwise. |

---

## Rung −1: the probe workload, which runs before everything

Everything above marked 🔍 is desk research. There is a cheap way to promote it to
measured, and it should happen **before the board-bridge and before step 1**, because
one of the things it reads is perishable.

> **`workload=gmac-probe` — a read-only boot that dumps what the board is already
> doing, and stops.** Additive in the existing `workload=` registry, so production
> builds compile none of it. Output is telemetry `Frame`s over the M2 UART channel.

### Why first: U-Boot's configuration is a read-once resource

The board is delivered over **TFTP**. That means U-Boot brought a GMAC and a PHY up,
negotiated a link, and moved megabytes over it, *seconds before `booti`* — on this
exact board, with this exact PHY and cable. A working configuration is sitting in the
register file when the kernel starts.

The design note above guesses at four constants: the MDIO CSR clock-range divider, the
`sys_syscon` `phy_intf_sel` value, the RGMII delay/`TX_INV` clock selection, and
`GMAC_CONFIG`. **U-Boot has already written all four correctly.** Reading them is
strictly better than deriving them, and it collapses three of the four "genuinely still
open" questions into a register dump.

That state does not survive step 5. The moment bring-up asserts a reset, the answer is
gone. So the probe is not merely "nice diagnostics ahead of the work" — **it is the
only opportunity to take the measurement at all**, and it is available now.

(U-Boot may `eth_halt()` after the transfer, which stops the DMA engine. It does not
wipe the clock tree, the syscon field, or the PHY's configuration — and it is the
static configuration, not the running state, that is wanted.)

### What one boot answers

| Probe reads | Closes |
|---|---|
| Live DTB: `dma-noncoherent` on `soc`; `ethernet@1603/1604` nodes | Phase 0's coherency confirmation — the finding the whole estimate rests on |
| **Which** MAC's registers are non-default | Which physical jack is which, *for free* — TFTP already proved one jack works end to end, so the configured node is that jack |
| `GMAC_CONFIG`, MDIO CSR field, `sys_syscon +0x90`, the `TX_INV`/`GTXC` clock regs | Three of the four open questions, as measured values rather than derivations |
| SYSCRG gate + reset-status bits for 97/98/102/106/107 and 66/67 | Whether U-Boot left GMAC1 ungated — and whether step 1's sequence has anything to do |
| Whether the first MMIO read at `0x1604_0000` returns at all | The megapage question, and T0's hang risk, before any driver exists |
| PHY reset GPIO state, if the DTB names one | The remaining open question |

### Verify the instrument

A probe is unreviewed code reading raw memory, and **a wrong base address reads
plausible garbage rather than erroring**. Give it a known-constant oracle, the same
trick T1 uses on the PHY OUI: the dwmac version register (expected `0x110`, *itself
unverified*) reports `DWMAC_CORE_5_20 = 0x52` in bits `[7:0]` 🔍. If that byte reads
`0x52`, the base and the offset are both right and every other number in the dump can
be believed. If it does not, the dump is fiction — and resolving *that* ambiguity is
the first thing the probe is for.

### Constraints

- **Read-only, with one deliberate exception.** No writes. The exception, if taken at
  all, is an MDIO read — which is physically a *write* to `GMAC_MDIO_ADDR` — and it is
  worth taking last, after the register dump is captured, because U-Boot's own working
  MDIO divider makes it nearly free. Order it so a perturbing read cannot contaminate a
  non-perturbing one.
- **Breadcrumb before each read, not after.** The failure mode being guarded is a bus
  hang on a gated peripheral, which produces no output at all. A breadcrumb on the
  near side of each access is what localises the hang to one line.
- **DTB walking is fine post-MMU** — the port already reads timebase from the board's
  DTB. The pre-MMU hazard in CLAUDE.md does not apply here. This also exercises the
  `collect_mmio_regions` path currently parked behind `#[expect(dead_code)]`.
- **It does not need the board-bridge.** One human-attended boot. That decouples it
  from the prerequisite chain entirely, which is why it can go first.

### What it does not buy

It retires **no** driver code and does not shrink steps 1–4, which are pure host logic.
It cannot tell you whether the ring implementation is right. What it does is convert 🔍
into measured, capture a perishable known-good reference, and de-risk T0's hang — for
roughly a day. It is also a tracer bullet for the *development loop itself*: if
`cargo xtask image` → TFTP → breadcrumbs-come-back does not work, that is much cheaper
to discover now than at T0.

### Afterwards

Keep it. "What state is this board in?" is a question the display driver will ask next,
and the board-bridge wants a liveness target that is cheaper than a full boot. Resist
generalising it into a probe *framework* until there is a second consumer — the same
threshold applied to AONCRG above.

---

## Prerequisites, and what they do and don't buy

**The board-agent bridge, both phases, first** — as the plan already argues, and the
argument survives this note intact. Two reasons, and the second is the stronger one:
the loop is flash→reset→read→edit for weeks, and **the thing being debugged is the
network path**, so the diagnostic channel must be a different physical link. Without
that, every failure at every rung looks identical: silence.

The bridge retires **zero** lines of this design. It is tooling, and the sizing assumes
it exists.

**U-Boot's source is worth more than U-Boot's leftover state.** It brings a PHY up for
TFTP seconds before `booti`, so a known-good sequence *for this exact board* exists and
should be read. But assume nothing is inheritable: the display work already found that
inheriting U-Boot's framebuffer was a dead end ([../plans/vf2-display.md](../plans/vf2-display.md)).
Whether the MAC is left usable is worth *measuring* in Phase 0 and worth nothing to
*plan around*.

---

## What this note changes in the plan

- **Phase 0 is mostly done**, at the desk, above. What remains is board-side and
  cheap: confirm `dma-noncoherent` is absent from the live DTB, confirm which physical
  jack is GMAC1, confirm the megapage, and measure whether U-Boot leaves anything.
- **Step 1** gains its indices (SYSCRG 97/98/102/106/107, resets 66/67) and its
  controller — no AONCRG module.
- **Step 2 (pinmux) is very likely empty.** The GMAC pads are not in any pinctrl group
  in the board DTS; what exists instead is the syscon PHY-mode field and the `TX_INV`
  clock selection, both of which belong to step 1. Retire step 2 with a note rather
  than inventing work for it — confirm against the board DTS first.
- **Steps 5–7 become the T0–T6 ladder** above, which is finer-grained (six rungs, not
  three) precisely because that is where the schedule risk is.
- **Open questions 3 and 4 are closed** (poll, no RX ring). Question 1 (which MAC) is
  closed with a better answer than it asked for. Question 5 (bootarg MAC vs hardware
  MAC) stays open but is a one-line decision: **bootarg wins**, matching the existing
  config story; note it as deliberate.
- **A new risk was added and then retired**: DMA coherency. Worth recording that it was
  asked, because "does this need cache maintenance" is the question that decides
  whether a bare-metal NIC is weeks or months, and it was not in the plan.

## Genuinely still open

1. **The CSR clock-range constant** — depends on the AHB (`pclk`) rate, which depends
   on what U-Boot left the clock tree in. Measurable at T1 by trying values; bounded.
2. **YT8531 delay configuration** — `rgmii-id` means the PHY inserts the delay, but the
   VF2 vendor tree carries `motorcomm,*` delay and clock-inversion properties whose
   mainline equivalents are applied by the PHY driver. Which of those this board needs
   is the genuinely unknown-shaped part, and T2→T4 is where it surfaces.
3. **PHY reset GPIO** — not present in the mainline VF2 DTS excerpts read here. Either
   the PHY self-resets adequately, or the vendor tree has it. Resolve against the
   board's own DTB.
4. **Ring depth.** Telemetry is bursty at heartbeat cadence. Start at 8; the number is
   a tuning knob, not a design decision, and `TxFull` is already the trait's contract.

---

## Sources

Desk research is against mainline Linux `master` (August 2026) unless noted:
`arch/riscv/boot/dts/starfive/{jh7110.dtsi, jh7110-common.dtsi,
jh7110-starfive-visionfive-2.dtsi}`, `include/dt-bindings/{clock,reset}/starfive,jh7110-crg.h`,
`drivers/net/ethernet/stmicro/stmmac/{dwmac4.h, dwmac4_dma.h, dwmac4_descs.h,
dwmac4_lib.c, stmmac_mdio.c, dwmac-starfive.c}`, `drivers/reset/starfive/reset-starfive-jh7110.c`.
Board facts (RAM base, revision, DTB inventory) are measured, from
[../notes/visionfive2-first-boot-and-firmware-update.md](../notes/visionfive2-first-boot-and-firmware-update.md)
and the port's own findings.
