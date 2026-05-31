# Post 6 — Grafana arrives

- v0.1 ended with the kernel emitting a real span tree on the wire and a host-reader pretty-printing it. v0.2 makes it _useful_: Tempo (traces), Prometheus (metrics), Grafana (dashboards). The same kernel workload, now visible in something that looks like a product.

![tracing](tracing.png)

## protocol change: typed metrics

- new wire variant: `Frame::MetricRegister { name_id, kind }` parallel to `StringRegister`. declare metric type once, reference by `name_id` thereafter.
- `MetricKind` enum: `Counter | Gauge | Histogram`. counter = monotonic, gauge = snapshot, histogram = distribution (placeholder; bucket encoding TBD).
- the alternative we rejected: "always treat metrics as gauges, synthesize counters via Prometheus `rate()`." silently wrong when counters reset. typed metadata costs ~4 bytes of wire per register; pays for itself instantly.
- kernel side: `tracing::register_counter("foo") -> StringId`, plus `register_gauge`, `register_histogram`. each registers the string AND the metric type on first sight; subsequent calls are idempotent (a `metric_registered: bool` bit on the intern table entry).

## the host-reader → collector rename

- one binary, multiple output modes: `--text` (stdout debug), `--otlp <url>` (Tempo), `--prometheus <port>` (scrape target).
- xtask shortcuts:
  - `cargo xtask collect` — full output (OTLP + Prometheus, no text). defaults to docker-compose endpoints.
  - `cargo xtask reader` — text only, no docker dependency. shorthand for `--text --no-otlp --no-prometheus`.
- clap derive throughout. typo protection, `--help`, `--version`.

## the over-engineering I had to back out

- on the way through the agent decided to factor `decode_stream` out of host-reader into the `protocol` crate, gated by a `std` feature. my pushback: "wait why does the stream reader need to live in protocol if it's only used by collector?"
- agent had justified it with "two consumers" — host-reader AND collector. but we'd collapsed those into one binary. agent didn't re-examine the decision after the premises changed.
- moved it back. protocol = wire format, collector = wire-format consumer. clean separation.
- filed the meta-lesson: "when premises change, re-examine the decisions that depended on them."

## docker-compose stack

- three services: Tempo (OTLP receiver + trace store), Prometheus (metric scrape + TSDB), Grafana (dashboards). `stack/docker-compose.yml` brings them all up via `cargo xtask stack up`.
- Grafana datasources + dashboard auto-provisioned via mounted YAML and JSON files. zero clicks to get a working dashboard on stack-up.
- two debugging adventures on the way:
  - **`compactor field not found in type app.Config`** — `grafana/tempo:latest` shifted under me. pinned to `2.6.0`, simplified the config (let Tempo use defaults for compactor).
  - **`mkdir /tmp/tempo/blocks: permission denied`** — Tempo runs as a non-root user; the named volume mount point at `/tmp/tempo` was owned by root. fix: mount at `/var/tempo` instead (the Tempo image has chowned this path for its user).
- moral: known-version image pins + minimal config = fewer surprises.

## collector State

- single mutable state machine that observes the frame stream and builds up:
  - `timebase_hz` (from Hello)
  - `session_anchor` (wall-clock + first frame t — for tick → wall-clock conversion)
  - `strings: HashMap<u32, String>` (from StringRegister)
  - `metric_kinds: HashMap<u32, MetricKind>` (from MetricRegister)
  - `open_spans: HashMap<u64, OpenSpan>` (SpanStart awaiting End)
  - `metric_values: HashMap<u32, i64>` (latest value per metric)
- `state.handle(frame)` updates state; returns `Some(CompletedSpan)` when a SpanEnd pairs with an open SpanStart.
- tests for everything, including the pre-init quirk (spans with `t < first_t` land slightly before the anchor).

## the OTLP exporter

- went with minimal raw protobuf + ureq instead of the full `opentelemetry-otlp` SDK. trade: ~60 lines of `prost`-derived proto types and a hand-rolled POST vs ~10 lines of pipeline setup + tokio + tracer ceremony. for v0.2 we have our own span model already — the SDK abstractions were fighting us.
- proto subset: ExportTraceServiceRequest → ResourceSpans → ScopeSpans → Span. no attributes, no events, no links. just spans with start/end times and parent linkage.
- `service.name = "snitchos"` as a Resource attribute so Grafana can group by service.
- per-frame export (one HTTP POST per SpanEnd). easy to batch later; the call site is one place.

## Hello-must-come-first

- spent a satisfying afternoon staring at "Tempo received 548 spans, ingester stored 59 traces, but Grafana shows nothing."
- root cause: collector started mid-session (after kernel was already running). it missed the Hello frame. without an anchor, the wall-clock conversion returned `0` → every span had `start_time_unix_nano = 0` → spans were filed at **Unix epoch 1970-01-01**. Grafana's "Last 5 minutes" never finds them.
- fixed twice over:
  - **collector**: warn loudly (once) when a non-Hello frame arrives before Hello, then drop. tests for the drop behavior.
  - **kernel**: reorder kmain so `send_hello()` runs _before_ `flush_pre_init()`. Hello is the very first frame on the wire, always.
- moral: protocol invariants you write down in plans are only real if you enforce them in code. "first frame is Hello" was an invariant in `plans/v0.2-grafana.md`; the collector's drop+warn + the kernel's reorder are what actually make it true.

## Prometheus /metrics

- tiny_http (blocking, single-threaded server), one endpoint, formats `State.metric_values` as Prometheus text.
- name munging: `snitchos.heartbeat.count` → `snitchos_heartbeat_count` (Prometheus forbids dots).
- Prometheus scrapes every 5s. Grafana queries Prometheus. the metric panels light up.
- `State` wrapped in `Arc<Mutex<...>>` so the decode loop and the HTTP server thread both touch it. trivial contention in practice.

## kernel metrics (v0.2 set)

- `snitchos.heartbeat.count` (counter): bumped once per heartbeat.
- `snitchos.intern.strings_used` (gauge): currently-registered strings; visible fill rate vs `MAX_INTERNED=64`.
- `snitchos.time.ticks` (gauge): latest `time` CSR snapshot. mostly proves the kernel didn't wedge.

## the Grafana dashboard

- 5 panels, provisioned as JSON, auto-loaded by Grafana on start:
  - heartbeat count (timeseries)
  - strings in intern table (stat, thresholded green/orange/red at 56/64)
  - time CSR ticks (stat)
  - heartbeat rate per second (`rate(snitchos_heartbeat_count[1m])`)
  - recent kernel spans (Tempo table, `{ resource.service.name = "snitchos" }`)
- got the traces panel wrong twice: first as panel type `traces` (which is Explore-only, not a dashboard visualization), then with a TraceQL `{}` (legal but inefficient). landed on type `table` with the explicit service.name filter.
- 5-second refresh; matches the Prometheus scrape interval.

## what i learned

- **per-frame OTLP POSTs are fine at heartbeat rates** — one HTTP request per second is nothing. would batch under real load.
- **`docker compose` errors are usually one of three things**: image got an old config, file permissions on a mount, port already in use. checking `logs <service>` first is always faster than reading the compose file again.
- **the right panel type is the one Grafana publishes a JSON example for** — if "Table" works for traces in Grafana's own examples, that's your answer; "traces" as a panel type might not exist for dashboards.
- **protocol invariants need an enforcer**. "first frame is Hello" was already documented; only when collector started warning + kernel reordered did it become true.
- **typed metrics > naming-convention metrics**. `_total` suffix means counter is a convention; `MetricKind::Counter` on the wire is a guarantee. 4 bytes of metadata per register, infinite confusion avoided.

## what's next

- **v0.2 polish**: figure out the unmatched-span leak in collector (open_spans never drains for spans the kernel left open), maybe add a sweep. capture a screenshot for the README. write the "v0.2 done" status update.
- **v0.3 — interrupts & clock**: timer interrupts via the SBI timer extension, monotonic `Clock` trait, trap handler, heartbeats become timer-driven instead of busy-spinning. first histogram metric: `snitchos.irq.duration_ticks`.
- **maybe SMP between now and v0.3**, given the offhand "could we pull SMP earlier" conversation that turned out to be more pragmatic than expected. the corners doc (`plans/scaling-corners.md`) is our checklist for that pull.
