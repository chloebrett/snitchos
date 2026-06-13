# 🗄️ Filesystem design

*The FS arrives at v0.10 (minimal RAMfs behind a stable trait); CoW + content-addressing + Merkle are post-v1.0 deepening. Concepts + post-v1.0 findings are below; the concrete v0.10 interface design is [its own section](#v010-interface-capability-mediated-access).*

# Roadmap position
- **v0.10 — minimal RAMfs.** A RAM-backed filesystem as a userspace component, accessed via capabilities. **The deliverable is the `Filesystem` trait** with a trivial in-memory implementation behind it. Not persistent, no snapshots. Rides on v0.9 IPC (endpoints). Interface designed below.
- **Post-v1.0 — FS deepening:** CoW + snapshots, then content-addressed + Merkle. All *additive behind the v0.10 trait* — new methods (snapshot, verify), existing methods stable. This is what makes punting them low-rework: the trait is the expensive part, fixed once at v0.10.
- Target end state: CoW + content-addressed + log-structured + Merkle-verified. Filesystem-as-Git.

# What a filesystem is, below the API
Bookkeeping over a block device. Key structural idea: the **inode**. An inode *is the file* — it holds the file's metadata (size, permissions, owner, timestamps, link count) plus the pointers to its data blocks. A **filename is deliberately not in the inode** — a filename is a directory entry (name → inode number). Decoupling identity from naming is what allows hard links: many directory entries pointing at one inode. (Same identity-vs-name pattern as capabilities, interning, sockets.)

# v0.10 interface: capability-mediated access

*Design-ahead. Depends on v0.9 IPC (endpoints), not yet built.*

## The core bet
**The kernel never learns what a "file" is.** It provides *badged endpoints*; the FS provides *all* file meaning. Mechanism in the kernel, policy in userspace — the line the [IPC design](ipc-design.md) draws.

## Two rights namespaces (don't conflate them)
| | Who defines | Who enforces | Examples |
|---|---|---|---|
| **Endpoint rights** | kernel | kernel (on the IPC op) | `SEND`, `GRANT`, `MINT` |
| **File rights** | the FS | the FS (per message) | `READ`, `WRITE`, `EXEC`, `LOOKUP`, `CREATE`, `REMOVE` |

The kernel **carries** file rights but never interprets them. Generic kernel attenuation narrows only *endpoint* rights; narrowing *file* rights is an FS mint operation under #2 (below), and becomes kernel-generic only in the #4 evolution.

## Capability mechanism — #2 badged endpoints (v0.9 substrate)
New kernel object `Object::Endpoint { endpoint: EndpointId }`. A cap to it carries a **`badge: u64`** (chosen at mint by a `MINT`-righted holder, **immutable** after, delivered **unforgeably** to the receiver on every send) plus the existing generic **`rights: Rights`** (here meaning `SEND`/`GRANT`/`MINT`). **Mint/derive** is kernel-generic: from a `MINT` cap, derive a child with a chosen badge and `rights ⊆ parent` — monotonic, never widening. On send, the kernel hands the receiver `(message, badge)`; the sender can't forge the badge.

## File capabilities
A **File cap** is a badged endpoint cap to the FS endpoint, `badge = pack(inode, file_rights)`. The FS holds the `MINT` cap; `Cap::File("/")` = `badge(root_inode, ALL)`; handing out a subset = minting a child `badge(inode, narrower_rights)`. Because the badge is immutable and FS-minted, **the FS is the sole authority over which inode + which file-rights a cap names** — both scope- and file-rights-narrowing go through an FS mint. (The kernel still narrows the *endpoint* rights generically — but that's `SEND`/`GRANT`, not `READ`/`WRITE`.) This is the *"FS holds `Cap::File("/")`, attenuates, hands out subsets"* model.

## File rights (FS-defined)
**First cut: `READ`, `WRITE` only**, gated on file inodes. Directory ops (`lookup`/`create`/`remove`/`readdir`) are ungated initially. Everything else is an additive badge bit — no trait change — along this growth path:
- **File inode:** `EXEC` (reserve the bit now per Q2; don't enforce until we load executables, ~v0.11)
- **Dir inode:** `LOOKUP` (resolve/stat a child), `LIST` (readdir), `CREATE` (add a name), `REMOVE` (unlink a name)
- **delete** = `REMOVE` on the *containing directory*; **move/rename** = `REMOVE` on source dir + `CREATE` on dest dir.

When the dir rights land, *move* and *delete* are **directory** rights, not file rights — a deliberate, Unix-faithful split.

## Storage — the inode model
A File cap must name an identity that **survives rename** — a path doesn't, an inode does (the identity-vs-name decoupling from [§What a filesystem is](#what-a-filesystem-is-below-the-api)). So an inode table arrives now: `inodes: InodeId → { kind, data, metadata }` (what badges name) + `directory: name → InodeId` (the namespace). **This supersedes the earlier "flat path→bytes" call.** The flat-vs-hierarchical choice survives but moves up a layer:
- **First impl (decided):** a single flat root directory; creating a `Dir` returns `Unsupported`. Subdirectories are an **additive impl capability — no trait change**.
- **Hierarchical later:** `directory` becomes a tree of dir-inodes; `lookup` walks components.

## The `Filesystem` trait (the deliverable — cap-agnostic, host-testable)
Inode-addressed; imports **no** cap/IPC types, so it unit-tests host-side like `kernel-core`.

```rust
pub struct InodeId(u32);
pub enum NodeKind { File, Dir }
pub struct Stat { pub kind: NodeKind, pub size: u64 }
pub struct DirEntry { pub name: String, pub ino: InodeId, pub kind: NodeKind }
pub enum FsError { NotFound, NotADir, IsADir, Exists, Unsupported, NameTooLong }

pub trait Filesystem {
    fn root(&self) -> InodeId;
    fn lookup(&self, dir: InodeId, name: &str) -> Result<InodeId, FsError>;
    fn stat(&self, ino: InodeId) -> Result<Stat, FsError>;
    fn read(&self, ino: InodeId, off: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, ino: InodeId, off: u64, data: &[u8]) -> Result<usize, FsError>;
    fn create(&mut self, dir: InodeId, name: &str, kind: NodeKind) -> Result<InodeId, FsError>;
    fn remove(&mut self, dir: InodeId, name: &str) -> Result<(), FsError>;
    fn readdir(&self, dir: InodeId) -> Result<Vec<DirEntry>, FsError>;
}
```
- **Stateless ops** — offset passed in, no cursor in the FS (a cursor, if wanted, lives client-side).
- `&self` reads / `&mut self` mutations — the server owns the FS single-threaded behind its receive loop.
- **No `open`/`close`** — "open" is a *capability* operation (mint a File cap), not a storage operation.

## Connecting layer (FS IPC front-end — above the trait)
Receive loop: `receive()` → `(msg, badge)`; unpack `badge → (inode, file_rights)`; decode opcode; check `file_rights` permit it (else reply `Denied` + **snitch** `MissingRight` + inode + attempted right); call the cap-agnostic trait method with `inode`. For ops that *yield a new inode* (`Lookup`/`Create`), **mint a child File cap** `badge = (child_inode, parent_rights ∩ requested)` and cap-transfer it in the reply (needs `GRANT`). So **`lookup` is the cap-minting / scope-attenuating op**; `read`/`write`/`stat` are plain badged messages. Badge→inode demux lives here, above the cap-agnostic trait.

## Revocation
- **FS-side (fine):** drop the badge→inode mapping / mark the inode dead → later invokes reply `Stale`. No kernel involvement.
- **Kernel-side (coarse):** bump the cap slot generation (existing hook) — kills the whole endpoint cap.

## Evolution to #4 (when a second server wants typed caps)
Lift `file_rights` out of the badge into a kernel-carried generic rights field the kernel **narrows monotonically but never interprets**; any holder then attenuates file-rights without an FS round-trip, while the FS still enforces meaning per op. Generalized: a server *registers an object type*, the kernel delivers `(badge, rights)` to the owner and enforces only monotonic narrowing + unforgeable delivery. Badged endpoints are the special case; #4 is the framework.

## Decisions & open items
- **Q1 — FileRights granularity → DECIDED:** start with `READ`/`WRITE` only; dir rights + `EXEC` are additive badge bits.
- **Q2 — EXEC → DECIDED:** reserve the bit, don't enforce until we load executables (~v0.11).
- **Q3 — First impl scope → DECIDED:** flat single-root directory; subdirs `Unsupported`, hierarchy additive.
- **Q4 — Badge packing → default (open):** `inode: u32 | rights: u16 | spare: u16` in a `u64` badge.

# Copy-on-write and near-free crash safety
Under CoW you never overwrite a block in place. Modify a data block → write a new copy elsewhere → the inode pointed at the old location, so write a new inode → whatever pointed at the inode is now stale → ... the cascade runs all the way **up to the root of the filesystem tree**. The whole FS is a tree; modifying anything means rewriting the path from that block up to the root.

Crash safety falls out almost free: the new tree is built entirely in fresh, unused space while the old tree stays fully intact and valid. The final step is a single atomic write swinging the **root pointer** from old tree to new. Before it: old version, consistent. After: new version, consistent. A crash anywhere mid-update just leaves the old root — the half-built new tree is harmless orphaned garbage. There is no half-committed state because commitment is one pointer swap. This is exactly how Git works — the FS-as-Git framing is literal, not metaphor.

# Content-addressed storage
A block is identified by the hash of its contents.

- **Free:** block IDs (the hash *is* the ID); automatic deduplication (identical blocks hash identically → stored once); integrity verification (re-hash, compare to the ID; mismatch = corruption).
- **Hard problem — garbage collection / lifetime.** A content-addressed block has no single owner; many files may reference it (that is dedup). "Can I delete this block?" becomes "does *anyone, anywhere* still reference it?" — a global problem, not a local one. Also: you cannot choose a block's ID, so you cannot organize storage by ID — hashes scatter, you lose locality and cannot pre-partition by ID.

# Garbage collection — mark-and-sweep, Git-style
Git is the worked example: content-addressed objects (blob / tree / commit), liveness by **reachability from roots** (the refs). `git gc` marks everything reachable from any ref, sweeps the rest. Lessons lifted directly:

1. **Reachability, not reference counts.** Do not track per-block counts. Periodically traverse the block DAG from all roots, mark reachable blocks, sweep the unmarked. Correct by construction; handles the DAG and dedup naturally (a block is alive iff *some* root reaches it); no per-write bookkeeping; crash-safe trivially (an interrupted sweep just freed some garbage — rerun it).
2. **Snapshots cost nothing extra.** A snapshot is literally a retained old root. "Alive" = reachable from any current root *or* any retained snapshot root — one more starting point for the mark phase. Reference counting, by contrast, would need every snapshot to have correctly bumped counts on all shared blocks.
3. **Grace period / deferred sweep.** Do not sweep continuously — periodically or under space pressure. Consider a reflog-like "recently deleted" list so accidental deletion is recoverable (very on-brand for a snapshot/time-travel FS).

The alternative — **reference counting** — gives immediate, local reclamation but requires transactional count updates on every CoW pointer churn, extra crash-consistent metadata, and cannot handle cycles. Rejected in favour of mark-and-sweep for the reasons above.

Honest caveat: pure whole-store mark-and-sweep gets expensive as the store grows. Real systems get cleverer — generational GC, region partitioning with incremental sweeps, or log-structured cleaning (GC and allocator are the same machinery). Start with whole-store mark-and-sweep; "the FS GC got too slow, here is the incremental version" is a good later milestone and post.

# Observability angle
Each mark-and-sweep run is a span — blocks scanned, marked live, swept, bytes reclaimed, duration. "Watch the filesystem garbage-collect itself" is a real demo; GC pauses showing up in traces is exactly what the observability pillar exists to make visible. Also planned: every block read/write a span, cache hit rates per tier, B-tree visualization.

# fsync and durability
`write()` only reaches the kernel page cache (RAM). Durability requires a flush toward the device — and data can still sit in the disk's own write cache. A CoW FS makes durability tractable: it becomes one well-defined moment — is the new root committed. (See Concepts & findings: "layered claims that lie for speed.")

# Open / deferred
- The `Filesystem` trait surface — designed at v0.9.
- Hash choice for content addressing.
- Merkle tree structure and verification — post-v1.0.
- Log-structured layout and the cleaner — post-v1.0.
- Encryption at rest, tamper-evident history — post-v1.0 (needs the entropy/key work).
- Persistence — v0.9 RAMfs is non-persistent; a real block device comes later.
