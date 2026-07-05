# Adversarial review — capability object names (2026-07-05)

*Scope: the shipped cap-names implementation (`abi` `pack_name`/`CapDesc.name`,
`kernel` `handle_endpoint_create`/`handle_cap_list`/`emit_cap_*`, `protocol`
`CapEvent.name`) against `docs/capability-names-design.md`. Read-only; the feature
is "mostly implemented, iterating." Confidence marked per finding.*

The design is strong and unusually self-aware — it pre-answers the obvious attacks:
impersonation is correctly framed as **phishing, not authority bypass** ("trust
keys on provenance, never the name"), non-endpoint objects are deliberately scoped
out (their kinds self-describe), and the wire cost is consciously bounded. The
"names for seeing, handles for doing" invariant is clean and the code honors it (no
path branches on a name). So the value here is three implementation gaps where the
code doesn't quite live up to the doc's confidence, plus one scope observation.

---

## C1 — The `CapEvent.name` wire change shipped without a `PROTOCOL_VERSION` bump (Low-Med, verified) — **FIXED 2026-07-05**

**FIXED (2026-07-05):** bumped `PROTOCOL_VERSION` 5→6 with a history entry recording
the `CapEvent.name` breaking positional field-add, so the version contract now sees
the change. (The deeper fix — Q2's golden-bytes snapshot test that would *enforce*
this rather than rely on remembering — remains a separate follow-up.)

`Frame::CapEvent` gained a `name: [u8; CAP_NAME_LEN]` field at the end
(`protocol/src/lib.rs:170`) — a **breaking positional wire change** (every CapEvent
frame grows 24 bytes). The version const's own rule (line 16): *"Bumped on every
breaking change… adding a field to an existing variant (positional encoding)."* But
`PROTOCOL_VERSION` is `5`, and the history records `5` as *"appended
`RefusalReason::CapNotDelegable`"* — a different, unrelated change. The CapEvent
name field has **no version bump and no history entry**.

What happened: two breaking wire changes (CapNotDelegable and CapEvent-name) landed
in the same session, in parallel, and collapsed into one version number that
documents only one of them. It's harmless *right now* (v5 == the current post-both
state, so a v5 decoder is correct), but the history now lies about what v5 contains
— anyone reasoning "the version didn't bump for cap-names, so CapEvent's layout is
unchanged" is wrong.

This is exactly the "wire contract has no teeth / discipline-by-comment" gap the
seven-questions doc (Q2/Q3 #1) flagged, and a pointed one: **a feature whose entire
payoff is wire legibility shipped a wire change the version contract can't see.**
Fix: amend the v5 history entry to record *both* changes (they shipped together), or
bump to 6 with its own entry. And it's the strongest argument yet for Q2's
golden-bytes snapshot test — that test would have failed loudly on the CapEvent
layout change and forced the bump.

## C2 — `EndpointCreate` *rejects* boundary-straddling UTF-8 names instead of truncating them (Low-Med, verified) — **FIXED 2026-07-05**

**FIXED (2026-07-05):** added a pure `abi::pack_name_bytes(&[u8])` that distinguishes
an incomplete trailing sequence (truncate on the char boundary, per the design) from
a genuinely invalid byte (still refuse), via `Utf8Error::error_len()`. TDD:
`pack_name_bytes_truncates_a_codepoint_split_at_the_bound` + `_accepts_valid_utf8` +
`_refuses_a_genuinely_invalid_byte` (RED confirmed against a stub, then GREEN; 13 abi
tests pass). `handle_endpoint_create` now calls it instead of raw `from_utf8` +
`pack_name`, so a valid name whose 24th byte splits a codepoint truncates instead of
being refused.

The doc promises names are *"truncated to `CAP_NAME_LEN` on a char boundary"*
(design §"Bounded", and the `handle_endpoint_create` doc-comment itself). `pack_name`
(`abi/src/lib.rs:368`) does exactly that — `utf8_chunk_end` truncates on a codepoint
boundary, and there's an `é`-at-the-boundary unit test proving it. **But that logic
is dead for the overflow case on the syscall path**, because
`handle_endpoint_create` intercepts first:

```
let name_len = (frame.a2 as usize).min(CAP_NAME_LEN);   // raw byte cut at 24
let name_bytes = copy_from_user(a1, name_len, …)?;       // first ≤24 bytes
let Ok(name) = core::str::from_utf8(name_bytes) else {   // ← fails if byte 24
    refuse(BadUtf8); return;                             //   splits a codepoint
};
let id = create(pack_name(name));                        // input already ≤24 valid → no-op
```

So `EndpointCreate("<23 ASCII>é")` (25 bytes; `é` straddles byte 24) copies 23 ASCII
+ the first byte of `é`, `from_utf8` fails on the incomplete trailing sequence, and
the endpoint is **refused as `BadUtf8`** — a valid UTF-8 name rejected, where the
design says it should truncate to the 23-char prefix. The `pack_name` boundary test
gives false confidence: it exercises `pack_name` directly, on a path the syscall
never reaches (the raw cut + `from_utf8` refuse first).

**Fix, with a real sub-decision:** read up to `CAP_NAME_LEN` bytes, then distinguish
*incomplete trailing sequence* (the boundary straddle — `Utf8Error::error_len()` is
`None`) from *a genuinely invalid byte mid-string* (`error_len()` is `Some`). Truncate
at `valid_up_to()` for the former (honor the doc), still refuse for the latter (a
garbage name isn't a long name). Naively taking `valid_up_to()` unconditionally would
silently accept fully-garbage input as an empty name — so the split matters.

## C3 — `describe(name_of)` nests `ENDPOINTS` inside `caps`, contradicting the emit sites (Low, latent)

`handle_cap_list` runs `proc.caps.lock().describe(crate::ipc::name_of)`
(`kernel/src/syscall/cap.rs:99`) — so `describe` calls `name_of` (which takes
`ENDPOINTS.lock()`) *while holding* `caps`. Lock order: **`caps → ENDPOINTS`.**

Meanwhile the design deliberately did the opposite at the `emit_cap_*` sites — the
increment-2 notes say it captures the endpoint id under the caps lock and resolves
the name *after dropping it*, "to avoid nesting the endpoint lock." So the codebase
now disagrees with itself: the same "resolve an object name" step is carefully
un-nested in one place and nested in another. It's **safe today** (no path takes
`ENDPOINTS → caps`, so the order can't invert), but the invariant the emit sites
were written to preserve isn't actually uniform — the first future endpoint
operation that consults a holder's caps would deadlock `describe`. Either the emit
sites are over-careful, or `describe` should use the same capture-then-resolve
pattern; pick one and make it consistent.

## Observation — badged caps all render the endpoint's name (in scope? design call)

The name is per-**object**, and in the v0.9c model the FS server's per-file caps are
*badges* on the one `fs` endpoint, not separate objects. So a process holding caps to
`/foo` and `/bar` sees two rows both reading `Endpoint │ fs │`, distinguished only by
an opaque badge number — the "which one?" problem the feature set out to kill,
reappearing one level down for the system's most cap-heavy client. This is
consistent with the design (endpoint-scoped, holder-alias deferred), not a bug — but
if the shell's target UX is "read your authority at a glance," badged-object identity
is the half the object-name doesn't reach. Worth an explicit note in the design's
"two axes" section that badged sub-objects share the parent object's name.

---

## What's right (keep)

- **The impersonation framing is exactly correct** and rare to get right: phishing
  vs. authority-bypass, "trust decisions key on provenance, not the name," mirroring
  the shell's color-keys-on-provenance rule. Don't dilute it.
- **Scope discipline**: naming endpoints first and deferring self-describing kinds +
  holder-aliases is the right cut.
- **Wire cost is a non-issue** (and the doc's caution is still fine): CapEvents fire
  on grant/mint/transfer/revoke — rare, unlike `ContextSwitch` which dominates
  bandwidth. 24 bytes on a rare frame is noise; the tight bound is good hygiene, not
  a necessity.
- **`pack_name`/`name_str`** are clean, pure, and correctly char-boundary-safe — the
  helper is right; C2 is only that one call site bypasses it.
