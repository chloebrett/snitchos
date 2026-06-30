# Typed processes, Hitch (the value model), and manifest storage

**Status:** **Design / exploration (captured 2026-06-28). Pre-implementation.**
Began as a correction to §7 of
[userland-text-streams-and-the-actor-model-design.md](userland-text-streams-and-the-actor-model-design.md)
(the cross-process format) and grew into a self-contained idea: **a process
declares its own input/output types and required capabilities as part of its file —
a function signature, externalized into the filesystem.** That single move resolves
the format question, makes cross-process pipelines typecheckable, completes the
function/process unification the userland doc was circling, and fixes Unix's
untyped `bytes → bytes` original sin. The shared value model underneath it all is
**Hitch** (§2) — the self-describing format that *hitches a ride* on every channel;
`hitch`/`unhitch` are its verbs.

Supersedes [userland §7](userland-text-streams-and-the-actor-model-design.md#7-the-cross-process-format--a-data-model-two-encodings-superseded).
Sits beside:

- [userland-text-streams-and-the-actor-model-design.md](userland-text-streams-and-the-actor-model-design.md)
  — `|>`/`~>`, the two-layer authority model, placement-follows-authority, actors.
- [filesystem-design.md](filesystem-design.md) — the inode/xattr model (where the
  manifest lives).
- [fs-executables-design.md](fs-executables-design.md) — programs as files; the
  `fs-image/` drop-in (`.st`) + build-injected ELF seed.
- [language-design.md](language-design.md) — Stitch `uses`, `prod`/`sum`.
- [protocol/](../protocol) — `Frame` (one instance of the data model below).

---

## 1. The bug this fixes

The userland doc's §7 proposed "a tagged postcard `Value`" as the one cross-process
format and claimed the tag bought generic, schema-free decoding. It doesn't, and
the reason generalizes:

- postcard is **positional** (like `repr(C)`): bytes carry no field names, so
  decoding requires the type definition. A **type tag is a discriminant — it says
  *which* type, not *what shape*.** So a tag disambiguates; it does not let a generic
  consumer attach field names.
- **Compact-positional** and **self-describing** are two *encodings of one data
  model*, not one format: positional bytes a Rust `struct` reads directly (shared
  type, *not* generic) **vs** a self-describing blob any consumer decodes into a
  named record. You cannot have both bytes at once.

Where it bites: the nushell-style generic renderer (userland §4) — a table over
*arbitrary* records, `frames`, inspecting unknown values — needs **field names at
the boundary**. In-process that's free (`DataValue` carries them); across a
positional boundary they vanish.

And it is the same untyped-ness that gave Unix its re-parsing tragedy: a Unix
program's type is `bytes → bytes` (argv+stdin → stdout+exit). Untyped programs ⇒
every pipeline stage re-parses the last one's text. **Typing the program is the
fix.**

---

## 2. Hitch — the value model (algebraic, one model with two encodings)

The model is named **Hitch**: the self-describing value format that *hitches a ride*
on IPC, telemetry, files, and the `~>` boundary. A serialized value is **a hitch**;
the verbs are **`hitch`** (serialize) and **`unhitch`** (deserialize). One name
covers both the abstract model and its encoding.

What's common to `Frame`, Rust structs/enums, and Stitch `DataValue` is not a wire
format — it's a **shape**: scalars, sequences, **products** (named fields), **sums**
(tagged variants). Hitch is that algebraic model, a superset of all three (`Frame`
is a sum; a Rust struct is a product; `DataValue { type_name, variant, fields:
[(Option<String>, Value)] }` is a self-describing product/sum). So **design the
model, treat the encoding as swappable:**

- **self-describing** — the schema rides *inline*, inseparable from the data
  (genuinely *hitched*: the value is married to its schema). Any consumer
  `unhitch`es it into a named record. Bigger.
- **packed** — positional payload against a schema that lives *apart*, in the
  program's manifest (§3, §5). Small; needs that schema to `unhitch`.

*Same model, two encodings, pick per consumer* — this is the clean resolution of the
§7 fork. Prior art to borrow from rather than reinvent: **CBOR** (self-describing
binary), **Avro** ("schema travels with the data"), **Protobuf** (schema-required,
positional), and especially **Cap'n Proto** — same author as protobuf, schema-based
canonical wire format, and an RPC layer with **promise pipelining** straight from
the E/object-capability lineage (see userland §10). Cap'n Proto is the closest
existing thing to "typed messages + capability RPC + pipelining."

**The known-schema principle (the syscall-decoration point):** *self-description is
materialized at boundaries where the consumer is generic, derived from a known
schema where it isn't.* The kernel **syscall ABI is a known schema** (positional
registers) — so a gateway can *lift* syscall args/results into the self-describing
model for tooling/observability, without the syscall path paying for it. Syscalls
stay positional; their self-described form is *generated* from the ABI on demand.

**The packed codec needs a POD fast-path (the structured-syscall-return case).**
A structured syscall return — the v0.13 `CapList`/`hold` is the first; `readdir`,
span-list, metric-list follow — is a *packed hitch*: the kernel writes records into
a caller buffer against a known schema (the ABI struct). For a **fixed-size,
padding-free, scalar-only** type (`CapDesc`), the packed encoding *is* the in-memory
layout, so Hitch's packed codec should special-case it to a **zero-cost transmute**
(`from_raw_parts` over `&[T]`), not a serialize pass. The payoff is not speed but
**centralization + graceful degradation**:

- The `unsafe` byte-cast moves out of each syscall handler into **one audited place**
  (the codec), gated by `#[derive(Schema)]`.
- "No uninitialized padding is copied to userspace" becomes a **derive-checked
  guarantee** (the derive proves POD-ness / zeroes padding), not a hand-written
  SAFETY comment per site. (`CapList`'s handler today hand-asserts this; the derive
  should subsume it.)
- The **same** codec handles the variable-length / sum-typed returns (`readdir`,
  span-list) where a raw `from_raw_parts` would be *unsound* — there it degrades to a
  real serialize. One mechanism from POD-free to complex, so the next structured
  syscall reuses it instead of inventing a third byte-cast.

Still *packed*, never self-describing at the syscall (the known-schema principle
above): the self-describing form is materialized only on the userspace `unhitch`
side. So `CapList`'s current hand-rolled `from_raw_parts` cast is the placeholder
for `hitch_packed(&descs)` — deliberately shaped so the swap is mechanical once the
codec + `Schema` derive exist.

`Frame` stops being a hand-rolled special case — it becomes a value in this model
(telemetry, IPC messages, and process I/O share one type language; routing stays
split per the "two channels, don't confuse them" rule).

---

## 3. Typed processes — the externalized signature

Give a program a **typed interface declared in its file**: input type, output type,
and — the SnitchOS twist — its `uses` capabilities. A process's interface is

```
(in: T, out: U, uses: Caps)
```

which is *exactly* a Stitch function signature: `f(x: T) -> U uses Caps`. Strong
prior art:

- **WASM component model + WIT** (WASM Interface Types) — a component declares typed
  imports/exports as an interface *in a file*, and the runtime lifts/lowers between
  the component's internal types and a canonical ABI. This is *precisely* "a process
  declares its in/out types as part of the file"; SnitchOS already has a WASM
  direction, so this is convergent. The novelty SnitchOS adds is **capabilities in
  the interface** — the flavor WIT lacks.
- **Typed actors** (Akka Typed, Pony behaviors) — the actor declares its message
  protocol as a type.
- **gRPC/proto service definitions**, PowerShell cmdlet `OutputType`.

What the manifest buys, beyond §2's correctness:

- **Packed payloads *and* generic decoding** — because the schema is declared
  **once, in the program's file**, not shipped per message. A producer emits *packed*
  hitches; any generic consumer reads the manifest to recover names and `unhitch`.
  This is Avro's "schema alongside data," with the schema living in the executable.
- **`~>` becomes typecheckable.** `a ~> b` is legal iff `a.out` is marshallable into
  `b.in` — layered on top of the cap-compatibility check from
  [userland §8 (placement-follows-authority)](userland-text-streams-and-the-actor-model-design.md#8-the-keystone-placement-follows-authority).
  The shell rejects a mismatched pipeline *before* spawning anything.
- **Structural, not nominal.** The manifest carries **structural** schemas (the
  shape), so `~>` compatibility is structural — the only thing that *can* work across
  a Rust-ELF ↔ `.st` boundary (same shape, different language). Keeps the
  language-neutral property real.

---

## 4. The payoff: a process is a function with its signature in the filesystem

The function/process dichotomy (userland §5–§6) collapses:

> **A process is just a function whose signature lives in the filesystem**, with
> marshalled I/O and a kernel-enforced boundary.

So the open threads all reduce to one concept — a typed, cap-annotated signature:

- composition: `~>` connects two externalized signatures across a process boundary
  exactly as `|>` connects two in-process signatures (`~>` : `|>` :: cross-proc :
  in-proc).
- relocatability (userland §6): "is this signature **marshallable**?"
- placement (userland §8): "does the runtime hold the caps this signature's `uses`
  row names?"

*Same signature shape; different enforcement strength and location.* This completes
the two-layer-authority thesis at the type level.

---

## 5. Where the manifest lives — xattrs (`user.iface`)

The home is an **extended attribute on the inode**, per
[filesystem-design.md](filesystem-design.md#file-metadata--xattrs-on-the-inode-not-in-band-sidecars).
The doc already anticipates the degenerate case (a `user.type` "is this a program?"
hint); the manifest is the rich version of exactly that. Why it fits:

- **Rename-safe, inode-attached** — the manifest is a property *of the executable*
  and must move with it under rename, like the cap. A sidecar `.manifest` file is
  the `.DS_Store` anti-pattern (needs directory authority, doesn't follow the inode).
- **Authority rides the file cap** — reading a program's manifest needs only a cap to
  the program file, no directory traversal. On-theme consequence: **you can inspect a
  program's required-authority (`uses`) manifest *before* granting it anything** —
  "see what it will ask for before you hand it caps," the powerbox "see before you
  grant," at the executable level.
- **O(1), no code-load** — the shell's pre-spawn step (typecheck `~>`, decide grants)
  reads `getxattr(file_cap, "user.iface")` without parsing the ELF or running a Stitch
  parser. Cheap, and it runs on every pipeline.
- **Uniform across `.st` and ELF** — consumers want *one* read surface regardless of
  the underlying file kind. The *populate* step differs (§6); the read does not.

The value stored is a **hitched manifest** (§2) — xattrs are already
`BTreeMap<String, Vec<u8>>`, so `user.iface` holds a small hitch. Clean closure:
Hitch (§2) is the xattr payload, the xattr is the rename-safe cap-gated home (§5),
and the manifest is the externalized signature (§3).

**Nuance — a manifest is a *contract*, not mere metadata**, which raises a
source-of-truth question `user.type` doesn't have:

- For **`.st`**, the manifest is *in the source* (typed `main` + type defs);
  the xattr is a build-time *extraction*.
- For **ELF**, it is *build-generated*; store it as the xattr, **or** embed it in an
  ELF note and treat the xattr as a cached *projection* of that note.

Caveats: an xattr detached from the artifact can drift or be lost (cross-FS copy,
xattr-stripping archive). Internally, xattrs travel with the inode; the risk is at
the **host-build seam** (`include_bytes!` an ELF into the `SEED`), where the xattr
must be threaded in lockstep. §6's build path makes the ELF note the authoritative
source and the xattr a derived projection — which removes the drift risk *for free*.

---

## 6. The build path — easy for Stitch, a macro for Rust

The asymmetry, named precisely: **Stitch's interpreter still has the types as data**
(it parsed them; the AST/`DataValue` carry names). **Rust erases types at compile
time** — by the time there's an ELF, `main`'s in/out shape is gone. So the task is
not "store the manifest" (`#[link_section]` makes storage trivial); it is **capture
the type shape *during* compilation, before erasure.** That belongs at the **macro
layer**, not a raw `build.rs` (which has no type info either).

**Rust path:**

1. **`#[derive(Schema)]`** — a bespoke reflection derive (schemars-style, targeting
   §2's model). Walks a struct's fields / an enum's variants and emits
   `const SCHEMA: &TypeSchema = …`. This recovers, for Rust, what the interpreter has
   for free.
2. **Extend the existing `#[entry]` macro** (`user/macros`) to
   `#[entry(in = Lines, out = Table, uses = [FsRead, ConsoleOut])]`. It composes
   `Lines::SCHEMA` + `Table::SCHEMA` + the cap list, `hitch`es it, and emits the
   bytes:
   ```rust
   #[unsafe(link_section = ".snitch.iface")]
   #[used]
   static IFACE: [u8; hitch::MANIFEST_BYTES] = hitch::encode_manifest(&MANIFEST);
   ```
   It already wires crt0/entry; this is a few lines more in the same expansion, run
   during normal `cargo build`. **Shipped** (with a fixed-size note + length prefix,
   not a const-generic `N`; a too-large manifest is a compile error).
3. **The seed step extracts it.** `user/fs/build.rs` (the declared-executable
   ELF-injection) reads the `.snitch.iface` section out of the linked ELF with
   the `object` crate and writes those bytes to `user.iface` in the `SEED`. *(Not yet
   built — the bytes are produced and `hitch::decode_manifest`-able; nothing reads
   them yet.)*

Because the manifest is generated by the macro from the **same type declarations the
code uses** and embedded in the ELF, it **cannot drift from the code** — rebuild the
code, the macro re-derives it. So the xattr is a *cached projection* of the
authoritative ELF note (the §5 hardening option becomes the default).

**Stitch path:** the manifest is the typed `main` + type defs in the source; extract
it to `user.iface` at seed time (or have the FS parse on demand). Same uniform read
surface for consumers.

**Gotchas to budget for:**

- **The linker script can eat the section (it did).** Two traps, both hit: (1)
  `user.ld` `/DISCARD/`s `*(.note .note.*)` — the GNU note convention — so a
  `.note.snitch.iface` name is silently dropped; **named it `.snitch.iface`** to
  dodge that. (2) `--gc-sections` can still GC it even with `#[used]`, so the script
  needs `KEEP(*(.snitch.iface))`. Both done in `user/runtime/user.ld`.
- **Real `SHT_NOTE` vs a plain custom section.** A proper note (name/type/desc)
  makes `readelf -n` work; a plain `.snitch.iface` is simpler. `object` reads either —
  start plain, upgrade if standard tooling needs to see it.
- **The derive's scope.** Concrete product/sum/scalar/seq is straightforward;
  *generics and recursion* are where reflection gets hairy. Bound v1 to monomorphic,
  non-recursive in/out types. (The proc-macro crate runs on the *host*, so it may use
  `std`/`syn`/`quote` even though its output is no_std.)

**The cheap v0:** ship a **hand-declared** shape in `#[entry(in = "…", out = "…",
uses = […])]` first — same section, same extraction — accepting that the declared
shape can drift from the real Rust type. Then add `#[derive(Schema)]` to make it
*checked against the type*. The storage/extraction pipeline is identical; only
*generation* hardens. This lands the end-to-end path (section → xattr → shell
typechecks `~>`) before the reflection derive exists.

---

## 7. Settled leanings vs open forks

**Settled (leaning):**

- **Hitch** — one algebraic value model (scalar/seq/product/sum) covering `Frame` +
  serde types + `DataValue`, with verbs `hitch`/`unhitch`; **two encodings**
  (self-describing vs packed+schema), picked per consumer. A tag is a discriminant,
  never a generic decoder.
- Self-description is **materialized at generic boundaries from a known schema**;
  syscalls stay positional (ABI is the schema), lifted on demand.
- **Typed processes:** each program declares `(in, out, uses)` — an externalized
  function signature. Schema-in-the-file ⇒ compact payloads *and* generic decoding;
  `~>` is **structurally** typecheckable on top of cap-checkable.
- **A process is a function with its signature in the filesystem** — the
  function/process duality collapses; relocatability = "marshallable signature,"
  placement = "runtime holds the `uses` caps."
- Manifest home = **`user.iface` xattr** (rename-safe, cap-gated, O(1), uniform
  across `.st`/ELF; "inspect required authority before granting").
- Rust build path = **`#[derive(Schema)]` + extend `#[entry]` → `.snitch.iface`
  link section → seed extracts to the xattr**; structural schemas keep `~>`
  cross-language; the note is authoritative, the xattr a projection (no drift).

**Open forks:**

1. **Hitch's encoding — roll our own or adopt CBOR/Cap'n Proto?** Own bytes (no_std,
   can make cap-handles/span-context first-class Hitch scalar kinds, on-theme) vs
   serde-targets-CBOR (free for Rust). Lean: Hitch is our *model*; start with a
   pragmatic CBOR-shaped *encoding*.
2. **xattr as source-of-truth vs cached projection of an ELF note.** §6 makes the
   note authoritative by default; revisit only if foreign (non-trusted-build) ELFs
   are ever loaded.
3. **Manifest versioning/evolution** — change `out` and consumers break; the usual
   proto/WIT discipline (don't reorder, add optional fields). Where does a version
   live — in the manifest, the xattr key, or the schema?
4. **Nominal identity for cross-program type *names*?** Structural compatibility is
   enough for `~>`; a shared name registry would let the shell *say* "this is a
   `Table`." Probably defer.
5. **Generics/recursion in `Schema`** — bounded out of v1; when do they come in?

**Build order (additive on shipped seams):**

- Hitch (§2) value model + encoder/decoder (`hitch`/`unhitch`) + a `Schema` derive —
  host-tested, pure. ✅ shipped.

**Manifest build order — DECIDED 2026-06-28: Stitch-first, parse-on-demand.** The
manifest is `(in: TypeSchema, out: TypeSchema, uses)` = a program's `main`
signature. Two phases that converge on the same `Manifest` + structural-compat
check, so phase 1 is not throwaway:

- **Phase 1 — Stitch, parse-on-demand (no FS/build plumbing).** The shell already
  has the interpreter and is about to run the `.st`, so it maps `main`'s parsed
  signature → a `Manifest` *in memory* — no xattr, no ELF note, no seed step. Needs:
  a **type bridge** (`stitch::Type` AST → `hitch::TypeSchema`, the type-level twin of
  the `Value` bridge), a `main`-signature → `Manifest` extractor, and
  `hitch::TypeSchema` schema-vs-schema **`compatible`** (v1 = structural equality).
  Unlocks typed `~>` between `.st` stages — the 90% case (shell coreutils are `.st`).
  *A stage's interface IS `main`'s typed signature* (`main(x: T) -> U uses C`);
  `Func`/`@`/generic types are not marshallable. v1 covers scalars + `List` +
  monomorphic prod/sum.
- **Phase 2 — the uniform `user.iface` surface + Rust stages.** `#[entry(in,out,uses)]`
  → `.snitch.iface` link section → `user/fs/build.rs` extracts to the xattr;
  Stitch manifests also extracted to the xattr at seed time; `getxattr` in `ramfs`.
  Adds a second *source* (xattr, O(1), no parse) and a second *producer* (Rust macro)
  for the identical artifact.
- Shell pre-spawn step: get each stage's `Manifest`, structurally typecheck the
  `~>` pipeline, decide `uses` grants — extends userland §8's cap analysis.

---

## 8. References

- WASM **component model / WIT**; **Cap'n Proto** (schema + capability RPC +
  promise pipelining; Kenton Varda); **CBOR** (RFC 8949); **Avro** (schema-with-data);
  **Protobuf**; **schemars** (derive-a-schema-from-a-type, the `Schema`-derive
  inspiration).
- In-repo: [userland-text-streams-and-the-actor-model-design.md](userland-text-streams-and-the-actor-model-design.md),
  [filesystem-design.md](filesystem-design.md),
  [fs-executables-design.md](fs-executables-design.md),
  [language-design.md](language-design.md),
  [ipc-design.md](ipc-design.md),
  [capability-system-design.md](capability-system-design.md).
