# Post 22 — Which hart said that?

- v0.6 put `hart_id` on the wire end-to-end — every span, every context switch knows which CPU it ran on. post 14 proved it; `smp-spans-carry-hart-id` guards it. then it falls off a cliff: the collector decodes the field and throws it away. the trace _knows_ which hart, and Grafana can't tell you. closing v0.6 was cashing that promise twice — once for traces, once for metrics. the first was five lines. the second made me break the wire.

## the field nobody read

- `SpanStart` carries `hart_id`. the collector's handler destructured it as `hart_id: _` — decoded, then dropped on the floor. one line of the post-20 disease: a thing fully present on the wire that no consumer reads is a lie of omission. the data says "I know which CPU"; the dashboard says "no idea."
- the fix is the boring kind: thread `hart_id` through `SpanStart` → `OpenSpan` → `CompletedSpan`, then emit it as a `host.cpu_id` OTLP attribute. now Tempo can slice a trace by the hart it ran on. that's the whole v0.6 trace story made visible — "task_a opened a span on hart 0" is finally a thing you can _filter on_, not just a number in a frame dump.

## the test that paid an old debt

- the OTLP attribute-building lived inline inside the exporter's `export()` — which is `#[mutants::skip]` because it makes real HTTP calls. so `thread.id` and `thread.name`, shipped two milestones ago, had **zero** test coverage. they rode inside an untestable function.
- to TDD `host.cpu_id` honestly I pulled the attribute set out into a pure `span_attributes(&CompletedSpan) -> Vec<KeyValue>`. testing the new field meant the old two got covered for free — `thread.id` always, `thread.name` when resolved, `host.cpu_id` always. the seam I cut for the new code retroactively pinned the code that was already there. mutation: 2/2.
- **moral, small but real: extracting a testable seam doesn't just test the new line. it tests everything that was hiding behind the same skip.**

## the second promise needed a wire break

- post 21's closeout list said, in one breath, "populate `host.cpu_id` _and per-hart metric labels_." I went to do the second half and hit a wall: `Frame::Metric { name_id, value, t }` **has no `hart_id`.** spans carry it; context switches carry it; metrics don't. you cannot label a metric by a hart the wire never told you about.
- worse, the absence hid a latent bug. collector metric state was keyed by `name_id` alone — `metric_values.insert(name_id, value)`. two harts emitting the same counter name is last-write-wins: hart 1's `context_switches_total` silently clobbers hart 0's. it doesn't bite _today_ only because today exactly one hart emits metrics (the heartbeat runs on hart 0; hart 1 idles). it's a bug in waiting, armed the moment a second producer appears.
- so: break the wire. `PROTOCOL_VERSION` 2→3, add `hart_id: u8` to `Metric`. this is the cheapest possible moment — pre-userspace, no external consumer of a v0.6 capture exists, the integration suite is the only reader and it updates atomically. the same reasoning post 14 used for putting `hart_id` on spans. the bill for a wire break only ever goes up.

## don't put the hart in the name

- the tempting shortcut is to bake the hart into the metric _name_ — `..._hart0`, `..._hart1` — and skip the wire change. the codebase already does this in one place: the storm-workload guards (`mutex_storm_acquires_hart0` / `_hart1`) are two differently-named counters precisely because there was no other way to tell them apart.
- it's the wrong instinct, and Prometheus is the reason. a hart is a **dimension**, not an identity. `snitchos.sched.context_switches_total{hart="0"}` and `{hart="1"}` are one metric you can `sum without(hart)` to get the system total, or break out per-CPU. two _names_ can't be summed, can't be compared, can't share a panel — they're strangers that happen to rhyme.
- so the collector now keys metric state by `(name_id, hart_id)` and emits the hart as a label. the metric _kind_ stays keyed by name alone — `MetricRegister` has no `hart_id`, and a counter is a counter regardless of who emits it. the Prometheus exposition groups by name (one `# TYPE` per family) and writes one labelled line per hart:

```
# TYPE snitchos_sched_context_switches_total counter
snitchos_sched_context_switches_total{hart="0"} 1041
snitchos_sched_context_switches_total{hart="1"} 0
```

## the metrics all say hart 0

- here's the honest part. after all that — wire break, re-keyed state, labelled exposition — every metric in the live system reads `hart="0"`. because only hart 0 emits. the frame dump from the integration run is wall-to-wall `hart=0`, including the SMP metrics: `shootdowns_received_total{hart="0"}` is hart 0's heartbeat _reading_ a counter the cross-hart machinery bumped. that label is correct — "hart 0 reported this value" — it's just not yet _plural_.
- this is interface-before-implementation, same as the reserved `Event` slot from post 20. the capability is real and tested; the visible split waits for a producer on hart 1. when v0.9 preemption or a future per-hart workload starts emitting from the second core, those samples land under `{hart="1"}` instead of overwriting hart 0 — and the dashboards already know how to draw them. I'd rather ship the dimension idle than retrofit it under a clobber bug.

## what i learned

- **a decoded-and-dropped field is the same bug as a stale comment** — the system claims to know something it won't tell you. post 20 found a field _claimed_ and absent (the phantom length prefix); this was a field _present_ and ignored. both are the wire and the story disagreeing.
- **"and also X" in a closeout list can hide a wire break.** "populate host.cpu_id and per-hart labels" sounded like two collector edits. one was. the other was a protocol version bump because the data didn't exist yet. read the wire before you size the task.
- **key by the dimension, not the name.** the instant a quantity can come from more than one source, the source is a label and the storage key is a tuple. baking it into the name feels faster and costs you every aggregation forever.
- **break the wire while it's free.** no external consumers, integration tests as the only reader — that window closes the day someone records a capture you have to stay compatible with.

## what's next

- v0.6 is done: SMP substrate, both telemetry promises cashed, the wire fully read on both ends. the reserved `Event` slot is the last frame still dropped on the floor — it graduates when profiling lands.
- next is **v0.7a: the first userspace process, built deliberately wrong.** one syscall, ambient authority, the Unix way — so v0.7b's capability rewrite can feel exactly what that costs. the substrate is multi-hart-correct underneath it from day one, which was the whole point of doing SMP first.
