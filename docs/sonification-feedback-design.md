# Sonification and the self-observation feedback loop

**Status:** 📐 **DESIGN NOTE — speculative (v2+).** Not a plan. Captures a design
problem discovered while extracting the `synth` crate for
[glitch](../plans/glitch.md): the *sonifier* — a future userspace server that turns
the telemetry `Frame` stream into sound — feeds back on itself, and naming the loop
precisely reveals what the real prerequisite is. No code hangs off this yet; it exists
so the insight isn't lost. Builds on the audio arc in
[vf2-audio-design.md](vf2-audio-design.md).

---

## Where this comes from

The audio arc has three lenses (see the audio design doc): a real stack, an
observability *output*, and a real-time *forcing function*. The middle one —
**sonification** — is "you can hear a boot": heartbeat → tick, context-switch →
click, OOM → a falling tone. Post 62 sketched it; this note is about the one
structural problem it hides.

A sonifier is **a client of glitch**, not new device machinery. glitch arbitrates the
DAC (holds `Object::AudioSink`, serves `Play`). A sonifier sits above it:

```
synth  (pure DSP)  ──►  glitch  (DAC cap, serves Play)  ──►  sonifier  (Frame → Play)
```

To do its job the sonifier needs a capability the system does **not** have yet: a
**read tap on the telemetry `Frame` stream**. Today frames flow one way — kernel →
collector, out the virtio-console. Nothing in userspace can *consume* them. Call the
missing primitive `FrameSubscribe` (it's also what collector-as-server and any in-OS
dashboard would need — so it's load-bearing well beyond audio).

## The loop

The sonifier **consumes frames and, by acting, produces frames** — including frames
about the very act of making sound. That's a feedback loop, and it is exactly
acoustic feedback (mic into speaker) expressed in the logical domain.

The naive defense — "drop frames I emitted" — **does not work**, because the sonifier
authors almost none of the frames its action causes. One `Play` fans out across
principals it does not own:

- glitch emits the `glitch.play` span + `plays_total`;
- the kernel emits `samples_emitted`, the `AudioWrite` cap-check outcome;
- the IPC rendezvous + the scheduling to service it emit `ContextSwitch` frames.

So `origin == my_pid` catches nearly nothing. The thing to exclude is not "frames I
authored" — it is **my entire causal cone**: everything downstream of my action,
across every principal it touched. That is a **taint/provenance** problem, not an
identity check.

And it is not specific to audio. Map `ContextSwitch → click` and a click *is* a Play,
which *is* context switches, which are more clicks — the loop closes through the
**scheduler**, never touching an audio frame. This is the **observer effect**: the
sonifier is a probe that perturbs the field it measures, and the perturbation returns
as signal. Any self-observing actor that touches the shared substrate (sched, IPC,
alloc) has it.

## The fix has the same shape as the acoustic fix

Acoustic feedback is tamed two ways at once, and the logical version takes the same
two layers:

### 1. Don't route output back to input — causal exclusion

The sonifier opens a `sonify` activation; if that activation id propagates through the
`Call` into glitch and into the `AudioWrite` syscall (and through the scheduling that
services them), then its whole causal cone carries that root, and it drops any
incoming frame descended from it.

This needs **no new synthesis machinery** — it is the existing span-parentage spine —
**with one prerequisite**: *span/causal context must cross the IPC `Call`/`Reply`
boundary between processes, and be inherited by syscall handling.* Within a task it
already propagates (`CURRENT_SPAN_CURSOR`); cross-process over IPC it does **not** yet.

**That propagation is the real missing primitive — bigger and more useful than
`FrameSubscribe` itself**, because it is what makes *any* distributed trace in this OS
causally correct (it's the W3C-`traceparent` idea: an ambient causal id threaded
through every scheduling / IPC / syscall hop). The frame tap is easy; causally-correct
cross-process tracing is the load-bearing work.

### 2. Keep loop gain < 1 — rate / energy limiting

Causal exclusion is inherently **racy**: the sonifier must act on a frame *before* it
can know the subtree that action will spawn, so some self-frames always leak. The
backstop is the sound engineer's: debounce + a decay envelope + a max-events/sec cap,
so the residual loop **converges** instead of howling. You want this regardless — a
click per context-switch at kHz is unlistenable — and it makes the system stable even
where exclusion is imperfect.

The honest design does **not** pretend at perfect isolation. The sonifier is *inside*
the system it listens to. Exclude the causal cone where you can; cap the gain so the
part you can't exclude decays.

## Research-tier aside: could provenance close it *completely*?

Not advocating — noting what it would take, because this OS is oddly well-positioned.
Full causal exclusion (not just the racy prediction of §1) means **total, automatic
provenance**: every frame carries a causal-activation tag, and it propagates through
*every* hop — IPC, syscall handling, the scheduler servicing the work, the allocator
if it emits. Any untagged path is a leak. That is dynamic taint tracking at OS scope —
genuinely hard, and normally a research OS in its own right.

But snitchos already has three-quarters of the substrate a normal kernel lacks:
**everything is already a structured `Frame` with provenance**; there is a span spine
that propagates across context switches; and the **cap-id spine** (v0.13) already
threads parent links through grants/transfers. The gap is narrower than usual — a
distinct *activation* id (not the semantic span: one sonifier does many actions),
threaded as ambient context through IPC + syscall + sched. It stays speculative, but
it's tractable *here* in a way it isn't in a kernel that doesn't already narrate
itself.

## Takeaways that land early (before any sonifier code)

- A sonifier is a **glitch client**, not new device code. glitch ships first.
- The prerequisite is **cross-process span/causal propagation over IPC**, *not* the
  frame tap. Wire that and causally-correct tracing falls out for free.
- Whatever else, **rate-limit the sonifier** (gain < 1). Feedback stability is not
  optional for a thing that both hears and speaks.

## References

- [vf2-audio-design.md](vf2-audio-design.md) — the audio arc; sonification is its
  observability-output lens.
- [../plans/glitch.md](../plans/glitch.md) — the DAC-as-capability server a sonifier
  is a client of.
