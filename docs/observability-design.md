# 📡 Observability design

# Host-side consumer (v0.1)
A small Rust binary (`host-reader`) that reads length-prefixed frames from the QEMU telemetry socket, decodes the `Frame` enum, maintains the open-span table and the string table, and pretty-prints a live span tree to stdout. That is the entire consumer for v0.1. The real collector + Tempo/Prometheus/Grafana stack arrives at v0.2 — swapping the stdout printer for an OTLP-emitting collector is a contained change because the wire protocol does not move.

# Kernel-side emit path — per-CPU rings, lossy by design
When the kernel emits a frame it must **never block on telemetry**. The emit path:

- Each CPU has its own ring buffer (`PerCpu<Ring>`). Emit writes the encoded frame into the local CPU's ring and returns immediately — no cross-CPU contention, ever.
- A separate drain step pushes ring contents out the virtio-console telemetry channel. In v0.1 the drain runs between heartbeats; later it gets a dedicated path.
- **If the ring is full, the frame is dropped** and a `dropped_frames` counter is bumped. The kernel never stalls waiting for telemetry. This is the "lossy by design under pressure" promise.
- The dropped-frame count is itself emitted as a metric — SnitchOS snitches on its own telemetry loss. You can *see* when you are losing data.

Per-CPU from line one is the same "invariants are forever" logic as span-ID partitioning: a `PerCpu<Ring>` costs nothing on single-CPU v0.1 (it is just one ring) and the emit path never needs revisiting when SMP lands.

**Known property, not a surprise:** per-CPU rings drained independently mean frames from different CPUs interleave on the wire in no global order. This is fine — every frame carries a timestamp and the host sorts. Documented here so it is a known design property rather than a bug discovered later.

For v0.1 each ring is a **fixed-capacity statically-allocated byte array** — no allocator needed (consistent with the intern table). Rings become heap-sized later if desired.

# Decisions locked
- 3 primitives (Span, Event, Metric); profiling rides on Event.
- Span-as-two-frames (SpanStart + SpanEnd).
- Span IDs: per-CPU-partitioned u64 counter, no RNG.
- Time: raw u64 cycles on the wire; `timebase_hz` sent once in `Hello`; host converts. No wall clock.
- Strings: one mechanism, runtime interning; `u32` refs; fixed-capacity static intern table for v0.1, heap-backed from v0.4.
- Single `Frame` enum, postcard-encoded, length-prefixed on the wire.
- All 7 frame types defined in the `protocol` crate now; kernel uses 5 in v0.1; no protocol change at v0.2.
- Metric value type: `i64` for now; widens to a union later if needed.
- Emit path: per-CPU ring buffers, drop-on-full, dropped count is itself a metric. Static rings for v0.1.
