# Post 45 — The tree has names

- post 44 ended with an IOU: the names were on the wire, but the collector was ignoring them. every `CapEvent` — grant, mint, revoke — arrived with a name field attached, and the collector read it, advanced its internal timestamp, and moved on. the snitching was happening; the drawing wasn't. this post closes that gap. the capability derivation tree is now in Tempo, and every node has a name.

## what "in Tempo" means for a capability

- a kernel span is easy to model. it starts, it ends, it goes on the wire as a `CompletedSpan`, Tempo draws it as a bar. a capability doesn't work that way — it isn't a timed call. it's a *holding*: a permission that exists, persists, and either gets reclaimed or outlives the session. the question before writing a line of code was: what shape does a holding take in a trace view?

- the wrong answer is to make each `CapEvent` its own zero-width span. that's a lie dressed as a trace — spans are for bounded work, not moments in time. what I wanted was the honest shape: one bar per cap, from the moment it was granted to the moment it was revoked (or the session ended). Tempo renders it as a timeline of authority. when a `revoke` sweeps a subtree, the bars all close at the same right edge. that's the reclaim story told visually: "at t=900ms, this holding and everything derived from it went away."

- the one real cost of this model is that OTLP doesn't have an "open span, update it later" primitive. a span is immutable; you push it once, when it closes. a cap that's never revoked only appears in Tempo when the session ends and the collector flushes. for the grant→revoke demo loop — which does produce explicit revokes — the bar appears immediately when the cap is reclaimed. for long-lived bootstrap caps, they show up at EOF. that's fine for this project's workflow: a session capture or itest run is always finite, and the full tree is visible afterward.

- but "fine" isn't the same as "live." what if you want to *watch* authority move in real time — watch a bar appear when a cap is granted, and watch it shrink to nothing when it's revoked? OTLP can't do that. re-emitting the same `span_id` with a later end time just produces duplicate flickers in the backend; there's no delta protocol. the real answer is a different channel: a prometheus gauge (`caps_held{object,holder}`) that increments on grant and decrements on revoke. you watch the gauge move in real time in Grafana, then drill into Tempo for the named tree. that's the pairing — live signal on metrics, structure on traces — and it's a follow-up, not a compromise in what landed here.

## the moments live on the bar

- the other design question was: what do you do with the intermediate events? a cap that gets granted, then transferred to another holder, then revoked is *one* holding — one bar — but three things happened to it. making each a child span would multiply the noise. making them invisible would throw away information.

- OTLP spans have a native answer: *span events*. timestamped annotations attached to a span, visible in Tempo's detail view alongside the bar's start and end. each cap span now carries a `granted` event at the moment the holding opened, a `transferred` event (with the new holder) each time it changed hands, and a `revoked` event when it was reclaimed. the bar is the lifetime; the events are the story of what happened during it. no extra spans, no extra queries — you click on the bar and the timeline is right there.

## the collector's side

- the implementation is a `CapTracker` in the collector: a map from `cap_id` to an open holding. each `Granted` or `Transferred` event opens or updates an entry; each `Revoked` closes one and pushes a `CompletedSpan` into a drain queue. the key design call was that the collector does **not** walk the subtree itself on revoke. the kernel already does that walk — "a transitive revoke emits one `Revoked` per swept descendant" is in the protocol doc. the collector just closes each cap it's told to close. no children index, no recursive logic; the kernel does the hard part and the collector does the bookkeeping.

- cap spans live in a separate `capabilities` trace, distinct from the session trace that carries task spans and heartbeats. that's the right call for two reasons. one: span ids. kernel `SpanId`s and global `cap_id`s are different namespaces; collapsing them into one trace would require careful id-space management with no benefit. two: the authority graph is a different *thing* from the execution trace. a cap tree is a question of "who holds what and where did it come from." a task trace is a question of "what did the kernel do and when." they're both worth reading; they're not the same read.

- flush happens in two places: on a new `Hello` (kernel restart — close any open holdings from the previous session before the anchor resets) and at stream EOF (close whatever's still held at shutdown). the `main` decode loop drains the closed queue after every frame and flushes at those two trigger points. the existing session-span export path is untouched — the cap spans are a parallel stream out the same exporter.

## what I learned

- **the shape of a thing in a trace is a design question, not a given.** "capability event goes in Tempo" has at least three different answers depending on whether you model it as a moment, a holding, or a count. picking the wrong shape produces technically-correct output that tells the wrong story. the holding model is right because a capability *is* a holding — a bar that starts when the permission is granted and ends when it's taken back, with the moments of transfer marked along it. fit the representation to the thing, not to what's easiest to emit.

- **read the wire protocol before designing the host.** I wrote two paragraphs of plan about "software-side transitive closure" — a children map, a recursive walk — before reading the protocol doc closely enough to see the comment: "a transitive revoke emits one `Revoked` per swept descendant." the kernel already does the walk. the host just needs to close each cap it's told to close. assumptions about what the wire doesn't tell you are the fastest way to build the wrong thing.

- **live and structured are different channels.** you can't make a Tempo bar grow in real time; OTLP has no delta protocol for open spans. but "watch it happen now" and "understand the structure after" are different questions that deserve different tools. metrics for the live signal, traces for the named tree. using each channel for what it's actually good at means neither channel has to compromise.

## what's next

- the tree is in Tempo. the next post in this thread is `view a-file` — spawn a viewer with a scoped read cap, watch the `CapEvent` carry the grant across the process boundary, revoke it when the viewer exits. that's the powerbox demo at full height: authority you delegate, named, observed, reclaimed, and now traceable through the derivation tree. the shell has hands; it's time to use them.
