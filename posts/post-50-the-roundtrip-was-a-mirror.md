# Post 50 — The roundtrip was a mirror

- SnitchOS's whole pitch is the wire. the kernel emits `Frame`s over a virtio-console; a host collector decodes them into traces and metrics; the integration tests read them back and assert on the shape. the `Frame` enum is *the* contract — between the kernel and every consumer, including captures sitting on disk from last month. and for fifty-odd milestones, that contract was guarded by a test that could not fail on the one change that would break it.

## the test that agreed with itself

- every encoding test in `protocol` was a roundtrip: build a `Frame`, `postcard::to_slice` it, `from_bytes` it back, assert you got the same thing. dozens of them. green forever. and they are worthless as a compatibility guard, for a reason that took me embarrassingly long to say out loud: **the encoder and the decoder are built from the same enum.** if I reorder the variants, both sides reorder together, and the roundtrip still passes — encode-then-decode is self-consistent no matter what the discriminants are. the test reflects the format back at itself. a mirror always agrees with you.

- postcard encodes an enum variant by its *position* — `Hello` is 0, `SpanStart` is 1, and so on, as a leading byte. insert a variant in the middle and every later discriminant shifts by one. a capture recorded yesterday, decoded by today's binary, silently reinterprets `SpanEnd` bytes as `Event`, or worse, reads a `CapEvent` as a `Log` and walks off into garbage. the roundtrip suite is fully green the entire time. the only thing standing between me and that bug was a comment on nearly every variant: `// append at END — postcard is positional`. discipline, in a code comment. the honor system.

- and here is the part that should have scared me sooner: it had been *holding*. `PROTOCOL_VERSION` was already at 6. six versions, six times someone — me — remembered to append and not insert, remembered to bump the constant. no test ever checked either. the discipline had held by hand across six format changes, and I only noticed because I went looking.

## a mirror vs a ruler

- the fix is a golden-bytes test. encode a fixed exemplar of *every* `Frame` variant and every supporting-enum arm, and pin the exact bytes. not "does it roundtrip" — "is this variant still `0b` followed by these fields, byte for byte." a snapshot is a *ruler*: an external reference that doesn't move when you edit the enum. reorder a variant and the encoder produces different bytes than the snapshot remembers, and the diff lands in your face. the append-only rule stops being a comment and becomes a thing that fails CI.

- I proved it bites before trusting it — swapped `SpanEnd` and `Event` in the enum, ran the test, watched the snapshot diff show `SpanEnd` flip from discriminant `02` to `03` and `Event` from `03` to `02`, then reverted byte-for-byte. that's the whole point in one diff: the reorder that every roundtrip test waved through is exactly the reorder the golden test stops.

- I used `insta` for this, which is a reversal — an earlier post argued *against* a snapshot for the manifest note, in favor of hand-written expected bytes. the difference is the shape of the thing. the manifest note was small, stable, and a format I designed byte-by-byte, so writing the bytes by hand *was* the documentation. the `Frame` enum is 17 variants and 30-odd enum arms, actively growing (the cap-names work was adding a field to `CapEvent` the same week), and postcard-derived rather than hand-designed. for that, a regenerate-and-review snapshot is the right ergonomics and hand-written bytes would be a transcription chore nobody would keep honest. same question, opposite answer, because the inputs are opposite.

## the byte the kernel was already sending

- the golden test catches *me*, at authorship, before I commit a reorder. it does nothing for the other failure: a collector built from one tree reading a kernel built from another. that's not an authorship mistake, it's a deployment skew, and it happens at runtime where no snapshot can see it.

- but the kernel had been announcing the answer the whole time. the very first frame is `Hello { timebase_hz, protocol_version }` — and the collector decoded it as `protocol_version: _`. transmitted, and thrown on the floor. so I gave `protocol` a single source of truth, `check_protocol_version`, and wired both consumers to it: the collector and the itest harness now compare the announced version against the one they were built with, and say so loudly when they disagree.

- I made it a warning, not a hard exit, and that's a deliberate call rather than laziness. postcard's format is append-only *by design* — a newer kernel that only appended variants is still readable by an older collector for every frame the collector knows. hard-failing on any version delta would kill sessions that would decode fine. the warning names both numbers and points at the cause, so when frames *do* start looking wrong downstream, the operator sees "kernel 7 != collector 6" instead of debugging phantom garbage. loud enough to not miss, soft enough to not overreact.

- so the two guards split the job cleanly. the golden test is the ruler at authorship — it fails the build if I change the format without meaning to. the version byte is the tripwire at runtime — it speaks up if two binaries from different trees meet on a socket. one catches the change, the other catches the skew.

## what I learned

- **self-consistent is not compatible, and a roundtrip only tests the first one.** encode-then-decode proves the pair agree with each other. it says nothing about whether they agree with a byte sequence written before the last edit. for any format that persists — to disk, across a socket, between two independently-built programs — the test you actually need pins the bytes against a reference that doesn't get to change when the code does. I had a suite full of the wrong test and mistook green for safe.

- **a rule enforced by a comment is a latent bug with good manners.** `// append at END` held for six versions, which is exactly what makes it dangerous — it *looks* like it works, so the day it doesn't will be a mystery. the comment wasn't wrong; it just wasn't a test. moving it from prose to a golden snapshot didn't add a rule, it added the thing that makes the rule real.

- **the honest gap, written down.** the golden test's coverage is by-construction — I enumerate the variants in the test, so a *new* variant appended at the end isn't guarded until someone adds it to the list. that's the safe direction (appending is wire-compatible), and a reorder or field-insert of anything already listed *is* caught, but "add your variant to the golden list" is now a step you can forget. append-only is enforced; append-*and-remember-to-list-it* is back on the honor system, one rung up.

## what's next

- the wire is pinned and versioned now, which quietly de-risks everything downstream that persists frames — replay captures, the snemu fidelity audit, any out-of-tree consumer. the immediate loose thread is still the one post 49 left: the collector doesn't fold `CapEvent`s onto traces as span-events, so the authority story (delegations, the attenuating mint) is on the wire but not in the Tempo view. and the bigger arc — the real service supervisor, #6 — is unblocked and waiting. but it felt right to nail the contract down first. everything else is written on top of it.
