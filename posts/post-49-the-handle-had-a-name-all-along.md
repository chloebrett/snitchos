# Post 49 — The handle had a name all along

- for six milestones, a spawned program found its authority by counting. the kernel handed a child a fixed array of capabilities, and the child knew — by convention, in a comment — that slot 0 was telemetry, slot 1 was its span sink, and slot 2 was "the endpoint my parent meant for me." `delegated_handle(0)` was the fs endpoint because everyone agreed it was. this post is about deleting that convention, and the surprise was how little it cost: no new syscall, no new kernel data structure, nothing copied into the child at boot. the name was already in the binary. we just hadn't asked it out loud.

## the positional contract

- here is the thing a positional startup ABI can't do: fail loudly. `let fs = Endpoint::from_raw_handle(delegated_handle(0))` compiles whether or not handle 0 is an fs endpoint, whether or not the parent delegated anything at all, whether or not the child even wanted an fs endpoint. the contract lives in two places — the parent's delegate array and the child's index literal — and nothing checks that they agree. they agree because I wrote both sides the same afternoon. that's not a contract, it's a coincidence I kept re-committing.

- the redesign question was: what would a *typed, named* startup ABI look like, where a program declares "i need an endpoint called `fs` with `SEND` rights" and something grants exactly that or refuses out loud? the obvious shape is a whole new mechanism — a BootInfo page the kernel fills in and maps into the child, a manifest the kernel parses, a registry the kernel consults. all of it kernel work, all of it new surface for authority to leak through.

- none of that got built. the child already carries everything needed.

## manifest-as-index

- a program's authority requirements already live in its ELF. `#[entry(needs = [("fs", ENDPOINT, SEND)])]` emits a `hitch` manifest into a `.snitch.iface` note — that's the thing a satisfier reads to decide what to grant. but the same macro emits a *second* artifact into the binary: `__SNITCH_SLOTS`, a name-to-index table in declaration order. `("fs", …)` is the first `needs` entry, so `fs` is index 0.

- so the child knows its own layout. it declared the slots; it knows their order. the runtime registers that table at startup, and `bootstrap().get::<Endpoint>("fs")` becomes a local lookup: find `fs` in the slots table, get index 0, return `delegated_handle(0)`. the positional access is still there underneath — but nobody writes the index anymore. you write the name, and the name resolves against a table the *same program* emitted.

- and the parent side needs no new mechanism either. a satisfier reads the child's `needs` (off the note, lifted to a `user.iface` xattr by the FS at seed time), decides what to grant, and delegates its caps in slot order through the **unchanged** `Spawn` handle array. slot 0 of the array lands at `delegated_handle(0)` lands at `bootstrap().get("fs")`. the array was always positional; the manifest is just an index into it that both sides can name. "manifest-as-index" — the kernel stays completely blind to any of this. it copies a handle array into a child, exactly as it did before v0.7.

## the satisfier

- with the child's needs readable as data, the grant step becomes a pure function: `hitch::satisfy(needs, have)`. it walks each declared slot, finds a held capability of the right object kind whose rights cover the need, and produces a plan — `Grant::Use` when the held cap matches exactly, `Grant::Mint` when the held cap is *wider* than asked and must be attenuated down, or `Unsatisfied` when nothing covers the slot. all-or-nothing: one unsatisfiable slot refuses the whole spawn. no partial grants, no "well, it got most of what it needed."

- the satisfier itself is ~120 lines of userspace. it reads a child's ELF off the FS, reads its manifest off the xattr, runs `satisfy` against the caps it holds, brackets each grant in a `satisfy.<role>` span, and `SpawnImage`s the child with the assembled handle array. the delegation is *data-driven* — read from the child's manifest, not hardcoded — and every decision is a frame on the wire.

## three grants on one boot

- to prove all three outcomes I gave one satisfier three children and one cap: it holds `MINT | SEND` on the fs endpoint.

- **fs-warden** declares `needs = [("fs", ENDPOINT, MINT | SEND)]` — exactly what the satisfier holds. `satisfy` returns `Use`: delegate the wide cap as-is. warden attaches, reads, emits its marker. an exact match is a copy.

- **fs-probe** declares `needs = [("fs", ENDPOINT, SEND)]` — narrower. the satisfier holds `MINT | SEND`, the child asked for `SEND`, so `satisfy` returns `Mint`, and the satisfier `MintBadged`s a fresh `SEND` cap — dropping `MINT` — before delegating. probe gets a bare send cap. it can talk to the fs; it cannot mint further. least authority, minted on demand.

- **fs-hungry** declares `needs = [("recv", ENDPOINT, RECV)]` — a right the satisfier doesn't hold. `satisfy` returns `Unsatisfied`, the satisfier refuses to spawn it, and snitches `satisfy.refused.recv` instead. the child never runs. the refusal is a named event, not a silent absence.

- in Tempo that's `satisfy.fs` and `satisfy.refused.recv`, nested in the boot trace among the `fs.serve` spans the satisfier generated *reading those three manifests*. the authority decisions and the work that informed them, on one timeline.

## the attenuation is on the wire — but not where I expected

- the `Mint` is the interesting one, and it produces the cleanest evidence: when the satisfier mints the narrowed cap, the kernel fires `CapEvent::Transferred { object: Endpoint, name: "fs", rights: 0b10, parent_cap_id: … }`. `0b10` is `SEND` alone. the `parent_cap_id` links back to the satisfier's `MINT | SEND` holding. so the wire literally shows a wide cap deriving a narrow one — the attenuation, as a fact, with the lineage attached.

- but here's what I got wrong when I went to screenshot it: that `CapEvent` is *not* in the Tempo trace. the collector turns spans into OTLP spans, but it doesn't attach `CapEvent`s as span events — the trace has zero events on it. the attenuation lives in the frame stream, not the trace tree. so the story needs two lenses: the trace shows the *named grants* (`satisfy.fs`, `satisfy.refused.recv`), and the frame log shows the *rights narrowing*. that split is itself worth noticing — two telemetry channels, and the authority-derivation detail only rides one of them. wiring the collector to fold `CapEvent`s onto the trace as events is a real next step.

## what I learned

- **the missing feature was a missing name, not a missing mechanism.** i went in expecting to build a BootInfo page and a kernel-side manifest parser. what the problem actually needed was to notice that the child already emits its own slot table, and to resolve names against it in userspace. the kernel didn't change. the wire didn't change. the `Spawn` handle array didn't change. the positional contract wasn't a gap to fill with new machinery — it was an index that hadn't been given a name yet, and naming it is free.

- **"refuse" is a first-class outcome, not an error path.** the nicest thing about `satisfy` returning `Unsatisfied` and the satisfier emitting `satisfy.refused.recv` is that a denied program looks exactly as observable as a granted one. you don't find out authority was insufficient by noticing a program didn't do anything. you see the refusal, by name, at the moment it happened.

- **the honest warts, written down while they're fresh.** the satisfier's `have` set is hardcoded — it "knows" its one bootstrap cap rather than enumerating its table via `CapList`, which a real satisfier would do. and `init` still over-holds `RECV` on the endpoint it hands the FS server, because delegation is copy-semantics and revocation is deferred. neither breaks the demo; both are the kind of thing that's invisible in six months unless it's in a post.

## what's next

- this satisfier is not a toy — it's the substrate for the thing I keep circling. supervision is capability ownership viewed twice: a supervisor owns the durable objects and grants each service its authority per incarnation, which is *exactly* what a manifest satisfier does. `init` reading a service's declared `needs` and granting them from its own caps is the same loop as this satisfier reading `fs-probe`'s needs. #6 in the redesign list — the real service supervisor — was blocked on this. it isn't anymore.

## postscript — the wire doesn't promise an order

- the increment that added the attenuation case shipped a test bug that's worth keeping. the scenario asserts two things on one boot: `snitchos.satisfy.attenuated_total ≥ 1` (the satisfier minted) and `snitchos.fs_warden.reached == 1` (the Use'd cap works). i wrote it as two sequential `wait_for`s: attenuated first, warden second.

- it failed. and the failure was a lie about the cause — the log said warden never reached, but the capture showed all three markers present: warden reached, the mint happened, probe reached. the impl was correct. the *test* was wrong: `wait_for` consumes the frame stream forward, and the satisfier processes warden before probe, so `fs_warden.reached` lands *before* `attenuated_total`. waiting for the later marker first steps the cursor past the earlier one, and then the second wait searches a stream that already flowed past what it wants.

- worse, the order isn't even fixed. which of the two lands first depends on cooperative scheduling — whether warden gets scheduled to emit its marker before the satisfier gets back on-CPU to mint for probe. so there is no correct order to assert them in. the fix was to stop assuming one: accumulate both flags in a single `wait_for` that returns true when it has seen each, in whatever order they arrive. 10/10 after that.

- same lesson as the last postscript, different ruler. the scheduler was fine; the impl was fine; the test was reading the wire in an order the wire never promised.
