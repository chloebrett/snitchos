# M2.5 — Telemetry over the network (design note)

> Status: **design, unbuilt.** This is the M2.5 milestone in
> [../plans/visionfive2-port.md](../plans/visionfive2-port.md): stream the
> `Frame` telemetry to the collector over UDP instead of (or alongside) a
> serial line. It builds on the UART work in
> [uart-telemetry-design.md](uart-telemetry-design.md) — same wire *payload*,
> a different envelope — and it is the transport a real production node would
> actually use. There is **no network code in the tree today**; this is
> greenfield.

## What this is actually about

The port's thesis is that the interesting kernel is hardware-agnostic and the
work is all at the metal boundary. Network telemetry is the sharpest instance
of that. The value proposition — *an observability OS streaming structured
telemetry over the wire, the way real nodes do* — is the strongest milestone in
the whole port. But the cost is lopsided in a way worth stating up front:

- **The packet layer is tiny and shared.** Egress-only UDP with static
  addressing is a few hundred lines of pure buffer-building, host-tested once,
  identical on every target.
- **The NIC drivers are the entire cost, and they share nothing.** QEMU's
  virtio-net, a snemu device model, and the JH7110's DesignWare GMAC have no
  code in common. Two are moderate; the GMAC is the monster the port plan
  already calls "bigger than the rest of the port combined to get the first
  frame out."

So the design's job is to put *all* the reusable value below a single trait, so
that reaching real hardware is a contained driver bring-up behind a proven
interface — not a rewrite. This is the same move `Clock`/`SbiClock` and
`FrameSink` already made for the timer and the frame sink.

## The shape

Three pieces, mirroring the existing hardware-boundary traits:

```
kernel-net/   NEW. no_std, host-buildable, NO deps (pure `core`, no alloc).
              Builds Ethernet II / IPv4 / UDP byte buffers from a static
              NetConfig + a payload slice. Header checksums are pure fns.
              Host-tested against golden byte vectors. Touches no MMIO — the
              same boundary kernel-devices draws (protocol logic, not devices).

trait NetDevice { fn send(&mut self, frame: &[u8]) -> Result<(), TxFull>; }
              The seam. One method: hand it a complete Ethernet frame, it DMAs
              it. Egress-only. Lives in kernel-net (or kernel-obs beside
              FrameSink); impls live in kernel/ because they touch MMIO + DMA.

UdpFrameSink  A `FrameSink` that composes kernel-net's packet builder over a
              `NetDevice`. `emit(frame)` → wire_encode → wrap in UDP/IP/Eth →
              NetDevice::send. Drop-and-count on TxFull, same discipline as
              the UART sink and the alloc/IRQ deferred paths.
```

`UdpFrameSink` is a peer of the UART `UartFrameSink` and of the virtio-console
sink — all three are `FrameSink` impls (`kernel-obs/src/sink.rs`, one method:
`fn emit(&mut self, frame: &Frame<'_>)`). Nothing above the sink changes: the
`Frame` enum, intern table, span registry, pre-init buffer, and batch ring are
all transport-agnostic already.

## Decision 1 — egress-only, static addressing (no ARP / DHCP / ICMP / TCP)

Telemetry is fire-and-forget: the board emits, the collector listens, nothing
flows back. That collapses the stack to the minimum that can put a UDP datagram
on the wire:

- **Ethernet II** — dst MAC, src MAC, ethertype `0x0800`.
- **IPv4** — 20-byte fixed header, one ones-complement header checksum, no
  options, no fragmentation (payload kept under the MTU).
- **UDP** — 8-byte header; checksum is optional over IPv4, so send `0`.
- **Everything static** — our IP, our MAC, the destination IP/MAC/port are all
  configured, not discovered.

The one wrinkle is ARP: to send to the collector you need its MAC. **Dodge it
with a static neighbour entry** — the destination MAC is part of the config.
That keeps the entire first light **TX-only**: no RX ring at the protocol layer,
no ARP state machine, no ICMP, no DHCP timing.

Rejected:

- **DHCP** — needs RX, a state machine, and lease timing to learn an address we
  can just as well assign. A dev board on a known cable has a known IP.
- **ARP** — needs RX and a cache. A static neighbour is one line of config and
  removes the only reason egress would need to receive anything.
- **TCP** — retransmit/ordering/flow-control is the opposite of what
  fire-and-forget telemetry wants; loss is already handled honestly by
  drop-and-count (`Frame::Dropped`), the way an observability system *should*
  lose data.
- **Inbound command path** (messages *to* the board) — a real new surface tied
  to the actor-model / typed-processes direction, and an authority question, not
  a telemetry feature. Explicitly out of scope; see B3 Decision 4's "write path
  is a capability boundary" note.

"TX-only" is a statement about the *protocol layer*. A given NIC driver may
still have to initialise an RX ring the hardware insists on (some GMAC configs
want one for link state); where it does, it drains-and-discards. The point is
that no telemetry logic depends on receiving.

## Decision 2 — the payload is the exact UART wire format; UDP only changes the envelope

The telemetry bytes on a UDP datagram are **identical** to the bytes on the UART
line: COBS-framed postcard `Frame`s, via `protocol::wire_encode` (landing now
for M2/B3). UDP does not get its own encoding.

This is the unifying decision of the whole design. The transport-independent
part — `wire_encode`, the `FrameSink` trait, the collector's `decode_stream`
(already generic over `R: Read`) — is written once for UART and carries over to
UDP untouched. Only the *envelope* differs: UART writes the byte stream
directly onto the wire; UDP carries chunks of that same byte stream inside
datagrams.

**Do we still need COBS inside UDP?** UDP is datagram-framed — each datagram is
delivered whole or not at all, with a length — so the *resync-after-byte-loss*
problem that motivates COBS on a raw serial line **does not exist within a
datagram**. But COBS earns its place for a different reason here: **batching.**

- **One frame per datagram** is simplest (no intra-datagram framing at all) but
  wasteful: ~42 bytes of Ethernet+IP+UDP headers per frame, and at tens of
  thousands of frames/sec that is enormous header overhead plus one DMA
  descriptor per frame.
- **Batch many COBS-framed frames per datagram, up to the MTU.** The datagram
  starts on a frame boundary and is independently decodable, so there is still
  **no cross-datagram resync** — a whole-datagram loss drops a batch and the
  next datagram picks up clean. This is a *stronger* property than the UART
  stream has, obtained by reusing the exact same COBS framing.

So COBS is not redundant on UDP; it is what makes efficient batching free. The
collector decodes a datagram's payload with the same COBS loop it uses for the
serial stream. The packet layer never inspects `Frame` semantics — it moves an
opaque payload slice.

## Decision 3 — one `NetDevice` trait, three unshared impls

The trait is the whole architectural bet: everything above it (packet layer,
sink, collector, tests) is written and proven once; each target supplies a
`send(&[u8])`. The reality below the trait, by target:

| Target | NIC | Reuse below the trait | Relative cost |
|---|---|---|---|
| **QEMU `virt`** | virtio-net (virtio-mmio) | **High** — the whole `kernel-devices::virtio` virtqueue layer (`MmioTransport`, `handshake`, `setup_queue`, `Virtqueue`, `avail_enqueue`, `stage_and_emit`); virtio-console is the working precedent. Adds a TX queue, the 12-byte virtio-net header, net feature negotiation. | small–moderate |
| **snemu** | virtio-net **device model** (host side) | Moderate — snemu already models a virtio device in the `Virtio { in_window, read, write, is_notify, service_tx, tx_output }` shape (`snemu/src/virtio.rs`, dispatched in `bus.rs`). A net device is the same shape with a different queue. | moderate |
| **JH7110 (real HW)** | Synopsys DesignWare **GMAC** (dwmac-5.20) + Motorcomm **YT8531** PHY over MDIO | **None** — bespoke driver: MAC init, TX (and likely RX) **DMA descriptor rings**, **PHY bring-up over MDIO** (board reset GPIO, RGMII clock/delay config in the JH7110 syscon), YT8531 specifics. | **the monster — weeks** |

Two things make the GMAC less alien than a from-scratch NIC would be, worth
recording so the plan doesn't over-fear it:

- The DMA-descriptor discipline is not new to this codebase. SnitchOS already
  learned "devices see physical addresses" (the `TX_STAGING` + `mmu::va_to_pa`
  staging in `virtio_console.rs`) and has the linear-map machinery to hand a
  device a PA. The NIC is the most DMA-heavy device it would have written, not
  the first.
- Egress-only (Decision 1) means the *first* frame out needs only the TX ring
  and a link that's up — not the full RX path.

None of that changes the headline: the GMAC/PHY/MDIO bring-up is where all the
hardware risk and nearly all the time lives. The design's response is to make it
the *last* step, arriving after the trait, packet layer, sink, and collector are
all proven on QEMU + snemu.

### The snemu model, concretely

snemu already exposes `virtio_tx_output()` as "the telemetry frame stream" that
itest decodes. The net device's `service_tx` should do the analogous thing:
extract the UDP **payload** from each transmitted Ethernet frame and expose it
as a byte stream the in-process decoder consumes — so an itest asserts on
decoded `Frame`s exactly as it does today, deterministically, no host sockets in
the gate. (A second, non-gated mode could forward raw datagrams to a real host
UDP socket so the *actual* collector receives snemu's telemetry — useful for the
collector demo, kept out of the deterministic test path.)

## Decision 4 — relationship to the UART sink: peers, not replacements

M2 (B3) puts telemetry on a UART; M2.5 puts it on UDP. These are **not** an
either/or — they are two `FrameSink` impls selected at boot, and UDP reuses B3's
framing/resync work wholesale:

- **What UDP dissolves that UART couldn't:** the throughput ceiling (B3 measured
  ~60 KB/s of telemetry against a 115200 UART's 11.5 KB/s — a 5× overrun forcing
  sampling) and the single-cable console-ownership problem (B3 Decision 4). GbE
  has ~200× headroom; the constraint moves entirely off bandwidth and onto
  driver correctness.
- **What UDP keeps from UART:** the wire payload (Decision 2), the `FrameSink`
  seam, the drop-and-count discipline, the "no wait-on-collector; reset is the
  sync point" model (a board can't block on a host consumer regardless of
  transport), and the collector's decoder.

So M2 is not throwaway on the way to M2.5. The sink is behind a trait and the
framing is transport-independent; UART-now buys real hardware telemetry sooner
and de-risks everything above the wire, and UDP is the bandwidth/ergonomics
upgrade once the GMAC exists.

**Sink selection** is a boot decision, like B3's `console=` mode: the kernel
chooses virtio-console / UART frames / UDP frames based on what the DTB
advertises plus a bootarg override. QEMU defaults to virtio-console (keeps the
gate unchanged); a `net=` bootarg opts into virtio-net; the board picks UART or
GMAC. This rides the same bootarg mechanism that now works in every build.

## Decision 5 — the collector side is nearly free

`decode_stream` is already generic over `R: Read`, and Decision 2 makes a UDP
payload the same bytes as the serial stream. So the collector gains a `--udp
<port>` source alongside the Unix socket and the (M2) serial source:

- Bind a UDP socket; each datagram's payload is one batch of COBS-framed frames,
  decoded by the same COBS loop.
- Decode errors are **non-fatal** on a lossy transport (as for serial): skip to
  the next `0x00`, count a resync, continue. But note a datagram boundary is
  itself a clean resync point, so resyncs should be rarer than on UART.
- A dropped-datagram counter (gaps, if we add a sequence number — see open
  questions) is itself telemetry about the transport.

## Configuration

Static, via a bootarg, reusing the `WorkloadKind`-style parse path that B4/B6
already established for board parameters:

```
net=<our-ip>,<our-mac>,<dst-ip>,<dst-mac>,<dst-port>
```

No discovery, no persistence. On a fixed dev cable or a home router with a
reserved lease, these are known constants. The parse is host-tested in
kernel-boot alongside the existing bootarg arms.

## Throughput — a non-issue, which is the point

The VF2's GMAC is Gigabit; even 100 Mbit is ~12 MB/s. Against B3's measured ~60
KB/s of telemetry that is ~0.5% utilisation. The 5× overrun that forced sampling
on a 115200 UART simply does not appear. This is *why* Ethernet supersedes the
UART cable question: bandwidth stops being a design constraint and the only hard
problem left is driving the NIC correctly.

## Scope — "just enough," stated as exclusions

In: egress UDP telemetry to **one static neighbour**, batched COBS frames,
header checksums, drop-and-count. Out, deliberately:

- inbound / RX command path (actor-model future, an authority boundary)
- TCP, DHCP, ARP, ICMP/ping, IP fragmentation, IPv6
- multiple neighbours / routing
- checksum offload, interrupt coalescing tuning, and other NIC niceties

Each exclusion is a place the design can grow later without reshaping the trait.

## What stays unchanged

- `FrameSink` (`kernel-obs/src/sink.rs`) — `UdpFrameSink` is a new impl.
- The `Frame` enum, `wire_encode`, the intern table, span registry, pre-init
  buffer, batch ring — all transport-agnostic.
- The virtio-console path for QEMU/snemu, so `itest` keeps working. Both
  transports carry the same wire payload (Decision 2), so the gate keeps testing
  the board's reality.
- `kernel-devices::virtio` — the virtqueue layer virtio-net reuses; no change,
  just a second consumer beside virtio-console.

## Open questions

1. **Datagram sequence numbers?** A small monotonic counter in each datagram
   would let the collector *detect* whole-datagram loss (vs. silently missing a
   batch). Cheap and honest, but it is metadata outside the `Frame` stream —
   decide whether it lives in a UDP-payload header or as a `Frame` variant.
2. **MTU and batch flush policy.** How many frames per datagram, and when to
   flush a partial batch (a timer? a byte watermark? every heartbeat?). Trades
   header efficiency against latency-to-collector.
3. **snemu net model fidelity.** Does the gate assert on decoded frames from the
   extracted payload (deterministic, in-process — preferred), or on real
   datagrams over a host socket (realistic, non-deterministic)? Probably the
   former in `itest`, the latter as an opt-in demo.
4. **RX ring on the GMAC.** Does the DesignWare MAC config we need bring the
   link up TX-only, or does it force a minimal RX ring for link state? Measured
   during bring-up, not assumed.
5. **PHY bring-up specifics.** YT8531 reset GPIO, RGMII clock/delay in the
   JH7110 syscon, MDIO addressing — the classic bare-metal NIC time sink, and
   the one genuinely unknown-shaped part of the whole design.
6. **Sink selection UX.** Bootarg-only, or DTB-driven auto-select with a
   bootarg override? (Mirrors B3's `console=` question.)

## Increment sketch (for turning into a plan)

Host-first, same discipline as the rest of the port — everything provable under
snemu is proven before the board, because a board round-trip is expensive.

1. **`kernel-net` crate**: pure Ethernet/IPv4/UDP builders + checksums, host-
   tested against golden byte vectors. `NetConfig`, `NetDevice` trait. No device.
2. **`UdpFrameSink`**: composes kernel-net over a `NetDevice`; a `FrameSink`.
   Host-tested against a mock `NetDevice` capturing sent frames.
3. **QEMU virtio-net driver**: `NetDevice` impl over `kernel-devices::virtio`.
   Real UDP telemetry off QEMU into a `nc -u` / collector.
4. **snemu virtio-net model**: the `Virtio`-shaped device; extract payload →
   in-process decoder. Lands the whole IP path in the deterministic `itest` gate.
5. **Collector `--udp` source**: bind a socket, decode datagram payloads, non-
   fatal errors + resync/loss counters → Grafana over the network from snemu/QEMU.
6. **JH7110 GMAC driver**: the `NetDevice` impl for real hardware — MAC init, TX
   DMA ring, PHY/MDIO bring-up. The only step with real hardware risk, arriving
   with everything above the wire already proven.

Steps 1–5 land network telemetry on snemu + QEMU with the gate green. Step 6 is
its own multi-week sub-project — but by then it is *just a driver* behind a
proven trait.
