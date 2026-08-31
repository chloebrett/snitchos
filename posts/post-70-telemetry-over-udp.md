# Post 70 — telemetry over UDP: the same COBS stream, a third transport

- this one spanned a couple of weeks of calendar time (drivel and glitch and the stitch corpus kept elbowing in), but it's one arc: **SnitchOS telemetry now leaves over the network.** the `Frame` stream that has ridden a QEMU virtio-console into a Unix socket since post 3 now also rides UDP over a virtio-net device — the transport a real board would actually use.

- it's M2.5 of the VisionFive 2 port. seven PRs, host-first, all TDD'd. it works end-to-end under the deterministic snemu gate: boot with `net=10.0.0.2,…,9000`, and Hello → `kernel.boot` → `kernel.heartbeat` all come off the NIC instead of the console, get their Ethernet/IP/UDP headers stripped by a snemu device model, and decode clean.

- the through-line, which I'll spoil now because it's the whole point: **the wire was never load-bearing.** the same COBS byte stream rides virtio-console, will ride UART, and now rides UDP — because the payload was always transport-agnostic. once I actually believed that, most of the work was small deltas on things that already existed.

## the design corrected two of my own premises

- I opened with a design note, and writing it killed two assumptions from the original port plan. worth recording because I was wrong in a useful direction both times.

- **"framing is the hard part."** the plan fretted about length-delimiting frames on a raw byte pipe, since "virtio gives us message boundaries for free." reading the code: it doesn't, and never did. `decode_stream` is `postcard::take_from_bytes` over a growing buffer, generic over `R: Read`. the wire has *always* been an unframed byte stream; virtio's message boundaries were never load-bearing. the collector would have worked over a serial port unchanged.

- **the real problem is resynchronisation, not framing.** a byte stream that never loses bytes needs no boundaries. a lossy one — a UART that drops a byte, a UDP datagram that vanishes, a board reset mid-frame — needs a way to *find its feet again*. postcard is self-delimiting but not self-*synchronising*: hand it a buffer that starts halfway through a frame and it decodes garbage forever. that's the actual design problem, and it's a transport property, not a payload one.

- so the wire format became **COBS**: each frame is COBS-encoded (guaranteed no `0x00` inside) then delimited by `0x00`. after any loss the decoder discards bytes until the next `0x00` and is instantly back in sync, losing at most one frame. `postcard` ships a COBS flavour, so it was a flavour change at the encode site, not a new format. and crucially — that resync work isn't UDP-specific. it's the same recovery UART will want. write it once.

## the plan's shape: the NICs are the whole cost

- the thing the design made obvious: the value is lopsided. the packet layer (Ethernet/IP/UDP, egress-only, static addressing) is a few hundred lines of pure buffer-building, written once, identical on every target. **the cost is entirely in the NIC drivers, and they share nothing** — QEMU's virtio-net, a snemu device model, and the JH7110's DesignWare GMAC have no code in common. two are moderate; the GMAC is the multi-week monster.

- so the plan pushed *all* the reusable value below a single `NetDevice` trait (`fn send(&mut self, &[u8]) -> Result<(), TxFull>`), and made everything above it host-testable and proven under snemu before any hardware. reaching a real board becomes a contained driver bring-up behind a proven interface, not a rewrite. same move `Clock`/`SbiClock` and `FrameSink` already made.

## the packet layer, and mutants that taught me to use literals

- `kernel-net`: pure `core`, no deps, no alloc. `build_udp_datagram(config, payload, buf)` writes Ethernet II + a 20-byte IPv4 header (with a real RFC 1071 ones-complement checksum) + an 8-byte UDP header (checksum elided — legal over IPv4) + the payload. egress-only, statically addressed: no ARP, no DHCP, no ICMP, no RX. a static neighbour entry is the one trick that removes every reason to receive.

- I computed the golden datagram bytes with a throwaway Python `struct` reference and asserted against those, rather than trusting my own arithmetic. good instinct — the checksum is exactly the kind of thing you get subtly wrong.

- the mutation-testing beat: the first pass left survivors on the size constants (`MAX_BATCH = MTU - 20 - 8`). the `- with +` mutants were "equivalent" in the sense that any nearby value still worked — the exact 1472 isn't a hard contract, and a second constant independently bound the real limit, so perturbing one was masked. chasing them with a boundary test would have meant landing a frame in a ≤16-byte window — brittle. the clean fix was to **express the sizes as literals with derivation comments** (`const MAX_BATCH: usize = 1472; // 1500 - 20 - 8`). literals have no arithmetic operators to mutate; cargo-mutants only tries `→ 0`/`→ 1`, which the existing tests kill outright (a zero-size batch drops everything). the derivation lives in the comment. clean 0-missed, no gaming.

- and one genuinely-equivalent mutant documented rather than killed: `(hi << 8) | lo` → `^` in the checksum's word assembly. `hi << 8` occupies the high byte, `lo` the low — disjoint, so `|` and `^` are identical. that's a real equivalent; it goes in `mutants.toml` with the reason, next to the dozen others this repo has already found.

## the B decision: one encode, three wires

- PR 2 built `UdpFrameSink` — a `FrameSink` that takes a `&Frame`, encodes it, batches into MTU-sized datagrams, drops-and-counts on a full transmit path. then wiring it into the kernel surfaced a fork I had to stop and think about, and it's the most important decision of the arc.

- the kernel's telemetry path is: `Frame → KernelSink::emit` (which **already** wire-encodes to COBS bytes) → then ship the bytes to a transport (virtio-console today), with a pre-init buffer of *bytes* as the fallback. but `UdpFrameSink` *also* encodes. they don't compose — two encode steps.

- two ways out:
  - **(A)** make `UdpFrameSink` the kernel's sink, reworking the byte-oriented pre-init path around it.
  - **(B)** extract `UdpFrameSink`'s batching into a **byte-level core** (`UdpBatcher`, in `kernel-net`), and have the kernel push *already-encoded* bytes into it. `UdpFrameSink` becomes a thin `wire_encode`-then-batcher wrapper.

- B, and it isn't close. the kernel has **one** encode step and a byte-oriented pipeline, and it's about to have **three** byte-transports, not one: virtio-console (today), a UART sink (M2, and the parallel work landed `uart_sink` mid-arc — I watched the convergence happen in real time), and UDP. all three consume COBS bytes. the design's own load-bearing sentence — *"the payload is transport-agnostic COBS bytes; UART carries the byte stream directly, UDP carries batches of it in datagrams"* — **is** the byte-level model. A would build a `Frame`-in special case that M2's UART immediately has to unwind.

- so `UdpBatcher<D: NetDevice>` is the shared core; `UdpFrameSink` wraps it with an encode; the kernel uses the batcher directly. PR 2's tests survived the refactor unchanged (behaviour identical), which is the whole point of having had them. the batching logic didn't get rewritten — its *altitude* dropped one layer, to where it belonged for a byte pipeline anyway.

## the driver, and the shape holding (again)

- the `net=` bootarg parser (`kernel-boot`, host-tested): a const-generic `parse_bytes::<N>` handles both IPv4 octets (4, decimal) and MAC bytes (6, hex) with one function; a malformed field is a typed refusal, never a silent ignore. small, clean, 0-missed.

- the kernel virtio-net driver is where "the shape held" showed up the way it always does in this project. `kernel/src/device/virtio_net.rs` is a near-clone of `virtio_console.rs`: same static queues, same handshake (reusing the host-tested `kernel_devices::virtio` layer), same descriptor-write/notify/poll TX cycle. **the one genuinely-new function is `find_net_base` — walk the DTB, probe each virtio-mmio slot, return the one whose `DeviceID` is 1 instead of 3.** everything else is the console driver with a 12-byte virtio-net header prepended to each frame.

- I did leave a DEDUP debt and wrote it down: `read_reg`/`write_reg`/`transmit` mirror the console driver, and can't be shared as-is because each device owns its own `static mut` queues (so `transmit` is tied to a specific `TX_QUEUE`). a follow-up should lift the queue into a field and extract a shared `virtio_mmio` device. kept the net driver purely additive so I wasn't editing the console file while the surrounding tree churned — collision avoidance over immediate DRY, flagged.

## the snemu model: console and net are the same device

- for the deterministic gate, snemu needed to model the virtio-net device — receive TX descriptors, and extract the telemetry payload. and here the refactor was the interesting part.

- snemu's existing `Virtio` was a single hardcoded console device that owned the whole 8-slot MMIO window. a *separate* net struct couldn't co-own that window. so I generalized: extract a `Device { kind, status, queues, output, … }`, and have `Virtio` hold a `console` (slot 7, `DeviceID` 3) and a `net` (slot 6, `DeviceID` 1). **they share every line of the register/queue/handshake machinery and differ in exactly two things: the device-id, and how `absorb` interprets a drained TX buffer.** console keeps the raw bytes; net strips the first **54** header bytes (virtio-net 12 + Ethernet 14 + IPv4 20 + UDP 8) to reach the COBS payload.

- the 10 existing console tests were the safety net for that refactor — behaviour preserved, then two net tests added (discovery answers `DeviceID` 1; TX strips to the payload). the user, mid-arc, asked whether the net code should live in its own file. it shouldn't, and it was worth being precise about why: with the shared `Device`, "the net code" is about five lines — a `DeviceKind::Net`, a slot constant, a strip constant, one `absorb` branch. a separate file would be nearly empty and would have to reach back across a module boundary for all the shared machinery. net isn't a parallel device; it's a parameterization of one. the honest separation would be *by responsibility* (a shared `Device` vs the container), not by device.

- threading it into the itest harness meant a `TelemetrySource { Console, Net }` carried through the replay/live `View`, the shared-stream recorder, and the boot-to-checkpoint scanner (which now watches *both* streams, since the marker rides whichever transport the boot chose). and a single `workload_bootargs` mapping so a `net-telemetry` workload boots `init` with a fixed `net=` config instead of a `workload=` bootarg — one source for the workload→bootargs translation, which is Decision A of "do it principled."

## the bug, and who caught it

- the itest failed the first time. and — again — the failure is the advertisement.

- the console tail showed the kernel booting fine: banner, `tx-irq-ok`, `entering heartbeat`. alive and heartbeating. but the scenario read the **net** stream and saw nothing: *no Hello frame over UDP within 3s.* so the kernel was running, telemetry was flowing — just onto the wrong wire.

- the DTB genuinely advertised slot 6 (`fdtdump` confirmed `virtio_mmio@10007000`), so discovery wasn't it. snemu answered `DeviceID` 1 there (a passing unit test said so). the net device *was* being found. so where were the frames going?

- the trace pointed straight at it. `send_hello` and `flush_pre_init` — the two functions that put the *first* frames on the wire — hardcoded `virtio_console::send(bytes)`, bypassing the sink selection entirely. every frame the live emit path produced routed correctly to UDP; but Hello and the pre-init flush, the two most important frames, went to the console no matter what `net=` said. the sink selection was real, but two callers reached around it.

- the fix is the tell for the class of bug: **one `transmit_bytes` function that chooses the wire, and every emit site routes through it** — the live path, `send_hello`, the pre-init flush. now nothing *can* hardcode the wrong device, because there's exactly one site that names a device at all. this is a cousin of the "address materialized in two places" family from the VF2 boot work: a decision (which wire) that was correct in one spot and baked wrong in two others. the fix is always the same — make the decision have a single home.

- re-ran: 100% fidelity, plain and scrambled, and the full 128/128 suite with no regression (every other scenario defaults to `TelemetrySource::Console`). the whole chain lit up: `net=` parse → `UdpBatcher` → virtio-net TX → snemu strips 54 bytes → decoder reads Hello, boot, heartbeat off UDP.

## the collector, and a test that stalled for a minute

- PR 7 gave the collector a `--udp <port>` source alongside the socket and replay ones. and here the design's "framing was already solved" premise paid off literally: the recoverable decode **already existed** — `decode_stream` takes an `OnDecodeError` policy, `Resync` was implemented and tested (via replay), and `DecodeSummary.resyncs` was already the counter. UDP's policy is just `Resync` (a lossy transport), and the source was mostly wiring.

- the one new piece: a `UdpSocket` isn't a `Read` (it's datagram, not stream), so a `UdpReader` wraps it — `read()` serves bytes from an internal buffer, refilling by receiving the next datagram when drained. each datagram is a COBS batch ending on a frame boundary, so concatenating datagram payloads yields a clean COBS stream, and a dropped datagram just costs a batch that the next one resyncs past.

- second bug, this one environmental and instructive. the loopback test — bind a real UDP socket, send two datagrams, read them back — **passed, but took 60+ seconds.** consistently, on re-runs. the culprit: macOS assessing the ad-hoc-signed nextest binary the moment it binds a listening socket. a minute-long gate test is a non-starter.

- the fix is the finding-seams move, and it's strictly better: **inject the datagram source behind a trait.** `RecvDatagram` has one method; `UdpSocket` implements it (thin I/O glue, marked `mutants::skip`); tests use a `MockDatagrams` queue. no socket, no stall — the test dropped from 60s to 0.02s. and the seam *enabled a stronger test* than the socket version could: `decode_stream` over a `DatagramReader<MockDatagrams>` proves frames decode **across datagram boundaries**, with the empty final `recv` as a clean EOF. the seam I added to dodge a firewall made the coverage better. that keeps happening.

## the shape held, top to bottom

- the recurring theme of this project, and this arc was the densest instance yet. everywhere, **net = existing-thing + a small delta**:
  - the kernel virtio-net driver is the virtio-console driver + `find_net_base` + a header prepend.
  - the snemu net device is the console device + a different `DeviceID` + a 54-byte strip.
  - the collector UDP source is the socket source + a `Read` adapter + a `Resync` policy that already existed.
  - `UdpFrameSink` is `wire_encode` + the batcher.
  - the whole transport is the COBS byte stream + a datagram envelope.

- none of that is an accident. it's what "the payload is transport-agnostic" *means* when it's true and not just said. the wire was never load-bearing, so a new wire is a thin shell around a body of code that doesn't change.

## what's proven, and what's the actual work left

- proven, under the deterministic snemu gate: the entire path, host-testable and 0-missed on mutants at every pure layer, one itest tying it end to end. that's PRs 1–7.

- **not** done, and honestly the whole remaining cost: PR 8, the JH7110 **GMAC driver** — MAC init, DMA descriptor rings, PHY bring-up over MDIO (the board-specific reset-GPIO-and-clock-config swamp where bare-metal NIC work eats days). the port plan has said from the start that "the CPU isn't the port" — and the NIC is the sharpest instance. the packet layer, the sink, the batcher, the collector: all of that is a proven shell now. the GMAC is the thing behind the `NetDevice` trait, and it's its own project.

- also deferred and written down: QEMU `--engine qemu` parity (needs `-device virtio-net-device` + a host UDP capture — snemu is the deterministic gate, QEMU is the fidelity hatch), heartbeat-driven batching (today it's one datagram per frame, correct but wasteful — telemetry trickles below the MTU so batch-overflow would never flush), and the virtio_mmio DEDUP.

## what I learned

- **believe the abstraction or don't have it.** "the payload is transport-agnostic" was true in the code long before I acted on it. the B decision was just me finally trusting it — routing bytes, not frames, because the kernel already spoke bytes. every time I hesitated it was because I hadn't internalized that the wire doesn't matter.

- **a decision needs a single home.** the sink-bypass bug was a correct decision (choose the wire) implemented in three places, two of them wrong. `transmit_bytes` isn't cleverer code; it's the decision having exactly one site. this is the same lesson as the VF2 codegen family, in a completely different register.

- **the seam you add under duress is often the right seam.** I abstracted the datagram source to dodge a macOS firewall stall, and got faster tests *and* better coverage. finding-seams isn't a testing tax; the injection point is usually a design improvement wearing a test's clothes.

- **the observability keeps paying for itself.** two arcs in a row now (glitch, then this) where the itest failure narrated exactly what went wrong — "the kernel booted and heartbeated but sent Hello to the wrong wire" is a sentence the *frame stream* told me, not a debugger. an OS whose job is watching itself hands you the bug report before you ask.
