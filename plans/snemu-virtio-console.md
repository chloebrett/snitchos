# snemu virtio-console device model

## Goal

Teach snemu enough virtio-mmio + virtio-console to receive the kernel's
telemetry frames, so the existing integration suite can run against snemu and be
diffed against QEMU — a differential oracle for free (no new tests, no
toolchain). This is the post's milestone-2 "snemu becomes useful" step.

## Where we are

The full kernel runs ~1.72M steps, prints its DTB summary, then halts at
`Bus(OutOfRange 0x1000_8000)` — `find_console_base` (`kernel/src/device/
virtio_console.rs`) walks the DTB's 8 `virtio,mmio` slots (`0x1000_1000` ..
`0x1000_8000`, enumerated highest-first) and reads `REG_MAGIC_VALUE` on each.
snemu's bus only decodes the ns16550a UART, so the first probe faults.

The device contract snemu must satisfy already exists, host-tested, in
`kernel-core/src/virtio.rs`: register offsets, status state machine, feature
negotiation (`VIRTIO_F_VERSION_1` only), `setup_queue` register order, the
`#[repr(C)]` virtqueue structs, and crucially a `FakeVirtioDevice` whose
register behaviour is *exactly* the device model snemu needs. snemu's job is to
be that fake, in the bus, against real DMA memory.

QEMU places the virtio-console at the highest slot, **`0x1000_8000`** (slot
index 7); the other 7 slots are empty (present, `DeviceID == 0`).

## Layers

### Layer 1 — MMIO register file + probe/handshake  ← THIS STEP

A `Virtio` device in snemu's bus that answers the discovery reads and drives the
full feature + queue-config handshake to completion (mirrors `FakeVirtioDevice`).

- Decode the virtio-mmio window `0x1000_1000 .. 0x1000_9000` on the bus
  (slot = `(addr - base) / 0x1000`, reg offset = `(addr - base) % 0x1000`),
  32-bit accesses.
- **Console slot (0x1000_8000):** serve `MAGIC` / `VERSION (2)` /
  `DEVICE_ID (3)`; `QUEUE_NUM_MAX >= QSIZE`; `DEVICE_FEATURES` = `F_VERSION_1`
  (low half 0, high half bit 0) gated by `DEVICE_FEATURES_SEL`; a `STATUS`
  register with the `FEATURES_OK` acceptance rule; store the queue-config writes
  (`QUEUE_SEL` / `QUEUE_NUM` / `DESC|DRIVER|DEVICE` addr pairs / `QUEUE_READY`)
  for layer 2. `QUEUE_NOTIFY` accepted and ignored.
- **Empty slots:** `MAGIC` / `VERSION` / `DEVICE_ID = 0`, so the probe skips them.

Expected new stopping point: handshake completes, `DRIVER_OK` is set, then the
kernel submits its first TX frame and **spins polling `used.idx`** (which never
advances without layer 2) — an infinite poll, i.e. snemu runs to the step limit
rather than faulting. That hang is the layer-2 trigger.

### Layer 2 — virtqueue / descriptor ring  ✅ SHIPPED (2026-06-28)

On a `QUEUE_NOTIFY` write for the TX queue, walk the queue the driver
configured: read `avail.idx` / `avail.ring` from guest RAM (the device reads DMA
memory directly through the bus), follow each descriptor chain (`addr`/`len`/
`next`) to pull out the transmitted bytes, then advance `used.idx` / write a
`used.ring` entry so the driver's poll (`used_advanced`) completes. The ring
addresses are PAs the driver wrote during layer 1.

Implemented in `snemu/src/virtio.rs` (`service_tx` + `drain_chain`, chain walk
bounded by `qsize`); the bus calls `service_tx(&mut ram)` when `is_notify(addr)`.
Captured bytes accumulate in `Virtio::output`, exposed via `Cpu::virtio_tx_output`
and reported by `main`. Result: the kernel boots past the TX hang to its
heartbeat and transmits **2102 telemetry bytes**; new stop is `stimecmp` (CSR
`0x14d`, the sstc timer — `Clock::arm`), a separate timer/CLINT milestone.

### Layer 3 — output sink + differential oracle

The bytes pulled off the TX queue are postcard-encoded `protocol::Frame`s. Emit
them where the harness can read them (stdout, or a socket the collector reads),
then add an `--snemu` mode to `xtask itest` that boots scenarios under snemu and
diffs the decoded frame sequence against QEMU.

**Output sink — ✅ SHIPPED (2026-06-28).** `snemu/src/main.rs` decodes
`Cpu::virtio_tx_output()` through `protocol::stream::decode_stream` and reports
the frame count (`--frames` dumps each). The emulator core stays protocol-free;
only the binary depends on `protocol` (feature `std`). `cargo xtask snemu-boot
--frames` surfaces it. **Validated:** the captured 2102 bytes decode into **112
real telemetry frames** — `Hello{protocol_version: 4}`, the `kernel.boot` /
`console_init` / `telemetry_init` span tree, `Dropped{0}`, and the full metric
registry — byte-perfectly. This conclusively confirms layers 1+2.

**Differential oracle — ✅ SHIPPED (2026-07-04).** `cargo xtask snemu-diff`
(`xtask/src/snemu_diff.rs`) boots the *same* default (`init`) kernel under both
snemu (in-process — xtask depends on the `snemu` lib) and QEMU (minimal spawn,
collect for a wall-clock window), then **structurally** diffs the frame streams:
`canonical()` zeroes the volatile fields (timestamps everywhere, metric values)
so snemu's deterministic clock and QEMU's cycles compare equal; `diff_streams`
reports the boot-prefix agreement + first divergence; `string_vocabulary`
compares the registered-name sets (order- and run-length-robust). Verdict: PASS
unless snemu emits a name QEMU never did.

Rather than adapt the 95 QEMU scenarios (which read a live socket and mostly
target `workload=demo`, while snemu boots `init` with no bootarg support), this
compares the two emulators directly on the same boot — simpler and it's a true
oracle. Result: **135-frame structural boot-prefix match, 83/83 vocabulary
agreement** (the extra QEMU-only names are FS/userspace behavior snemu hadn't
reached in its step budget). The first sequence divergence is a benign cross-hart
*registration ordering* difference (SC round-robin vs real-parallel) — the
expected limit of a sequence diff under concurrency; the vocabulary check sees
past it. **Follow-ups:** per-scenario replay via a snemu-backed `View` (needs a
`workload=` bootarg for snemu — patch `/chosen/bootargs` in the DTB), and a
multiset/registration-set diff to quantify agreement past the ordering point.

## Testing strategy

- Layer 1/2 device logic: unit tests in the new `snemu/src/virtio.rs` (register
  semantics, status acceptance rule, ring walk over a hand-built queue in
  `Memory`).
- Integration: the meta-loop itself — run the real kernel under snemu after each
  layer and confirm it advances to the predicted next boundary.

## Non-goals (for now)

RX path (kernel never receives in v0.1), the `MULTIPORT`/`SIZE` console
features, interrupts (the driver polls), and modelling more than one live
device.
