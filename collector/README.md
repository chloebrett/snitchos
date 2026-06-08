# collector

Host-side telemetry daemon. The kernel emits postcard-encoded
[`protocol::Frame`](../protocol)s over a virtio-console socket; the
collector is the other end of that wire. It decodes the stream,
reconstructs the kernel's observable state, and fans the result out to
the observability stack:

- **Tempo** — completed spans as OTLP/HTTP traces
- **Loki** — completed spans as human-readable log lines
- **Prometheus** — counters / gauges / histograms over a `/metrics` endpoint
- **stdout** — raw decoded frames (`--text`, for ad-hoc debugging)

All four are on by default (except `--text`); each has a `--no-*` flag.
`cargo xtask collect` runs the full set against the docker-compose stack;
`cargo xtask reader` is the text-only shortcut (no stack needed).

Bin-only, `publish = false` — nothing imports it.

## How it works

```
unix socket ──▶ protocol::stream::decode_stream ──▶ State::handle(frame)
                                                          │
                                  ┌───────────────────────┴── returns Some(CompletedSpan) on SpanEnd
                                  ▼
                       for exporter in exporters { exporter.export(span) }   ← push: OTLP, Loki
                       prom::serve reads State live behind a Mutex           ← pull: Prometheus scrape
```

`State` is the heart of it: a stateful observer that accumulates the
name table, metric types, currently-open spans, and latest metric
values as frames arrive. When a `SpanEnd` matches a remembered
`SpanStart`, `handle` returns a `CompletedSpan` ready to export.

| Module | Responsibility |
|---|---|
| `main` | CLI (`clap`), socket connect, decode loop, exporter fan-out |
| `state` | The frame observer: span matching, metric tables, tick→wall-clock anchoring |
| `otlp` | OTLP/HTTP trace exporter (hand-rolled proto subset via `prost`) |
| `loki` | Loki log-push exporter |
| `prom` | Prometheus `/metrics` server + text-exposition formatting |
| `url` | `ensure_suffix` — idempotent endpoint-path joining, shared by the HTTP exporters |

## Surprising details worth knowing

**Two clocks, reconciled at the first frame.** The kernel has no wall
clock — its timestamps are raw cycle counts (`t`). Tempo/Loki need
absolute time. So `State` anchors the *first* frame's `t` to the host's
`SystemTime::now()`, then converts every later `t` to nanoseconds since
epoch using `timebase_hz` (sent once in `Hello`). Consequence: pre-init
burst frames carrying `t < first_t` are mapped to a time slightly
*before* the anchor — a documented quirk, not a bug.

**`Hello` is a session boundary, not a handshake.** A *second* `Hello`
means the kernel restarted, so `handle` wipes the string/metric/span
tables (`reset_session`). All accumulated state is per-session; reconnect
semantics fall out of this for free.

**Pull vs push are deliberately different shapes.** Prometheus *scrapes*,
so `prom::serve` spawns a blocking `tiny_http` thread that reads a live
`Arc<Mutex<State>>` on each `/metrics` hit. OTLP and Loki are *pushed*
once per `CompletedSpan`, fire-and-forget. That asymmetry is why `State`
is shared+locked while the exporters are stateless per-call.

**One trace per kernel session.** Every span from one run shares a single
`trace_id` (derived from the collector's start-time nanos — uniqueness
per run is all that's needed, not entropy), so a whole boot shows up as
one Tempo trace. A new `Exporter::new()` starts a new trace.

**Histograms are bucketed host-side.** The kernel emits histogram
observations as ordinary `Metric` frames; `State` routes
`Histogram`-kind metrics into per-bucket counts (stored *non*-cumulative),
and `prom.rs` converts to the cumulative `le=` buckets Prometheus expects
only at exposition time. Metric names are sanitized there too
(`snitchos.heartbeat.count` → `snitchos_heartbeat_count`; Prometheus
forbids dots).

**The OTLP proto is a hand-picked subset.** No `.proto` file, no
`build.rs` — `otlp.rs` declares just the message tree we actually emit
(`ExportTraceServiceRequest → ResourceSpans → ScopeSpans → Span`) as
`prost`-derived structs. Enough for spans with timing, parent linkage,
and `thread.{id,name}` + `host.cpu_id` attributes (built by the pure
`span_attributes` helper); no span-events, links, or full attribute
support.

**Some frames are accepted but not yet surfaced.** `Event`,
`ContextSwitch`, and `HartRegister` are decoded and acknowledged but
don't yet produce OTLP/Prometheus output (`ContextSwitch` still advances
the time anchor so downstream timestamps keep progressing) — matching the
protocol's "define the wire format ahead of its consumers" stance.
They're reserved, not dropped. Note the hart a span ran on *is* now
surfaced — as `host.cpu_id`, sourced straight from `SpanStart.hart_id`
rather than from `HartRegister`. `HartRegister`'s `role` (Boot/Worker)
stays unsurfaced; a per-hart *metric* label would need it, but
`Frame::Metric` carries no `hart_id`, so per-hart metric labels are
deferred behind a wire change. `Event` is the OTLP span-event slot,
awaiting its first emitter (~v0.8).

**Exporters never take the process down.** HTTP failures (Tempo/Loki
down, slow, non-2xx) are logged to stderr and swallowed; there's no
retry or backpressure, and export is one POST per span. Fine at
heartbeat rates; would need batching under load.

**`WallClock` is injected** (`trait WallClock`) so `State` is fully
host-testable with a pinned `FakeWallClock` — otherwise every timestamp
assertion would race the real clock. `SystemWallClock` is the only
production impl.

## Tests

`cargo test -p collector` — all logic is host-testable. Span matching,
session reset, tick→wall-clock conversion, histogram bucketing, and
Prometheus formatting are covered directly; the I/O boundaries (socket
read, HTTP POST, TCP bind, real clock) are `mutants::skip`'d and verified
by running the binary. Stream-decoding tests live in
[`protocol::stream`](../protocol/src/stream.rs).

## See also

- [docs/observability-design.md](../docs/observability-design.md) — wire format + span semantics, the "why" behind the emit/decode split
- [protocol](../protocol) — the `Frame` contract this crate consumes
