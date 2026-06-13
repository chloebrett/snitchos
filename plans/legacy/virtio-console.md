# virtio-console driver (v0.1 telemetry channel)

The kernel's second serial channel, dedicated to binary telemetry frames.
Separate from the existing NS16550A which is reserved for `println!` /
panic text.

## Why virtio-console (not a second NS16550)

Confirmed empirically: QEMU's `virt` machine has exactly one NS16550 baked
in at `0x10000000`. Additional `-serial` flags route through virtio-console
under the hood, not into a second 16550 slot. We have to build the
virtio-console driver to get a real second channel.

## Big-picture step list

| # | step | size |
|---|---|---|
| 1 | xtask: attach virtio-console + UDS chardev to QEMU | ~5 lines |
| 2 | Verify the DTB now shows the populated slot | sanity-check |
| 3 | Understand virtio-mmio register layout (this doc) | reading |
| 4 | Walk DTB for `virtio,mmio` slots; probe each for DeviceID 3 | ~30 lines |
| 5 | Drive the handshake: reset → ACK → DRIVER → features → FEATURES_OK → DRIVER_OK | ~50 lines |
| 6 | Allocate the virtqueue (descriptor table + available ring + used ring) | ~80 lines — hairiest |
| 7 | Transmit path: encode a frame, add a descriptor, notify, wait | ~30 lines |
| 8 | Wire it into kmain so it sends a `Hello` frame at boot | a few lines |
| 9 | host-reader: read raw bytes from the socket, decode `Frame`s, pretty-print | new bin, ~50 lines |

(6) is the only really hairy chunk. Rest is small steps.

## What virtio is, conceptually

- **Para-virtualized device interface.** "Para" = "alongside" — the guest
  knows it's virtualized and cooperates with the hypervisor.
- Compare: full virtualization emulates hardware (slow; every register
  access traps). Paravirtualization (virtio) shares memory for data and
  traps only for "poke" notifications. Fast.
- Standardized in the 2000s to stop hypervisors building incompatible
  para-virt interfaces. Now every hypervisor speaks it.

## Shape of every virtio device

- **Control registers** — identify device, negotiate features, signal status.
- **One or more virtqueues** — shared-memory ring buffers for bulk data.
  Each virtqueue has three regions:
  - **Descriptor table** — array of `{address, length, flags, next}`.
  - **Available ring** — guest's "process these descriptors" list.
  - **Used ring** — host's "I finished these descriptors" list.
- **Notify mechanism** — guest writes a magic register to poke the device;
  device raises an interrupt back.

Pattern: shared memory for data, traps only for poke. The same pattern
SnitchOS IPC will adopt.

## Transports

- **virtio-mmio** — memory-mapped registers. What QEMU `virt` uses.
  Discovered via DTB. **Our target.**
- **virtio-pci** — PCI bus. What QEMU `pc` uses, what real cloud VMs use.
- **virtio-ccw** — IBM s390. Don't care.

## virtio-mmio register layout

At each slot's MMIO base (the `virtio_mmio@10001000` ... `10008000` nodes
in the DTB):

| offset | name | meaning |
|---|---|---|
| `0x000` | MagicValue | 4 bytes; reads `"virt"` (`0x74726976` LE). If not, ignore. |
| `0x004` | Version | `2` for the modern spec. Legacy `1` exists; we won't support it. |
| `0x008` | DeviceID | `0` = empty; `3` = console; others (1=net, 2=block, 18=input). |
| `0x00C` | VendorID | Usually `0x554D4551` ("QEMU"). |
| `0x010` | DeviceFeatures | What the device offers (read after writing DeviceFeaturesSel). |
| `0x014` | DeviceFeaturesSel | Select feature-bit page (0 = bits 0-31, 1 = bits 32-63). |
| `0x020` | DriverFeatures | What we accept (write after writing DriverFeaturesSel). |
| `0x024` | DriverFeaturesSel | Same paging. |
| `0x030` | QueueSel | Select which virtqueue we're configuring. |
| `0x034` | QueueNumMax | Max queue size (read-only). |
| `0x038` | QueueNum | Our chosen queue size (write). |
| `0x044` | QueueReady | `1` = queue is live. |
| `0x050` | QueueNotify | Write the queue index here to tell the device "go look." |
| `0x060` | InterruptStatus | Why we got an interrupt (we'll poll for v0.1). |
| `0x064` | InterruptACK | Acknowledge an interrupt. |
| `0x070` | Status | The device-state machine. |
| `0x080` / `0x084` | QueueDescLow / High | Physical address of the descriptor table for the selected queue. |
| `0x090` / `0x094` | QueueDriverLow / High | Physical address of the available ring. |
| `0x0A0` / `0x0A4` | QueueDeviceLow / High | Physical address of the used ring. |

Notice the pattern: "select which queue/feature page first, then
read/write the value." Saves register space; common MMIO idiom.

## Status field bits

The device's state-machine, OR-ed in step by step:

| bit | name | meaning |
|---|---|---|
| `0x01` | ACKNOWLEDGE | Driver has noticed the device. |
| `0x02` | DRIVER | Driver knows how to drive this device. |
| `0x04` | DRIVER_OK | Driver is fully set up and ready. |
| `0x08` | FEATURES_OK | Driver has accepted features it can support. |
| `0x40` | DEVICE_NEEDS_RESET | (read-only) Device wants to be reset. |
| `0x80` | FAILED | Driver has given up. |

Handshake = OR these bits into Status one by one, verifying each step.

## Step 1: xtask attaches virtio-console to QEMU

Add these flags to the `qemu-system-riscv64` invocation in
`xtask/src/main.rs`:

```
-chardev socket,path=/tmp/snitch-telemetry.sock,server=on,wait=off,id=telemetry
-device virtio-serial-device
-device virtconsole,chardev=telemetry
```

What each does:

- `-chardev socket,...` — declares a host-side endpoint: a Unix domain
  socket at `/tmp/snitch-telemetry.sock`, server mode, no waiting for a
  client before boot. Identified as `telemetry`.
- `-device virtio-serial-device` — attach a virtio-serial controller to
  the guest (the parent device for virtconsole ports).
- `-device virtconsole,chardev=telemetry` — attach a virtio-console port
  to that controller, pointed at our chardev.

After this, dump the DTB with `-machine dumpdtb=virt.dtb` and look for a
populated `virtio_mmio@1000X000` slot (i.e., one whose runtime read of
DeviceID at offset `0x008` will return `3`).

## Step 6 sketch: virtqueue layout

A virtqueue lives in guest physical memory. For v0.1 (no MMU, no
allocator) we use **static arrays** sized at compile time. Three regions:

### Descriptor table

`N` descriptors, each:

```rust
#[repr(C, align(16))]
struct Descriptor {
    addr: u64,    // guest physical address of buffer
    len: u32,     // buffer length in bytes
    flags: u16,   // 0=NEXT, 1=WRITE, ...
    next: u16,    // index of next descriptor if chained
}
```

### Available ring

What the *driver* puts there:

```rust
#[repr(C, align(2))]
struct AvailableRing<const N: usize> {
    flags: u16,
    idx: u16,         // monotonically increasing
    ring: [u16; N],   // descriptor indices
    used_event: u16,  // optional, with EVENT_IDX
}
```

### Used ring

What the *device* puts there:

```rust
#[repr(C, align(4))]
struct UsedRing<const N: usize> {
    flags: u16,
    idx: u16,
    ring: [UsedElement; N],
    avail_event: u16,
}
struct UsedElement { id: u32, len: u32 }
```

### v0.1 sizing

- `N` = 8 (small but plenty for one outgoing frame at a time)
- Static allocation; no heap needed
- Synchronous TX: encode → add descriptor → notify → spin on used ring
  index → return

Per the v0.1 plan, the wire format is the contract; the consumer is
cheap to evolve. Same applies here — the virtqueue mechanics are
boilerplate we'll write once and not touch again until v0.5 or so.

## Open questions / deferred

- **Receive path.** v0.1 is TX-only. RX would be needed once we accept
  commands over telemetry (e.g., remote-control from host-reader).
- **Interrupt-driven completion.** v0.1 spins on the used ring. v0.3
  (interrupts milestone) replaces with proper IRQ handling.
- **Multiple TX in flight.** v0.1 serializes through the spin lock that
  also wraps the queue; one frame in flight at a time. Real throughput
  would use the descriptor table as a real ring with multiple in flight
  before any completion.
- **DTB-driven probing vs. hardcoded slot.** Right now we'd walk the DTB
  for `virtio,mmio` and probe each slot for DeviceID 3. Once we know
  QEMU's behavior is stable, we could hardcode. Keep the probe — same
  cost, more portable.
- **Buffer ownership.** Encode-into-stack-buffer-then-notify works because
  we spin until the device returns the descriptor. If we ever go async
  the buffer lifetime gets trickier.
