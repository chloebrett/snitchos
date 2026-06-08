# Post 20 — A grep is a question, not a verdict

- post 19 ended with a `crate-audit` skill pointed at the test harness. this post points it at the two crates the kernel speaks _through_: `protocol` (the wire format) and `collector` (the host decoder). the cleanups were boring. the two findings I _didn't_ act on, and a bug I found writing the README, were not.

## the audit, two more crates

- same skill as last post. evidence-first: "unused" needs a grep with zero callers, "duplicated" needs both sites shown, honest divergence isn't debt.
- `protocol` ~640 lines, `collector` ~1400. both `publish = false`, internal-only — so "no caller == dead" actually holds, no external-API escape hatch.

## what came out of collector

- **`postcard` — declared dependency, zero direct uses.** all decoding goes through `protocol::stream`, which owns its own postcard. removed; it built clean, which _is_ the proof.
- **`fastrand` — a whole crate for one call:** 16 random bytes for an OTLP `trace_id`. but the id needs uniqueness-per-run, not entropy — `SystemTime` nanos already give that. dropped the dep.
- **a dead accessor with a lying excuse.** `State::timebase_hz()` carried `#[allow(dead_code, reason = "…fires only in the lib build")]`. there is no lib build — `collector` is a binary. its only callers were two tests that existed to test the accessor. deleted all three.
- **two stale docs.** module header still said "`--otlp` and `--prometheus` are stubs that print 'not yet implemented'." they're 400 lines of working exporter now. comment frozen at the moment before the work happened.
- plus the dedup tidy: URL-suffix joining lived copy-pasted in both HTTP exporters → one `url::ensure_suffix`, tested once.
- none of it interesting. it's the texture of a codebase that moved faster than its comments. 43 → still-green tests, clippy clean.

## the two I left alone

- the grep flagged two things in `protocol` as dead. both times it was wrong — not because the grep lied, but because a grep can't see intent.
- **`Frame::Event` has no producer.** kernel never emits it, no commit constructs it. by the audit's own rule, a delete candidate. but `observability-design.md` ("decisions locked") says: _three primitives Span/Event/Metric, profiling rides on Event, all seven frame types defined now, kernel uses five._ it's a **reserved slot**, defined ahead of its producer so profiling needs no wire change later.
- **same for `SwitchReason::{Preempt, Blocked, Exit}`** — reserved for a preempting/blocking/exiting scheduler the cooperative one isn't yet.
- so I taught the skill the rule it was missing. new operating principle, now in the audit skill: **"no in-repo producer" ≠ dead for contract surface.** wire formats / ABIs / protocol enums are routinely defined complete and ahead of consumers. grep the design docs first; a grep miss is a prompt to read the design, never the verdict.
- a grep answers "who calls this." it does _not_ answer "should this exist." confuse them and you delete the future.

## the bug the README extracted

- then I sat down to write the crate READMEs — the _why_, the surprising bits. and writing "here's how the framing works" honestly means actually knowing how the framing works.
- `protocol`'s module doc, line one, since forever: _"Postcard-encoded `Frame` enum, length-prefixed on the wire."_
- went to document it, traced the decoder: `take_from_bytes`, never reads a length. traced the kernel send path: copies the encoded frame to a staging buffer and ships it, no prefix written. **there is no length prefix.** there never was. four crates read past that sentence without blinking.
- what actually happens — and what the README now explains: postcard is **self-delimiting**, so the _schema is the framing_. read the variant discriminant → it tells you exactly which fields follow → consume each by its type's rule (integers self-terminating varints, strings carry their own length, fixed scalars fixed). last field consumed → frame done. length is _discovered by parsing_, never declared.
- `SpanEnd { id: 511, t: 1234 }` is 5 bytes and the decoder knows that only because it knows what a `SpanEnd` is.
- which is _why_ the append-only-variants rule is load-bearing, not bureaucracy: there's no length field to resync on, so a desync silently misreads the next bytes against the wrong schema instead of failing cleanly. the doc that claimed a prefix was hiding the one fact that explains the whole design. fixed the line.
- I'd never have caught it auditing. "length-prefixed" has a real noun and reads confident; nothing greps as wrong. it took _explaining the system to someone who doesn't know it_ to notice the explanation didn't match the machine.

## what i learned

- **a grep is a question, not a verdict.** "who calls this" and "should this exist" are different questions. for ordinary code they're the same; for a wire format, an ABI, a public schema, they're opposites — the thing with no caller is often the thing you promised the future.
- **writing the README is a verification pass.** you cannot write an honest explanation of a thing you haven't re-derived, and re-deriving is where the false comment dies. the doc that's lying surfaces the moment you have to teach from it.
- **the artifact that catches the error is rarely the one aimed at finding errors.** the flake hunt needed a histogram; the lock needed a variance column; the wire-format bug needed a README.
- **write the rule down, not just the fix.** the "contract surface" lesson is in the skill now, so the next crate gets it for free instead of me re-learning it once.

## what's next

- the reserved-but-unwired frames (`Event`; `ContextSwitch`/`HartRegister` on the collector side) are decoded and dropped on the floor today. when profiling lands, `Event` graduates — and that's the post where the reserved slot pays for itself.
- READMEs exist for `protocol` and `collector` now. the other crates don't have them. same exercise, and it'll probably find the next stale comment the same way this one did.
