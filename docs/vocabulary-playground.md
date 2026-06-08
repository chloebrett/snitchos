# Vocabulary Playground

> ## ⚠️ MASSIVE DISCLAIMER — READ FIRST ⚠️
>
> **This is exploratory wordplay, NOT canonical project vocabulary.**
>
> Nothing in this file is authoritative. None of these coinages have been
> adopted, agreed upon, or blessed for use in real documentation, code
> comments, commit messages, identifiers, or anywhere a reader expects
> precise established terminology. They were invented for fun in a single
> brainstorming session.
>
> **Do not** propagate these terms into `docs/`, `CLAUDE.md`, plan files,
> wire-format names, or public-facing prose on the strength of this file
> alone. If a term ever *earns* its place in real docs, that's a separate,
> deliberate decision — promote it explicitly, don't let it leak in.
>
> **Fair game:** the agent and Chloe may use this dialect freely when
> talking *to each other* in chat. It's a shared shorthand for conversation,
> nothing more. If it makes a debugging session faster or funnier, great.
>
> Some coinages overlap with terms that are *already* real and load-bearing
> in the codebase (`wedge`, `heartbeat`, `intern`, `hart`, `deflake`,
> `trampoline`, `span`). Those are genuine. The *extensions* of them here
> are not.

---

## 1. Word-formation

### Verbs (nouns put to work)
- **to snitch** — to emit a telemetry `Frame` for an event you'd otherwise just `println!`. Past: *snitched*. The OS's personality as a verb.
- **to wedge** *(intr.)* — for a hart to die silently mid-frame, socket-disconnecting. Past: *wedged*.
- **to deflake** — to drive a flake rate to zero by classification, not retry-spam.
- **to intern** — string→id deduplication; one pass is an *internment*.
- **to higher-half** — to relocate a pointer (or oneself) above `KERNEL_OFFSET`.
- **to spike** — to throw away a quick probe whose only job is to confirm a channel works (cf. commit "verified by spike").

### Portmanteaus
- **flakefile** — `.itest-baseline.toml`; exists to make flake rate diffable in PRs.
- **wedgeprint** — a failure's socket-level signature; the fingerprint that tells SIGKILL from honest silence.
- **snitchpoint** — a code site that emits a frame; telemetry's answer to a breakpoint.
- **heartmiss** — a skipped/late heartbeat; "alive but starving."
- **stagewait** — the `TX_STAGING` copy-through-a-static dance.
- **bucketblend** — the single flake rate you get by collapsing all cause-buckets (the thing the classifier *un*-blends).

### Unusual compounds
- **silence-shaped failure** — a death that looks exactly like normal idle on the wire. The founding problem the signature classifier solves.
- **alive-but-slow** — a failure mode distinct from wedged (cf. `BudgetExhausted`).
- **wire-shaped** *(adj.)* — an assertion made on decoded `Frame`s, not UART text.
- **frame-true** *(adj.)* — a fact the kernel actually snitched, vs. merely printed.
- **lock-at-the-semicolon** — the `let x = *MUTEX.lock();` early-drop bug class.
- **boot-shaped observability** — spans that start before `main` and survive the trampoline.

### Roles (nouns naming a part)
- **snitch** *(n.)* — any frame-emitting subsystem (the heap is a snitch, the scheduler is a snitch).
- **the choir** — all harts emitting heartbeats together; a **soloist** is the lone still-singing hart in a `HartStalled`.
- **deflaker** — a scenario whose job is to provoke, not assert (`deflake-spawn-storm`, `deflake-mutex-storm`).

---

## 2. Metrology (coined units)
- **a heartbeat** *(duration)* — "OOM exhausts RAM in ~8 heartbeats."
- **a grow** *(heap pressure)* — 1 MiB of watermark expansion; pressure in *grows-per-heartbeat*.
- **a tick** *(CPU attention)* — the currency tasks are paid in (`cpu_time_ticks`).
- **the snitch-rate** — frames/second on the wire.

## 3. Eponymy (incidents become proper nouns)
Convention: a confirmed-and-fixed incident earns a Capitalized Name.
- **the Staging Wedge** — the `TX_STAGING` cross-hart race.
- **the Semicolon Lock** — the dropped-guard early-release bug.
- **the Soloist** — a `HartStalled`: one hart singing while the choir died.

## 4. Taxonomy (a bestiary of failures)
The signature buckets as a Linnaean tree; genus over species.
- **Genus *Wedge*** (dead): *staging*, *semicolon*, *percpu-panic*.
- **Genus *Stall*** (alive, mute): *hart-stall*, *budget-exhausted*.
- **Genus *Phantom*** (`Unknown`): insufficient evidence — the cryptids.

## 5. Collective nouns
- **a storm of spawns** (cf. `deflake-spawn-storm`).
- **a choir of harts** — emitting heartbeats in unison.
- **a flush of frames** — the pre-init buffer draining after `Dropped(0)`.
- **a scatter of frames** — the *physical* kind: contiguous VAs, scattered PAs.
- Disambiguators for the overloaded **frame**: **wireframe** (telemetry) vs **pageframe** (memory).

## 6. Axes (name the spectrum, not just the poles)
- **truthfulness** — *frame-true* ↔ *print-only*.
- **shape** — *wire-shaped* ↔ *log-shaped*.
- **temperament** — *Wfi* ↔ *Cpu* (idle-leaning vs work-leaning; parallel vs serial run).
- **resolution** — *bucketed* ↔ *blended*.

## 7. Diagnostic mood (grammar, not vocabulary)
- **a counterfactual** *(n.)* — a run whose value is the world it rules out (`thread=single`, per-file revert sweep). Not a test; a *what-if*.
- **the cheap-to-confirm principle** — order experiments by time-to-next-decision; prefer the run you predict will fail. Privileges the *falsifying* mood over the *confirming* one.

## 8. Forensic register (the coroner's vocabulary)
- **cause of death** — the wedgeprint of a dead hart.
- **an alibi** — what *BudgetExhausted* has and *Wedge* lacks: evidence of being alive but slow.
- **time of death** — last frame timestamp before the socket dropped.
- **the corpus** — capture clustering as a case file.

SnitchOS doesn't crash, it gets *investigated*.

## 9. Periodization (boot has eras)
- **the Identity Era** — pre-trampoline, low VAs, no formatted `println!`.
- **the Crossing** — the trampoline; the instant PC goes higher-half.
- **the Linear Age** — post-`unmap_identity`, frames reachable via `pa_to_kernel_va`.

"That bug is *pre-Crossing*" locates it in boot time *and* explains why formatted printing crashes there.
