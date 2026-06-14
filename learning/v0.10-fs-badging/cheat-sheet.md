# v0.10 — The Badging Model in the Filesystem — Cheat Sheet

## The core bet
**The kernel never learns what a "file" is.** Kernel = mechanism (rights-checked IPC + unforgeable badge delivery). FS = policy (what a file is, what READ means). Mechanism/policy separation taken to its limit.

## Two rights namespaces (don't conflate)
| | Defined by | Enforced by | Lives in | Examples |
|---|---|---|---|---|
| **Endpoint rights** | kernel | kernel, on the IPC op | the cap's `rights` mask | `SEND`, `RECV`, `MINT` |
| **File rights** | the FS | the FS, per message | the **badge** | `READ`, `WRITE`, `EXEC`(reserved), dir: `LOOKUP/LIST/CREATE/REMOVE` |

The kernel **carries** file rights in the badge but **never interprets** them. Same mechanism serves unboundedly many policies (FS, net server, display server each define their own badge rights).

## File cap = badged endpoint cap
- `Object::Endpoint { id, badge }`, `rights = SEND` (for a client's File cap).
- `badge = pack(inode, file_rights)` — `fs-proto::Badge`: `inode` in bits `[0..32)`, `rights` in `[32..48)`, spare on top. `badge == 0` = the FS's own owner/`RECV` cap; File caps are always nonzero.
- Every client's File cap is just another `SEND` cap to the **one** FS endpoint — they differ only by badge. The badge turns "a cap to the FS" into "a cap to *inode 5, read-only*."

## MINT and minting
- **`MINT` is a right** (kernel bit), not an operation. The operation is the **`MintBadged` syscall**, which `MINT` authorizes.
- `mint_badged(parent, badge, rights) -> child` — child names the **same endpoint id**, with a badge + rights you choose. The FS owns its endpoint, so it sets a child's rights **freely** (owner privilege). Non-owner narrowing (client re-delegation) is **deferred**.

## lookup = "open"; there is no open/close
- `read/write/stat` = plain badged messages (inode rides in the badge).
- `lookup`/`create` = the **cap-minting** ops: resolve/make an inode, then `mint` a child File cap `badge = (child_inode, parent_rights ∩ requested)` and `reply_with_cap` it back. **"open" returns a *capability*, not an fd.**
- No open-file table, no fd, no server-side cursor: the trait is **stateless** (offset per call); the cursor, if wanted, lives **client-side**. The statefulness of `open()` didn't move into the FS — it dissolved.
- (Today: `Lookup`'s wire message has no requested-rights field — `w3` is the unused/spare word — so the first cut propagates parent rights; per-call narrowing is an additive change in that word.)

## Attenuation
`child.rights = parent.rights ∩ requested`. Monotonic — authority only ever shrinks downward. Fail-closed.

## Where policy lives (the unlock)
Lampson access matrix: rows = subjects, cols = objects, cells = rights.
- **ACL stores it by column** (each object lists who) — **Linux**. Identity-checked at access time.
- **Capabilities store it by row** (each subject lists what it holds) — **SnitchOS**.

There is **no object-attached policy** — a file has no owner/mode. The decision "A may read foo" is made **at delegation time, by whoever holds delegatable authority**, and bottoms out at **`init`** (the trusted base that distributes the first subtree caps). **The FS has *no* policy — it's pure identity-free mechanism.** Policy = the shape of the delegation graph.

## Litmus test: "is my policy component just Linux again?"
| Consulted… | = |
|---|---|
| at **every access**, arbitrating by **identity** | ambient / Linux ❌ |
| only at **delegation/spawn time**, by **handing out caps** | capabilities ✅ |
The **launcher/shell/init is the policy point** — decides what each child is *born* with (`spawn(prog, caps)`), then steps out of the path. A "policy server" is fine as a *delegation authority*; Linux-land only if it becomes a runtime identity arbiter.

## Two failure modes (keep them distinct)
| Attempt | Outcome | Why |
|---|---|---|
| wrong op on a **held** cap (WRITE with a READ badge) | **FS rejects** → `Denied` + snitch `MissingRight` | holds the cap, badge lacks the right |
| touch an object you hold **no** cap to (`/etc/secret`) | **unnameable** | inode rides in the badge (fixed); no badge arg to forge; no MINT; no dir cap to `lookup` there |
"Rejected" = key doesn't turn this lock. "Unnameable" = door isn't on your keyring. Cap isolation is mostly the second — fail-closed by **unreachability**.

## Bulk bytes — option D (decided)
Message is `[u64; 4]` = 32B. For `read`/`write` payload, the message carries `(ptr, len)` into the *sender's* AS; the **kernel copies across address spaces** at the rendezvous, validating both. Not a hole: exactly `len` bytes, **ownership moves, nothing shared**, no over-exposure. (Option B / shared memory reserved as a post-v1.0 bandwidth-only optimization. A→D→B is `fs-proto`-only, no trait churn.)

## Revocation
- **FS-side:** drop the badge→inode mapping / mark inode dead → later ops reply `Stale`. No kernel involvement.
- **Kernel-side:** bump the cap slot generation (existing hook) → kills the whole endpoint cap.

## Isolation falls out (authority axis only)
Your root File cap *is* your chroot — there's no `/` to escape *into* (you start with nothing, reach only what you were handed). Fails **closed** (forget to grant → too weak) vs Docker/Linux failing **open**. "A container is just a process + its starting cap set." Resource limits (CPU/mem quantity) are NOT free — authority ≠ quantity.

## Lineage
- **seL4** — endpoints, badges, `Mint`, one-shot reply caps, badge-demux: lifted almost beat-for-beat.
- **EROS/KeyKOS** — "files are capabilities," no ambient authority.
- **Plash / powerbox (CapDesk) / Genode / Fuchsia / Capsicum** — the launcher-grants-explicit-caps + "designation is authorization" shell model.
- **Not seL4:** no untyped (cap-counted) memory; not formally verified; bulk path is option-D copy (L4/Zircon-ish), not seL4's shared frames; the snitching/observability layer is original.
- False friend: Linux `capabilities(7)` (`CAP_NET_ADMIN`…) are *ambient* privilege slices, **not** object capabilities.

## Build status
- `fs-core` (trait), `ramfs` (first impl), `fs-proto` (Badge/Op/Request/Response): **built + host-tested**.
- `user/fs` IPC front-end (`fs-server`/`fs-client`, the badge→inode demux + lookup-mints-cap logic): **now under construction** (appears in `kernel/build.rs`).
- Remaining below the trait: the kernel user→user copy primitive (option D).

## SnitchOS angle
Every delegation is a `CapEvent` on the wire → you can **watch least-authority happen in traces**. An explicit-permission, observable-delegation shell (`spawn cat` with one file cap, the grant shows up as a span) is a novel demo + post — see `learning/v0.10-fs-badging/session-log.md`.
