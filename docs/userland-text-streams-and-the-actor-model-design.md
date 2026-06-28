# The SnitchOS userland — text streams, capabilities, and the actor model

**Status:** **Design / exploration (captured 2026-06-28). Pre-implementation.**
Records a long design discussion that started from a small question — "what are
the *useful userspace programs*, the GNU of SnitchOS, that need only pure I/O?" —
and pulled a thread all the way to "what is the shell's worldview, and does
Stitch-as-shell motivate an **actor model**?" It sits beside and extends the two
shell docs and the language design:

- [shell-surface-and-tui-design.md](shell-surface-and-tui-design.md) — the *identity*
  (powerbox "grant, then watch"; `hold`/`view`/`watch`; the ANSI TUI).
- [shell-primitives-design.md](shell-primitives-design.md) — the *primitive contract*
  (native surface → syscalls; the four-verb first iteration).
- [fs-executables-design.md](fs-executables-design.md) — programs as files in the FS.
- [language-design.md](language-design.md) — Stitch: `uses` capabilities-as-effects,
  telemetry-as-language, the parked actor model, the algebraic-effects north star.
- [software-on-snitchos.md](software-on-snitchos.md) — the difficulty ladder; the
  "self-referential tooling" and "servers over clients" sweet spots.

`view` (run a viewer over a read-only file cap) and `hold` (render my cap table)
are being built separately; this doc is about *everything around them* — the model
the rest of the userland is built from.

The throughline of the whole discussion, stated once up front:

> **The Unix pipeline fuses three things SnitchOS should keep separate —
> composition, authority, and isolation. Pull them apart and the userland, the
> shell language, the cross-process format, and the actor model all fall out of
> the separation.**

---

## 1. The governing reframe — the Unix pipe conflates three concerns

`a | b` in Unix secretly bundles three independent things, because Unix *has* to:

1. **Composition** — chaining a transform onto a stream.
2. **Authority** — each stage inherits your ambient reach (it can open *any* file,
   not just the data flowing through it).
3. **Isolation** — each stage is a separate process, because C programs can't share
   a runtime and the kernel pipe buffer is the only shared medium.

SnitchOS doesn't have to fuse them, and shouldn't. The separation:

- **Composition is the language, in-process.** The shell *is* a Stitch interpreter,
  so a filter is a Stitch function over a `Seq`, not a spawned `grep`. Most
  "coreutils" become **stdlib functions**, not a `bin/` of executables.
- **Authority is the cap granted at spawn — and that is the *only* reason to cross a
  process boundary.** You spawn a separate program precisely when something must
  *touch a capability* under least-authority, so the grant is visible.
- **Isolation is a process, used only where authority (or fault-containment)
  demands it.** The process boundary should track the *authority* boundary, not the
  *composition* boundary.

This also rejects the Unix *nouns* the shell docs already reject (`ls`→`hold`,
`cat`→`view`, `ps`→`watch`) — but goes deeper, rejecting the *pipe* as the thing
that carries the worldview.

### Carry typed records, not re-stringified bytes

Unix's tragedy is re-parsing text at every stage (`ls | awk | cut | sort`). The
nushell/PowerShell answer is to pass **structured records**. For SnitchOS this is
not a bolt-on — it falls out of the "everything is observed" pillar: the telemetry
channel is *already* structured `Frame`s, and Stitch has `record`/`Seq`. So a
pipeline carries Stitch values, and `where`/`select` (structured filter +
projection) is the one general tool that replaces grep + cut + awk + sort-keys.

**The advantage over nushell is real:** nushell constantly fights to *structure
unstructured* OS output (it wraps `ls`/`ps`/`df` and parses their text). SnitchOS's
OS output is **born structured** — caps, spans, frames, metrics are already
records. We skip the parse-and-pray step; the pipeline's source is already typed.

---

## 2. Composition: `|>`, lazy `Seq`, and the three-way collapse

**`|>` already ships and is lazy.** Stitch's natives are deliberately *data-first*
("collection-first so it pipes: `seq |> take(3)`" is in the source), and sequences
are `LazySeq` with demand-driven `force`. `iterate(0, …) |> take(3)` terminating is
the proof — demand-driven, backpressure-by-forcing. That is Unix pipe semantics
(lazy producer, bounded consumer) **in-language and over typed values**, with no
process per stage.

So the shell pipeline and the Stitch expression are the **same construct**. This is
the third instance of a collapse the shell-primitives doc already celebrates:

| shell needs | bash builds a bespoke mechanism | Stitch *reuses* a language feature |
|---|---|---|
| decide which command, run it | a command-dispatch table | **`on X` dispatch** (parse `"view notes"` → `View("notes")`, `on View { run() }`) |
| repeat | a builtin REPL loop | **recursion** (Stitch's only iteration) |
| pipe stages | the `\|` operator + kernel buffer | **`\|>`** (lazy, typed, in-process) |

Three core shell mechanisms; zero of them new. The shell's structure *is* the
language's structure — the concrete form of "Stitch from day one."

---

## 3. The "GNU of SnitchOS" — a shortlist, split by the authority boundary

Because `|>` is lazy and typed, most coreutils are *already written* — they are the
existing `Seq`/`Str` natives. The remaining work splits by which side of the
authority boundary a tool lands on (§5 makes this rule precise).

**Library functions** (no new mechanism, no process, ship as a `Text`/`Seq` stdlib;
run with the shell's authority):

- `count` — over *records*: count lines, or caps-by-kind, or spans. (fold)
- `where` / `select` — structured filter + projection. **The flagship**; subsumes
  grep + cut + awk + sort-keys. Ergonomic today via implicit lambda params:
  `where($a.rights contains "w")`.
- `uniq`, `nl` (enumerate), `rev`, `head`/`tail` (take/drop), `tr`/`fmt`/wrap.
- `sort`/`sortBy` — already natives; expose as verbs.
- `calc`/`bc` — a pure expression evaluator.
- `seq`/range — generates a `Seq` to feed pipelines.

**Spawnable programs** (cross the authority boundary — each needs a granted cap;
each is a file in the FS per fs-executables; can be `.st` or a Rust ELF):

- `feed`/`cat`-equivalent — read a granted file cap → console. The un-rendered
  sibling of `view`.
- `json` — read a file cap, parse → Stitch record, pretty-print. Output flows
  straight into `where`/`select`.
- `hexdump`/`xxd` — bytes → text.
- `frames`/`decode` — read a postcard `Frame` dump and pretty-print it. **Eats the
  OS's own wire format** — the most on-theme pure-I/O tool possible, doubling as
  debugging tooling.
- `tee` — in cap terms, "fan a stream cap to two readers" — observable fan-out.

**The most SnitchOS-flavored tools are the self-referential ones** (`frames`,
`count` over spans/caps, `where` over cap records): they consume the structured
data the OS already emits, and are cheaper here than anywhere else.

---

## 4. nushell-style structured rendering — the consequence of §1, not a feature

Once the pipe carries typed values, nushell stops being an option and becomes the
natural rendering: the default presentation of a structured value is a **table**.

**The REPL's result-printer *is* the renderer**, dispatched on Value shape — *not* a
`| table` verb you invoke:

- `Seq<record>` of one type → **table** (columns = field names, rows = records)
- a single `record` → key/value table
- a `sum`/nested/tree → **tree** (`term.tree`)
- a scalar → itself

This is feasible from today's data model: `Value::Data { type_name, variant,
fields }` carries field names at runtime, so a homogeneous `Seq<record>` reflects
directly into columns. It is the pure, host-testable **Tier-0 render lib** the
surface doc already wants (model → escape bytes, snapshot-tested, no QEMU). `hold`,
`frames`, `where caps`, `select` then render as tables for free.

Disciplines:

1. **Take the rendering + structured pipe + cell-path access; leave the rest.**
   nushell is huge (`$env`, closures-as-data, a giant stdlib). We already have the
   structured pipe and cell-paths (`$a.rights`); add only auto-table-render.
2. **Tables are a UART/human-terminal thing**, not the virtio telemetry stream (the
   surface doc's channel split). Semantic color stays — rights green/amber, exited
   dim.

---

## 5. The two-layer authority model — and what it does to `view`

`uses` is **implemented and enforced today** (dynamically): the authority set is
threaded down the call graph (`with_authority`), and natives refuse without it —
`print requires uses ConsoleOut`, `span requires uses Telemetry`, `readLine
requires uses ConsoleIn`. So a *function* is not stuck with the shell's full
authority — it can declare and be bounded to less.

That gives **two enforcement strengths of the same idea**:

| | `uses` row (in-process) | spawned program (process) |
|---|---|---|
| authority | declared, bounded, **visible in the row** | declared, bounded, **visible as a `CapEvent`** |
| boundary | **soft** — VM-enforced; a VM bug voids it | **hard** — kernel-enforced; survives a VM bug, separate address space |
| isolation | shares the shell's process; a crash takes the shell down | crash-isolated, separately scheduled |
| language | Stitch only | Stitch **or** a Rust ELF |
| cost | a function call | a spawn |

So `view` is no longer "must be a process." It is:

- **`view(f) uses FsRead, ConsoleOut`** — when you trust the viewer code (yours,
  Stitch). Cheap; authority is right there in the row; the trace still shows it.
- **`spawn(view, [readCap(f)])`** — when the viewer is *untrusted* (third-party, or
  a Rust binary) or you want the grant to be a real kernel `CapEvent` that holds
  *even if the interpreter is compromised*.

**The key unification:** the `uses` row and the spawn cap-set describe the *same*
least-authority intent at two strengths. A stage can be **promoted** from a
`uses`-bounded function to a spawned process when distrust or fault-isolation
demands it — the authority *description* doesn't change, only who enforces it.
"Function or process" becomes "**pick the enforcement strength the trust
relationship needs**," not a syntactic accident.

**Honest caveats (carried, not solved):**

- Today's named authorities are only `{Telemetry, ConsoleIn, ConsoleOut}`. An
  FS/file-read authority would be *added* the same way; it isn't there yet.
- Today `uses` is a **name-set check** (`with_authority(HashSet<String>)`), not yet
  an unforgeable cap *value* threaded in (that is the affine/linear north star).
  So the soft layer is "declared effect *names*, checked dynamically"; the hard,
  unforgeable *token* lives at the kernel edge — which is exactly why the hard
  boundary still matters.

---

## 6. Function vs process: relocatability, "isolated", "marshallable"

The provocative idea raised: *could any Stitch function run as a separate kernel
process with kernel-enforced cap boundaries* — a "secure mode" where every function
only receives the caps it declares?

**Verdict:** "**any** function, **automatically**, **every** call" is unworkable.
The sound version is "**selected** functions, **opt-in**, that the compiler can
**prove are relocatable**, with the `uses` row as the kernel cap-set."

### Why "any function automatically" breaks

1. **Closures capture outer scope, and code/heap pointers can't cross an
   address-space boundary.** A lambda closes over free variables — data, *other
   functions*, lazy thunks. A captured *function* can't be shipped by reference;
   you'd have to ship its AST or RPC back per call. **Higher-order functions don't
   cross process boundaries cleanly.**
2. **Spawn cost is ~microseconds; a call is ~nanoseconds.** Per-call spawning inside
   a `map` over 10k elements is a 1000×+ blowup. The boundary must be coarse and
   chosen.
3. **The boundary forces eager + reintroduces partial failure.** Serializing a lazy
   seq forces it (lose streaming). A process can crash/OOM/hang independently, so a
   "function call" silently becomes async + fallible (Waldo et al.,
   *A Note on Distributed Computing* — distribution is **not** transparent).

### Pure functions rescue it — with one refinement

If a function is **pure** (no effects), **non-capturing** (closes over nothing but
its parameters), and has **marshallable** I/O (`Value`s in, `Value`s out — no
thunks, no function args), all three blockers vanish — and the function is
**referentially transparent**, so relocating it is *semantically invisible* (the one
case where distribution transparency genuinely holds).

The refinement: **pure ≠ relocatable.** Relocatable = pure **and** non-capturing
**and** marshallable. And purity is nearly free to detect because **`uses` already
classifies it**: an empty `uses` row means no effects. The new mechanism is the
**capture/closure check** layered on top of an empty effect row. (Terms adopted:
**isolated** for the relocated unit — echoing Pony's `iso` — and **marshallable**
for serializable-to-cross.)

### Pure-as-process and `uses`-as-process buy *different* things

- A **pure** function has no caps — it can't abuse authority *by definition*. So
  isolating it isn't about authority; it buys **resource containment** (preempt /
  memory-cap an infinite loop or OOM), **fault isolation**, and — the strongest
  positive — **free parallelism** (pure + isolated = no possible data race → run it
  on another hart). Worth it for *untrusted or expensive* pure code.
- An **impure `uses`** function is where authority confinement matters: run it as a
  process the kernel hands *exactly* its declared caps, and even a VM exploit can't
  exceed them. **That** is the security win — and it is exactly `view`.

---

## 7. The cross-process format — a data model, two encodings (superseded)

When data crosses a process boundary (only then — in-process `|>` passes `Value`s
directly, zero serialization), it must be serialized. The earlier call here — "a
single *tagged postcard* `Value` that is at once compact, language-neutral, **and**
generically decodable without a schema" — was **wrong, and the error is
instructive:**

- postcard is **positional** (like `repr(C)`): the bytes carry no field names, so
  decoding needs the type definition. A **type tag identifies *which* type — a
  discriminant — not its *shape*.** So a tag buys disambiguation, *not* a generic
  decoder; a consumer still needs a per-type mapping to attach field names.
- Therefore **compact-positional** and **self-describing** are *two encodings of one
  data model*, not one format. You can have positional bytes a Rust `struct` reads
  directly (shared type required, *not* generic), **or** a self-describing blob any
  consumer decodes into a named record (`DataValue` already carries
  `Vec<(Option<String>, Value)>`, so encoding the *`Value`* is self-describing) —
  **not both at once.** The original §7 implied both.

The resolution — **declare the schema once, in each program's typed interface, so
payloads stay compact *and* generic decoding works** — together with the unified
data model and where the schema is stored, is its own design:
[typed-processes-and-the-data-model-design.md](typed-processes-and-the-data-model-design.md),
which **supersedes this section**.

`|>` still has **two lowerings**: RHS is a Function → pass `Value`s in-process; RHS
is a Program → serialize across an Endpoint (encoding per that doc).

---

## 8. The keystone: **placement follows authority**

Consider `data |> clean ~> analyze |> render` (`~>` = a cross-process hop, §10).
*Where does each stage run?* Two candidate semantics:

- **(a) Segmented** — `~>` moves the pipeline to a new process; subsequent `|>` stay
  there. → `{data, clean}` here; `{analyze, render}` in process B. Efficient for
  long chains, but "where does render run?" needs a left-scan (non-local reading).
- **(b) Per-stage isolation, result returns home** — `~>` isolates *just* the next
  stage; its result flows back; following `|>` run locally again. → `{data, clean,
  render}` here; `{analyze}` in B. Local, predictable.

**Capabilities break the tie toward (b).** `render` needs the `ConsoleOut` cap,
which the shell holds and `analyze`'s process — spawned for pure computation, not
granted the console — does not. Under (a), `render` in B would be a *compile error*
(B lacks the cap). Under (b), `render` returns home where `ConsoleOut` lives. So the
cap a stage needs *constrains where it can run*. The general principle:

> **A stage's `uses` row determines its legal process placements.** A *pure* stage
> (no caps) can be isolated anywhere — relocate it freely. An *effectful* stage must
> run where its cap lives: a `ConsoleOut` stage in the shell, a `FsRead(notes)`
> stage where that file cap was delegated.

Isolation placement is **not arbitrary — it is derived from authority.** The
compiler can check (even infer) placement from `uses`; `~>` is legal exactly when
the stage to its right is *relocatable* (the §6 check). This is where caps,
isolation, and the pipe operators unify under one rule — the keystone of the whole
design.

**What can and can't be expressed:**

- Any subset of stages isolated in a **linear pipeline**: yes — any *contiguous
  segmentation*. `a |> b ~> c |> d ~> e` is fine.
- But *which* stages may be isolated is constrained by caps (above) — and that's a
  feature, not a limit.
- **Arbitrary process *topologies*** — fan-out, fan-in, non-adjacent stages sharing
  a process/state — **no.** Pure pipe operators express only same/different relative
  to the *adjacent* stage. Real graphs graduate to **named actors + explicit
  Endpoints** (the heavier tier). Clean boundary: `|>`/`~>` for linear dataflow (the
  90% case, ergonomic and visible); named actors when you need topology.
- **REPL pipelines block-and-return** (you're waiting for the table) → synchronous
  isolation, **no promise**. Promises appear only when you *background* a pipeline
  (fire-and-collect). Everyday case stays promise-free.

---

## 9. Is the actor model the right fit?

Every thread above keeps landing on the same shape — *a capability-secure,
observable unit you run in isolation that communicates by serialized typed
messages*. That is an **actor**. The language-design doc already parks the actor
model, deferring it because (a) it needed IPC and (b) it's a whole-language identity
commitment. **IPC is now shipped.**

**Right fit for *which* parts.** Actors fit the *concurrent, fault-prone* parts —
pipelines, background jobs, the live `watch` pane, supervised children. They do
**not** fit the sequential REPL **spine** (read line → dispatch → show). So the
shape is: **the spine stays plain Stitch; the things it isolates become actors.**

**Two caveats that decide *which kind* of actor language this can be:**

- **SnitchOS actors are heavyweight.** An actor here is a kernel process (page
  table, stacks, cap table) — nothing like Erlang's millions of green processes.
  MAX_HARTS = 2, tree-walk interpreter. So you get **few coarse actors** (dozens),
  landing near **E's vats** or a classic OS-process actor model, **not** the BEAM.
  This *fits* the "coarse isolation boundary" conclusion.
- **It's a whole-language identity commitment.** Don't *declare* Stitch an actor
  language; let `~>` + Endpoints *be* an actor substrate, and see if the shell's
  concurrency genuinely wants it before committing the identity.

**The novel, on-theme win nobody else has:** actor authority = capabilities, and
every message is already a traceable IPC frame. That is the E-lineage caps+actors
fusion *plus* the SnitchOS observability pillar, for free.

---

## 10. Prior art — the actor-language map

Origin: **Carl Hewitt (1973)** posed the actor model; **Gul Agha (1986)**
formalized it (on a message, an actor may send messages, spawn actors, change its
next-message behavior; share-nothing, async). Descendants that map to SnitchOS:

| Language | The idea | Relevance to SnitchOS |
|---|---|---|
| **E** (Mark Miller) — *not Erlang* | **Object-capabilities + actors.** "Vats" = event loop + heap (≈ a process). Sync calls within a vat; `obj <- msg()` **eventual send** across vats returns a **promise**; **promise pipelining** cuts round-trips. **CapTP** = caps over the wire. | **The closest prior art** — it *is* caps+actors, SnitchOS's exact pair. Vats ≈ processes; eventual-send ≈ Endpoint message; CapTP ≈ cap-transfer-on-reply (shipped). Promise pipelining answers the per-element latency of streaming. Read Miller's thesis *Robust Composition*. |
| **Pony** | Actors + **reference capabilities** (`iso`, `val`, `ref`, `box`, `tag`, `trn`) — a *type system* that statically proves data-race freedom and decides **what may cross a boundary**. `iso` = isolated/unique (sent by move); `val` = immutable (shared freely). | **The rigorous version of "isolated" and "marshallable."** `iso` is literally our isolated unit; `val` is "immutable Stitch value, copy-free." Pony *proves at compile time* the relocatability check of §6. |
| **Erlang/OTP** (Armstrong, Ericsson) | Millions of cheap green processes, share-nothing, async + selective receive, mailboxes, links/monitors, **supervision trees**, "**let it crash**," hot code reload. BEAM VM, per-process GC. | Supervision trees ≈ `init` + `Wait`/reap (you have the supervisor); "let it crash" ≈ process fault-isolation. **Granularity does not transfer** (kernel-weight actors) — take the *patterns*, not the *scale*. |
| **Elixir** | Erlang's model with a modern, Ruby-flavored syntax + macros + tooling (mix/hex/Phoenix) on the **identical BEAM VM**. | Same runtime/actors/OTP, friendlier surface. Erlang vs Elixir = same VM, different skin. |
| **Akka** (Scala) | Erlang's model as a library; Akka Typed was a retrofit. | Cautionary: put the actor contract in the *types* (`uses` + relocatability), not an untyped library. |
| **AmbientTalk / Unison** | AmbientTalk: distributed actors. Unison: content-addressed code + **abilities** (= algebraic effects) + ship-code-to-data. | Unison's abilities ≈ Stitch's `uses`; its distributed execution is the network version of relocating an isolated stage. |
| **Go / CSP** (Hoare) — *contrast* | Communicate over **named channels** decoupled from process identity (often rendezvous), not actor mailboxes. | SnitchOS Endpoints are channel-like (multiple senders via minted/badged SEND caps) but process-per-endpoint is actor-like. The unusual, on-theme bit: **capability-secure channels** — neither Erlang nor Go has that. |

---

## 11. Lazy streams across the boundary *are* an actor protocol

Streaming a lazy `Seq` one element at a time to another process is fine and
laziness-preserving (demand-driven pull = backpressure) — but it **converts a lazy
`Seq` from a language primitive into a streaming protocol**, which must answer four
things the in-process `Seq` got for free:

1. **Explicit backpressure** — a pushing producer floods the consumer's mailbox
   (the classic unbounded-mailbox bug). Needs demand signaling / credit / windowing
   (reactive-streams, Erlang `{active, once}`). "Force on demand" becomes "request N."
2. **End-of-stream + failure as messages** — distinguish "done" / "more" / "producer
   died." In-process that's `Step::Nil` vs a panic; across, it's protocol design.
3. **Per-element IPC is slow** — a million elements = a million hops → **batch**
   (chunk), reintroducing a latency/throughput knob. (E's **promise pipelining** is
   the latency answer.)
4. **Order/identity** — now a property you depend on the channel to preserve.

So: **eager boundary = function relocation** (copy a materialized `val`,
transparent); **lazy boundary = an actor** (a streaming mailbox with flow control).
Both fine, different mechanisms — and the lazy one *is* the actor model, confirming
the direction. The compiler picks the lowering by whether the boundary's type is a
`val` (copy) or a `Seq` (stream).

---

## 12. Syntax: what this direction earns, and what it doesn't

The core grammar holds — `prod`/`sum`, `|>`, `uses`, `on`, `->`=maps-to /
`=>`=case / `|`=alternation / `?`/`?.`. The direction earns a small, surgical delta;
resist a rewrite.

- **`~>` — the cross-process pipe (the one genuinely new operator).** `|>` =
  in-process; `~>` = a stage crosses a process boundary. It marks the **exact**
  stage that crosses — more precise than wrapping a region — which is the whole
  SnitchOS "make the boundary visible" move. **`~>` beats `||>`** because the
  language reserves `|` for **alternation, always**; `||>` would overload it,
  breaking a stated invariant. `~` is a fresh glyph whose "loose/approximate"
  connotation fits a fallible, async edge. Keep a `~`-family for *all* boundary
  crossings (so an explicit actor send stays visually unified).
- **`iso { block }` — optional, mostly subsumed by `~>`.** A three-letter keyword
  matching `ext`/`use`/`mut`, echoing Pony's `iso`. Only earns its place for
  **input-less** isolation (no left operand for `~>` to attach to). Don't build it
  first. (Implemented as spawn-and-join — *not* an effect handler — so it needs no
  continuation machinery.)
- **Visible cross-process send.** SnitchOS's "boundaries are visible" ethos *argues
  for* making a boundary-crossing send syntactically distinct (E-style), against
  hiding it behind a normal-looking call. Note `<-` is **already taken** (`use <-`
  for telemetry scoping), so a send glyph needs another form in the `~` family.
- **Promises / streams — no new syntax.** Stdlib types + effect-row entries. Promise
  *pipelining* (E) is a later optimization.
- **Implicit lambda param — already exists** (`where($a.rights contains "w")`).
  Nothing to add; the `where`/`select` ergonomics are already there.

**Isolation is a *placed boundary*, not an effect handler** — so it does **not**
wait on the (expensive, unbuilt) `with`-handler / continuation machinery. The real
`with`-handlers stay parked for genuine effects (scheduler, local suspension).

---

## 13. PL background — coloring, CPS, continuations, algebraic effects

The conceptual underpinning for "no coloring," `uses`, and the parked `with`
machinery. Recorded so the reasoning survives.

### Function coloring

A "color" is two function species that don't freely compose. **JS:** an `async
function` returns `Promise<T>`; `await` is legal only inside `async`, so async
**propagates virally** up the call tree, and there is no construct that *absorbs*
it (it reaches `main`). **Kotlin:** `suspend fun` is the same color, but the
ceremony is hidden (calls look like they return `T`; no explicit `await`) via a
compiler transform. **Coloring exists because the runtime cannot snapshot an
arbitrary call stack** — so the language fakes resumption per-marked-function, and
the marker is the scar.

### CPS transform

**Continuation-Passing Style:** instead of *returning*, a function takes an extra
argument `k` (the **continuation** — "the rest of the program from here") and
*calls* it with the result. Once "what to do next" is an explicit value, you can
**store it and call it later** — which is suspension. JS callbacks are hand-written
CPS; `async/await` is the **compiler** doing the CPS transform (built on ES6
generators), producing a **heap-allocated resumable state machine** — *not*
native-stack capture. (Kotlin threads a hidden `Continuation` = the reified `k`.)

A **tree-walk interpreter cannot do this** — it runs on the *host's* (Rust's) call
stack, which it can't snapshot — the same wall JS/pre-Loom-JVM hit. The **bytecode
VM** gives Stitch its own reified stack it *can* save/restore, which is *why* the
effects/`with` machinery waits for the VM. (Wasm stack-switching / JSPI is this
capability arriving on the web.)

### "Make every function async" — no; make *none* colored

Two ways to kill the distinction:

- **(A) every function async** (one color) — uniform, but the *expensive* color
  paid everywhere: `add(1,2)` returns `Promise<Int>`, every call allocates a
  promise/state machine. This is why Kotlin made `suspend` opt-in.
- **(B) no color** — suspension is a **runtime capability, not a signature
  property**. A function is just a function; the runtime (continuations +
  scheduler) parks and resumes it. Zero colors, **and** cheap (only actual
  suspensions pay). This is **Loom / Go / Koka**.

Subtlety — **with implicit unwrap, (A) and (B) look the same to the programmer**
(one color = no color; coloring is about the *boundary*, and one color has none).
But they diverge underneath: **(A) taxes every call and is one-shot; (B) pays only
on actual suspension and is multi-shot** (powers generators, exceptions,
backtracking — not just async). Stitch wants **(B)'s implementation** (the bytecode
VM with real continuations, already the plan) to land the no-color surface without
the tax — which is why it can have `uses`-tracked effects *and* "no coloring":
`uses` effects are the **benign, dischargeable** kind — a `with` handler **absorbs**
the effect locally, so it isn't viral-to-`main` like JS async.

### Algebraic effects = resumable exceptions (not a category-theory prerequisite)

The mechanism behind all of the above. **Split an effect into a *declaration* (an
operation, no implementation) and an *interpretation* (a handler, installed
dynamically).** When code performs the operation, control jumps to the nearest
handler, which receives the **continuation** and may **resume** it. It is exceptions
with one twist — *the handler gets the continuation* — and everything else is "what
does the handler do with it?":

| handler behavior | feature you get |
|---|---|
| never resume | exceptions |
| resume once | normal return / injection |
| resume per demand | generators / iterators |
| resume on I/O completion | async |
| resume multiple times | nondeterminism / backtracking |
| thread a value through resumes | mutable state |

One mechanism, parameterized by continuation use — exactly `uses` (declare) + `with`
(interpret) + continuations (resume), and why the doc says iteration/async/
concurrency all fall out of building it once.

**Is it category theory?** **No — not to use or understand it.** The name is from
**universal algebra** (operations + equations, the way a group is "an algebra"),
not specifically CT. Lineage: **Moggi (1991)** monads (the CT-heavy framing) →
**Plotkin & Power (~2002)** effects as algebraic theories (where "algebraic" comes
from) → **Plotkin & Pretnar (2009)** *handlers* (what languages implement). The
usable model is entirely operational: "resumable exceptions with handlers." It's
**real and shipping**, not just research: **OCaml 5** (2022) put effect handlers in
its runtime and built async I/O (Eio) on them with **no async/await coloring** —
the (B) outcome in a mainstream language. Also **Eff, Koka, Frank, Effekt**, and
Unison's "abilities."

---

## 14. Settled leanings vs open forks

**Settled (leaning):**

- The pipe conflation reframe: composition = language, authority = cap-at-spawn,
  isolation = process-only-where-authority/faults-demand.
- Typed records in the pipe; nushell-style shape-dispatched rendering as the REPL
  result-printer; coreutils mostly as stdlib functions over the existing lazy `Seq`.
- The two-layer authority model (`uses` soft / spawn hard) as the *same* intent at
  two strengths; "function vs process" = "pick the enforcement strength."
- **Placement follows authority** (the keystone): a stage's `uses` row determines
  its legal process placements.
- Cross-process format = **tagged postcard `Value` over an Endpoint**.
- The actor model fits the *concurrent/isolated* parts (heavyweight, coarse,
  E/vat-style), not the REPL spine; cap+actor fusion is the novel win.
- Syntax delta: **`~>`** (not `||>`), `iso {}` optional, visible send in the `~`
  family; isolation is a placed boundary, not an effect handler.
- Stitch targets the **(B)** no-color-via-runtime-continuations cell, via the
  bytecode VM + algebraic-effect handlers.

**Open forks (to decide before building):**

1. **Send glyph** — the `~`-family form for an explicit actor/eventual send (`<-`
   is taken). Visible (leaning) vs invisible.
2. **`iso {}` — keep or drop?** Only the input-less case isn't covered by `~>`.
3. **Auto-table render — every result, or only `Seq<record>`?** (nushell auto's
   everything; scalars/strings may want to print plain.)
4. **Pipeline placement semantics** — confirm reading **(b)** (per-stage isolation,
   result returns home) over (a) (segmented). Caps argue for (b).
5. **When does the relocatability capture-check land?** Today's `uses` is a name-set
   check; the affine/linear unforgeable-cap-value form and the non-capturing check
   are the gap between "soft name" and "real threaded cap."
6. **How far to commit to actors** — substrate-only (`~>` + Endpoints) vs a
   whole-language identity.

**Dependencies / build order (largely additive on shipped mechanism):**

- `where`/`select`/`count` + a `Text` module — pure stdlib, host-tested, no kernel
  work.
- Shape-dispatched render lib (Tier-0) — pure, snapshot-tested.
- `~>` lowering: Function (in-process `Value`) vs Program (Endpoint + tagged
  postcard `Value`); the relocatability check in the compiler.
- The hard, later items: the bytecode VM (snapshottable stack) → algebraic-effect
  handlers (`with`) → real concurrency. Promises stay *quarantined to the process
  boundary* until then.

---

## 15. References

- Waldo, Wyant, Wollrath, Kendall — *A Note on Distributed Computing* (distribution
  is not transparent; partial failure).
- Mark S. Miller — *Robust Composition* (object-capabilities, E, vats, promise
  pipelining).
- Bob Nystrom — *What Color Is Your Function?* (function coloring) and *Crafting
  Interpreters* (the tree-walk lineage Stitch is built on).
- Plotkin & Power — algebraic operations for effects; Plotkin & Pretnar — handlers
  of algebraic effects.
- Leijen — *Koka*; OCaml 5 effect handlers (Eio); Pony reference capabilities;
  Erlang/OTP; Unison abilities.
- In-repo: [language-design.md](language-design.md),
  [shell-surface-and-tui-design.md](shell-surface-and-tui-design.md),
  [shell-primitives-design.md](shell-primitives-design.md),
  [fs-executables-design.md](fs-executables-design.md),
  [ipc-design.md](ipc-design.md), [capability-system-design.md](capability-system-design.md),
  [software-on-snitchos.md](software-on-snitchos.md).
