# Plan: Network telemetry (M2.5 — UDP frame transport)

**Status**: 🟡 **PRs 1–7 SHIPPED and gate-green; PR 8 (the JH7110 GMAC driver) is all
that remains — and it is its own project, not a step here.**
**Design**: [docs/network-telemetry-design.md](../docs/network-telemetry-design.md)
**Milestone**: M2.5 in [plans/visionfive2-port.md](visionfive2-port.md)
**Write-up**: [post 70 — the wire was never load-bearing](../posts/post-70-the-wire-was-never-load-bearing.md)

Verified in-tree 2026-08-06: the `kernel-net` crate exists, the `net-telemetry-over-udp`
scenario is registered (`xtask-itest/src/itest.rs:201`) and passes under snemu, and the
collector carries a `--udp` source (`collector/src/main.rs:74`). The whole chain is live:
`net=` parse → `UdpBatcher` → virtio-net TX → snemu strips 54 header bytes → decoder
reads Hello, boot and heartbeat off UDP.

**Also deferred, deliberately** (from post 70's close): QEMU `--engine qemu` parity
(needs `-device virtio-net-device` + a host UDP capture), heartbeat-driven batching
(today it is one datagram per frame — correct but wasteful, since telemetry trickles
below the MTU so batch-overflow never flushes), and the `virtio_mmio` DEDUP —
`virtio_net.rs`'s `read_reg`/`write_reg`/`transmit` mirror the console driver and can't
be shared while each device owns its own `static mut` queues.

**The inbound (RX) path stays out of scope** — but it is now *scoped* rather than
one-line-deferred: see the addendum in
[docs/network-telemetry-design.md](../docs/network-telemetry-design.md) for the gap
list, including the fact that console *output* over UDP already works today via
`net=… console=frames` (no new code) and that only the inbound half is missing.

## Goal

Stream the `Frame` telemetry to the collector over UDP, proven end-to-end on
snemu + QEMU with the gate green, leaving the JH7110 GMAC driver as the only
remaining hardware step.

## Design decisions this plan implements

From the design note (read it first):

1. **Egress-only, static addressing** — no ARP/DHCP/ICMP/TCP/RX.
2. **Payload is the exact UART wire format** — COBS-framed `Frame`s via
   `protocol::wire_encode`; UDP only changes the envelope, and COBS is what
   makes MTU batching free.
3. **One `NetDevice` trait, unshared impls** — all reusable value above the trait.
4. **UART and UDP sinks are peers** — both `FrameSink` impls, boot-selected.
5. **Collector `--udp` source is nearly free** — `decode_stream` is generic
   over `R: Read`.

## Prerequisite

The COBS wire format (`protocol::wire_encode`, M2/B3 Step 1) must be landed —
this plan reuses it verbatim. It is already in flight (the capturing sink uses
it); confirm it is committed before starting PR 4.

## Acceptance Criteria

All eight met by PRs 1–7. The last three are jointly evidenced by the
`net-telemetry-over-udp` scenario, which is in the deterministic gate and passes plain
and under `--scramble`.

- [x] `kernel-net` builds a valid Ethernet II / IPv4 / UDP datagram from a
      static config + payload, host-tested against golden bytes, checksums correct.
- [x] `UdpFrameSink` encodes frames (COBS), batches them up to an MTU, and hands
      complete datagrams to a `NetDevice`; drops-and-counts when the device is full.
- [x] A `net=<our-ip>,<our-mac>,<dst-ip>,<dst-mac>,<dst-port>` bootarg parses
      (host-tested in kernel-boot), and an invalid one is refused, not silently ignored.
- [x] The virtio-net TX path (queue setup, virtio-net header, feature
      negotiation) is host-tested in kernel-devices over the `MmioTransport` mock.
- [x] Booting QEMU/snemu with `net=…` emits telemetry as UDP datagrams whose
      payload decodes to the same `Frame`s the virtio-console path produces.
- [x] A deterministic `itest` scenario boots under snemu with `net=…` and asserts
      decoded frames arrive over the modelled NIC — the whole IP path in the gate.
- [x] The collector's `--udp <port>` source decodes datagram payloads, treats
      decode errors as non-fatal (resync + count), and exports a resync/loss counter.
- [x] The QEMU/snemu virtio-console path and the full test gate are unchanged
      (`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`).

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test. Host tests run with `cargo nextest run` (never plain
`cargo test`); mutation via `cargo xtask mutants`; integration via
`cargo xtask itest`. Present acceptance criteria and get confirmation **before
writing any code for each step.**

PRs 1–4 and 7 are pure/host-testable and deterministic. PRs 5–6 are the emulator
integration. PR 8 is hardware and gets its own sub-plan.

---

### PR 1 — `kernel-net` crate: the pure packet layer

New crate `kernel-net/` (no_std, host-buildable, NO deps, pure `core`, no alloc —
same boundary as kernel-devices prod code). `[lib] doctest = false`. Opts into the
workspace pedantic lints.

**Acceptance criteria**: given a `NetConfig` (our IP/MAC, dst IP/MAC/port) and a
payload slice, `build_udp_datagram` writes a byte-exact Ethernet II frame:
correct MACs + ethertype `0x0800`, a 20-byte IPv4 header with a correct
ones-complement header checksum and total-length, an 8-byte UDP header with
correct length and checksum `0`, then the payload. Buffer-too-small returns
`Err`, never panics or truncates.

**RED**: A test with a fixed `NetConfig` and a known payload asserting the full
datagram against a hand-computed golden byte vector (verifiable with `xxd`/a
reference like Python's `struct`). Plus a focused test for `ip_checksum` on a
known header (RFC 1071 worked example) and a `buffer too small → Err` test.
**GREEN**: `ip_checksum` (pure fold), `build_udp_datagram` writing headers
big-endian into `&mut [u8]` and returning the written prefix.
**MUTATE**: `cargo xtask mutants -p kernel-net`. Checksum/length arithmetic and
the endianness writes are the mutation targets.
**KILL MUTANTS**: A flipped byte-order or off-by-one in a length field must fail
a test — add per-field assertions if a survivor shows a gap.
**REFACTOR**: Consider a typed `NetConfig` builder vs positional; keep it flat if
that reads clearer.
**Done when**: golden datagram + checksum + error tests pass, mutation report
reviewed, gate green, human approves.

*Also lands in this PR (trivially): the `NetDevice` trait (`fn send(&mut self,
frame: &[u8]) -> Result<(), TxFull>`) and a `TxFull` marker — no impl yet, so no
test beyond "it compiles"; it exists for PR 2 to depend on.*

---

### PR 2 — `UdpFrameSink`: batching FrameSink over a NetDevice

A `FrameSink` (`kernel-obs::sink::FrameSink`) that COBS-encodes each frame
(`protocol::wire_encode`), accumulates encoded frames into an MTU-bounded batch,
wraps a full batch with `kernel-net::build_udp_datagram`, and calls
`NetDevice::send`. Lives where it can see both traits (kernel-obs, or a thin
kernel-net consumer — decide during CONFIRM).

**Acceptance criteria**:
- Emitting frames whose COBS encodings fit one MTU produces exactly one datagram
  containing all of them, and its payload decodes (COBS) back to those frames in
  order.
- A frame that would overflow the current batch flushes the batch first, then
  starts a new one (no frame split across datagrams).
- When `NetDevice::send` returns `TxFull`, the sink drops the batch and
  increments a drop counter (drop-and-count, exposed for a later `Frame::Dropped`).

**RED**: Tests against a mock `NetDevice` capturing sent datagrams: (a) three
small frames → one datagram → payload decodes to the three frames; (b) frames
sized to force a flush → two datagrams, boundary on a frame edge; (c) mock
returning `TxFull` → drop counter increments, no panic.
**GREEN**: The batching buffer + flush logic + drop counter.
**MUTATE**: `cargo xtask mutants` on the sink. The flush-threshold comparison and
the drop-counter increment are the targets.
**KILL MUTANTS**: A mutated `>=`→`>` on the MTU threshold must break the
flush-boundary test.
**REFACTOR**: Batch-flush policy (byte watermark now; a time/heartbeat flush is
an open question — leave a seam, don't build it yet).
**Done when**: the three behaviours are tested, decode round-trip holds, mutation
reviewed, gate green, approved.

---

### PR 3 — `net=` bootarg parse

Add the `net=` bootarg to `kernel_boot::bootargs` (host-tested), producing a
`NetConfig` (or a `NetBoot` describing it). Refusal path, not silent-ignore.

**Acceptance criteria**: `net=10.0.0.2,52:54:00:12:34:56,10.0.0.1,aa:bb:cc:dd:ee:ff,9000`
parses to the exact addresses; a malformed field (bad octet, short MAC, missing
comma) returns a parse error the caller can snitch on; absence of `net=` leaves
the default transport unchanged.

**RED**: A table of `(input, expected)` cases including several malformed inputs
asserting `Err`, and one asserting absence → `None`.
**GREEN**: The parse arm reusing the existing bootarg-splitting helpers.
**MUTATE**: `cargo xtask mutants -p kernel-boot`. The octet/field boundaries are
the targets.
**KILL MUTANTS**: A mutant accepting a 5-byte MAC must fail a test.
**REFACTOR**: Share IP/MAC parsing helpers if kernel-net wants them too.
**Done when**: parse table + refusal tests pass, mutation reviewed, gate green,
approved.

---

### PR 4 — virtio-net TX logic in kernel-devices

The device-*protocol* half of the QEMU NIC, in `kernel-devices` over the existing
`MmioTransport` mock — no MMIO, no emulator, fully deterministic. Reuses
`kernel_devices::virtio` (`handshake`, `setup_queue`, `Virtqueue`,
`avail_enqueue`, `stage_and_emit`). Adds: the TX virtqueue, the 12-byte
virtio-net header prepended to each frame, and net-specific feature negotiation.

**Acceptance criteria**:
- Feature negotiation accepts a virtio-net device advertising the required
  features and rejects one missing a mandatory feature.
- Handing the TX path an Ethernet frame enqueues a descriptor whose buffer is
  `[virtio_net_hdr(12 bytes, zeroed) || frame]`, and advances the avail ring by one.
- A second send advances the ring correctly (no index/wrap bug).

**RED**: Tests over the `MmioTransport` mock asserting the descriptor contents
(header + frame), the avail-index advance, and the feature accept/reject cases —
the same style as the virtio-console tests.
**GREEN**: `VirtioNetTx` (or extend the virtio module) implementing the above.
**MUTATE**: `cargo xtask mutants -p kernel-devices`. The header length, ring
advance, and feature mask are targets.
**KILL MUTANTS**: A mutated header length (12→11) or a dropped ring increment
must fail a test.
**REFACTOR**: Factor anything genuinely shared with virtio-console back into the
common virtio module; don't over-abstract two consumers.
**Done when**: descriptor + ring + feature tests pass, mutation reviewed, gate
green, approved.

---

### PR 5 — kernel-side virtio-net device + sink selection

The MMIO/DMA glue in `kernel/src/device/virtio_net.rs`: binds PR 4's logic to
real MMIO discovery + `TX_STAGING`/`mmu::va_to_pa` staging (devices see physical
addresses — grep the four existing virtio-console staging sites), implements
`NetDevice::send`, and wires boot-time sink selection so `net=…` installs a
`UdpFrameSink` instead of the virtio-console sink. This is kernel glue (statics +
MMIO), so it is **not** host-testable — its coverage is the PR 6 itest.

**Acceptance criteria** (verified via PR 6's scenario, developed together):
- With `net=…`, the kernel installs the UDP sink and telemetry stops going to
  virtio-console.
- Without `net=…`, behaviour is byte-identical to today (virtio-console default).
- No frame carries a heap/higher-half VA into a descriptor (staging is applied —
  the va_to_pa discipline).

**RED/GREEN**: No host test at this layer; the failing test is PR 6's itest
scenario (write it first, watch it fail against a kernel with no UDP sink).
**MUTATE**: N/A at the kernel layer (excluded from mutants per repo policy);
effectiveness comes from the itest.
**REFACTOR**: Keep the selection dispatch beside the existing sink construction;
mirror the `console=` mode shape from B3 if it has landed.
**Done when**: PR 6's scenario passes with `net=…` and the default path is
unchanged, gate green, approved. *(May be committed together with PR 6.)*

---

### PR 6 — snemu virtio-net model + deterministic itest

The host-side NIC model in `snemu` in the existing `Virtio`-shaped seam
(`in_window`/`read`/`write`/`is_notify`/`service_tx`/`tx_output`, dispatched in
`bus.rs`). `service_tx` extracts the UDP **payload** from each transmitted
Ethernet frame and exposes it as a byte stream (parallel to the existing
`virtio_tx_output()` telemetry stream) so the in-process decoder consumes it
deterministically — no host sockets in the gate.

**Acceptance criteria**: a new `itest` scenario (`net-telemetry-over-udp`, in
`xtask/src/itest/scenarios.rs`, registered in `SCENARIOS`) boots the kernel under
snemu with `-append net=…`, reads the modelled NIC's extracted payload, and
asserts the decoded `Frame`s include the boot sequence (`Hello` → `kernel.boot`
SpanStart → a `kernel.heartbeat`), matching what virtio-console produces.
Deterministic → one run is the gate; also passes under `--scramble`.

**RED**: The scenario, failing (kernel has no NIC model / no UDP sink yet).
**GREEN**: The snemu net device + the header-stripping extractor; PR 5's kernel
side makes it pass.
**MUTATE**: `cargo xtask mutants` on the host-side extractor (payload offset,
length). The itest itself is the integration oracle.
**KILL MUTANTS**: A mutated payload offset must break the decode.
**REFACTOR**: Share Ethernet/IP/UDP field offsets with `kernel-net` if that
avoids a second copy of the header layout (single source of truth for offsets).
**Done when**: `cargo xtask itest net-telemetry-over-udp` green plain and
scrambled, full gate green, mutation reviewed, approved.

---

### PR 7 — collector `--udp` source

A `--udp <port>` source in `collector/` alongside the Unix-socket source. Binds a
UDP socket; each datagram's payload is one COBS batch decoded by the existing
COBS-aware decoder. Decode errors are **non-fatal**: skip to the next `0x00`,
increment a resync counter, continue (contrast the socket path's fail-fast). A
whole-datagram boundary is itself a clean resync point.

**Acceptance criteria**:
- A datagram carrying a valid COBS batch decodes to the expected `Frame`s.
- A datagram with leading garbage before the first `0x00` decodes the frames
  after it and counts one resync (does not exit).
- A `Resyncs`/dropped counter is exposed as telemetry about the transport.

**RED**: Tests feeding crafted datagram payloads (clean; garbage-prefixed;
truncated) to the decode routine, asserting decoded frames + resync count. A
real-socket smoke (`std::net::UdpSocket` loopback) proves the source wiring.
**GREEN**: The UDP source + non-fatal decode policy (make the error policy
explicit in the type, per the design, not a buried flag).
**MUTATE**: `cargo xtask mutants -p collector`. The resync-skip loop and counter
are targets.
**KILL MUTANTS**: A mutant that swallows a frame after resync must fail.
**REFACTOR**: Unify with the (M2) serial source if that also wants non-fatal
decode — likely the same policy type.
**Done when**: decode + resync + loopback tests pass, mutation reviewed, gate
green, approved. **At this point network telemetry works end-to-end on snemu +
QEMU into the collector/Grafana.**

---

### PR 8 — JH7110 GMAC driver (own sub-plan)

The `NetDevice` impl for real hardware: DesignWare GMAC (dwmac-5.20) MAC init,
TX DMA descriptor ring, PHY bring-up over MDIO (board reset GPIO, RGMII
clock/delay in the JH7110 syscon), YT8531 specifics. Multi-week; the only step
with real hardware risk. **Everything above the `NetDevice` trait is already
proven by PRs 1–7, so this is a driver swap, not a rewrite.**

This does **not** get TDD-decomposed here — it is its own project. **That plan now
exists: [vf2-gmac-driver.md](vf2-gmac-driver.md)**, scoped against the design note's
open questions (RX ring necessity, PHY specifics) plus the JH7110 TRM. It opens with a
reconnaissance phase rather than steps, for the same reason this section stayed a
placeholder: per the planning skill, don't write steps you can't yet size.

---

## Pre-PR Quality Gate

Before each PR:
1. Mutation testing — `cargo xtask mutants -p <crate>` (kernel/ excluded; its
   coverage is the itest).
2. Refactoring assessment — run the `refactoring` skill.
3. `cargo xtask clippy` (whole workspace, host + riscv) and
   `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble` green.
4. `cargo xtask links` after any doc `git mv`.

## Notes

- **Do not regress the virtio-console gate.** Both transports carry the same wire
  payload; the default (no `net=`) path stays byte-identical.
- **`kernel-net` has NO deps and no alloc** — it is pure `core`, matching
  kernel-devices prod code, so it stays host-buildable and mutation-testable.
- **The GMAC is the whole cost.** PRs 1–7 are ~1–1.5 weeks of mostly-pure,
  gate-green work; PR 8 is the multi-week hardware project. Do not let PR 8's
  size distort the ordering — land the proven stack first.

---
*On completion, `git mv` this file to `plans/legacy/` (per CLAUDE.md override of
the planning skill's delete step) and merge any learnings via the `learn`/`adr`
agents.*
