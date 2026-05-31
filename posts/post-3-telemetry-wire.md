# Post 3 — Telemetry on the wire

- the killer feature in line of sight. kernel emits its first structured telemetry frame, the host pulls it off a different transport, hex-decodes the bytes. real second channel.

## protocol crate (TDD)

- new crate, `no_std`, hosted tests. wire format design.
- chose **postcard** — serde-derive friendly, varint encoding (small ints = small bytes), no_std-friendly, schema-evolution friendly. ditched the cookie-cutter `bincode`.
- `serde = { default-features = false, features = ["derive"] }` to keep std out.
- one `Frame` enum, 7 variants: `Hello`, `StringRegister`, `SpanStart`, `SpanEnd`, `Event`, `Metric`, `Dropped`. defined upfront so the kernel and host-reader speak the same language from day one.
- roundtrip tests: encode → decode → assert equal. one test per variant.
- TDD rhythm: red (test for missing variant) → green (add variant) → repeat. caught the agent batching tests + impl in one edit and called it out; the rhythm matters even when the next case is "obviously" the same shape.
- moral: the discipline is the point, not the verification. familiar patterns are exactly when you skip the red and start trusting yourself instead of the tests.

## the string variant has a wrinkle

- `StringRegister { id: u32, value: &str }` — strings in no_std can't be `String` (no alloc), need `&str` borrowed.
- the borrow forces a lifetime on the whole enum: `Frame<'a>`.
- `fdt`-style problem propagates: every reference to `Frame` may need `Frame<'_>`. edition 2024 elides most of them.
- decoded `Frame` borrows from the input buffer. caller has to keep the buffer alive through the use of the frame. host-reader can just hold its receive buffer.

## newtypes for the id namespaces

- two different "id" worlds: span ids (u64, per-CPU-partitioned counter, lots of them) and string ids (u32, indexes into the intern table, far fewer).
- type aliases (`type SpanId = u32`) don't enforce anything. wanted real compile-time separation.
- `#[serde(transparent)]` newtypes — `pub struct SpanId(pub u64)`, `pub struct StringId(pub u32)`. zero runtime cost, wire format unchanged, but mixing them is now a compile error.
- matches the project ethos (capabilities are unforgeable handles). namespaces shouldn't bleed.

## virtio: what and why

- needed a second channel for binary telemetry (separate from the println UART). thought about a second NS16550 via QEMU `-serial`. **doesn't work on QEMU virt** — the machine has exactly one 16550 baked in; additional `-serial` flags route through virtio under the hood. verified by dumping the DTB.
- so: virtio-console. and that meant learning virtio properly.
- **virtio = "para-virtualized device interface."** guest knows it's virtualized and cooperates with the hypervisor instead of pretending to be on real hardware.
- compare:
  - **full virtualization**: emulates hardware (slow; every register access traps and emulates).
  - **paravirtualization (virtio)**: shared memory for data, traps only for "poke" notifications. fast.
- standardized so every hypervisor implements one protocol instead of each one inventing their own.

## virtio vs MMIO — what's the relationship?

- the question I asked: if MMIO works for NS16550, why do we need virtio?
- answer: **virtio uses MMIO underneath.** specifically `virtio-mmio` is one of three transports (mmio, pci, ccw). it's not *instead of* MMIO — it's a protocol layered on top.
- three reasons to bother with the higher layer:
  - **one trap per byte vs one trap per batch.** NS16550 = one MMIO write per byte = one trap-and-emulate cycle per byte. virtio = "process this 60-byte buffer" = one trap.
  - **bulk data via shared memory.** NS16550 has 8 byte-wide registers; to send 1 KB you write 1024 bytes one at a time. virtio puts the data in guest RAM and the host reads it directly.
  - **standardization.** NS16550, every NIC, every disk = unique register layouts. virtio = one handshake + one virtqueue shape for net, block, console, rng, input, gpu, ...
- for our case (low-bandwidth console), trap-per-byte would have been fine. used virtio because (a) the NS16550 was already taken by println, (b) it's the future for everything else (net, rng, ...).

## a quick tour of other virtio device types

- in any QEMU virt machine the 8 mmio slots can hold any of: net (NIC), block (disk), console (us), rng (hardware random for entropy seeding), input (keyboard/mouse), gpu (display), 9p (filesystem passthrough), balloon (memory reclamation).
- **9p** is the **plan 9 filesystem protocol**. plan 9 = bell labs OS that took "everything is a file" to its conclusion (processes, network conns, GUI windows all as files in a namespace). 9P is its filesystem-over-the-wire protocol. influences SnitchOS's `Filesystem` trait shape down the line.

## DTB describes the transport, not the device

- thought I'd dump the DTB after adding `-device virtconsole`, see a populated slot. nope — DTB shows the same 8 generic `virtio_mmio@1000X000` slots regardless of what's attached.
- by design: **the DTB describes virtio-mmio slots; you probe each one at runtime** by reading its MagicValue + DeviceID registers. QEMU virt always declares 8 slots, devices populate them dynamically.

## QEMU defaults to legacy virtio

- first probe showed `version=1` (legacy spec) on the console slot.
- legacy virtio has a different (older) register layout we don't support.
- fix: `-global virtio-mmio.force-legacy=false` in the QEMU args. now `version=2` and our register offsets are correct.

## virtio-mmio register layout

- learned to read this table:
  - `MagicValue` at offset 0: the literal bytes `"virt"` as a u32 LE = `0x74726976`.
  - `Version` at offset 4: 1 (legacy) or 2 (modern).
  - `DeviceID` at offset 8: 0 = empty slot, 3 = console.
  - feature negotiation registers (paged 32-bits at a time via `*Sel`).
  - per-queue setup (paged again via `QueueSel`).
  - `Status` is a state-machine: OR in bits as the driver progresses through init.

## the handshake

- canonical virtio init flow:
  1. Reset (Status = 0).
  2. ACKNOWLEDGE.
  3. DRIVER.
  4. read DeviceFeatures (low + high 32-bit halves via Sel).
  5. write back what we accept via DriverFeatures.
  6. FEATURES_OK (and verify it stuck — the device clears it if it rejects our subset).
  7. set up virtqueues.
  8. DRIVER_OK.
- **must accept VIRTIO_F_VERSION_1 (bit 32)**. without it the device thinks you're a legacy driver pretending.
- if anything goes wrong, OR FAILED into Status. cleanup signal.

## virtqueue: descriptor table + avail ring + used ring

- the three regions of guest RAM the device reads/writes:
  - **descriptor table**: array of `{addr, len, flags, next}` — slots pointing at buffers.
  - **available ring**: driver-writes, "process descriptor #N." monotonic `idx`, indexed at `idx % QSIZE`.
  - **used ring**: device-writes, "I finished descriptor #N." same shape, different writer.
- pattern repeats: shared memory + monotonic index + modular arithmetic. same pattern SnitchOS IPC will adopt.

## descriptor details

- `#[repr(C)]` so the layout matches what the device expects byte-for-byte. without it Rust may reorder fields for performance.
- 16 bytes per descriptor: `u64 addr + u32 len + u16 flags + u16 next`.
- flags is `u16` not `u8` for layout reasons: after `addr(8) + len(4) = 12` bytes, a `u8 flags + u8 padding + u16 next` would waste a byte. `u16 flags` fits naturally at offset 12 (which is 2-aligned). exactly 16 bytes, no padding.
- `next` is `u16` → max queue size is `2^16 = 65536` descriptors. ours is 8. spec requires powers of two.

## static-mut and `&raw`

- no allocator in v0.1. virtqueue lives in `.bss` as a `static mut TX_QUEUE: Virtqueue = ...` with an all-zero const initializer.
- accessing static-mut in rust 2024 requires `unsafe` blocks AND the `&raw mut` / `&raw const` syntax (no normal references because UB rules).
- the pattern reads ugly but the discipline is right: every read and write of the rings is volatile-ish (the device may be touching the memory too).

## transmit, the conceptual finale

- the 5-step TX cycle:
  1. fill descriptor slot 0 (`addr = bytes.as_ptr() as u64, len = bytes.len() as u32, flags = 0, next = 0`).
  2. snapshot `avail.idx` and `used.idx` *before* submitting.
  3. push descriptor index 0 into `avail.ring[avail.idx % QSIZE]`, bump `avail.idx`.
  4. write `QUEUE_TX` to `QueueNotify`. the only trap; everything else is silent shared-memory coordination.
  5. spin until `used.idx` advances past the snapshot. confirms the device drained the buffer; safe to release the bytes.
- bug I made: compared `used.idx` to `avail.idx` instead of to the previous `used.idx`. they're independent counters; second-call onward, the comparison was always false → loop exited immediately → use-after-free on stack memory if anyone was watching.
- subtle point: spin on `used`, snapshot `used`, not `avail`.

## the bugs

- **timebase: 0 Hz.** `dtb.cpus().next().properties()` returns `cpu@0`'s properties only. the `timebase-frequency` property lives on the *parent* `cpus` node. fix: `dtb.find_node("/cpus")` then look there. so my postcard-encoded Hello frame was 3 bytes (with timebase = 0) instead of 11.
- **must initialize BOTH RX and TX queues.** virtio-console spec requires it even if you never plan to receive. without RX setup, my TX bytes went to nowhere (`used.idx` advanced — device silently consumed them and discarded). added an empty RX queue and TX started actually delivering.
- **socket lifecycle.** xtask cleaned up `/tmp/snitch-telemetry.sock` at start of every `up`. if `nc` was connected to the previous one, the deletion killed it. fix: use `wait=on` on the chardev so QEMU blocks until a client connects; user starts `nc` AFTER `xtask up`.
- **stdio buffering bites.** `nc -U socket | xxd` shows nothing live, even after the kernel sends bytes. block buffering on the `nc → xxd` pipe (4 KB by default) holds my 11 bytes hostage until EOF. `nc > file` works because no pipe in between. the real fix is the host-reader, which is up next.

## what i learned

- **virtio is a protocol over MMIO, not a replacement.** the layering question I asked turned out to be the most useful framing for the whole topic.
- **read the spec carefully.** "must initialize both queues" was one line; ignoring it cost me an afternoon of bytes-going-nowhere debugging.
- **`#[serde(transparent)]` is a power tool** for getting type-level safety without paying any runtime / wire-format cost. should use it more.
- **subtle counters need careful naming.** the "compare to the snapshot of the right counter" bug was insidious — it didn't crash, it just silently lied. tests + names that track which counter is which would have caught it sooner.
- **`fdt` 0.1.5 doesn't propagate inherited DTB properties.** the `cpus`-vs-`cpu@0` confusion would have been caught by a richer crate. file under "consider better crate" for v0.2.

## next

- finish the host-reader. connects to the socket, decodes `Frame`s, pretty-prints. scaffold is in; the three function bodies (`connect`, `read_frames`, `print_frame`) come next.
- after that, framing on the wire (length-prefix each `Frame` so the host can find boundaries in a continuous stream).
- then the kernel emits a real span tree at boot — `kernel.boot { serial_init, telemetry_init, ... }` plus a heartbeat loop. screen recording.
- that's v0.1.
