# 🗄️ Filesystem design

*The FS arrives at v0.10 (minimal RAMfs behind a stable trait); CoW + content-addressing + Merkle are post-v1.0 deepening. Concepts + post-v1.0 findings are below; the concrete v0.10 interface design is [its own section](#v010-interface-capability-mediated-access).*

# Roadmap position
- **v0.10 — minimal RAMfs.** A RAM-backed filesystem as a userspace component, accessed via capabilities. **The deliverable is the `Filesystem` trait** with a trivial in-memory implementation behind it. Not persistent, no snapshots. Rides on v0.9 IPC (endpoints). Interface designed below.
- **Post-v1.0 — FS deepening:** CoW + snapshots, then content-addressed + Merkle. All *additive behind the v0.10 trait* — new methods (snapshot, verify), existing methods stable. This is what makes punting them low-rework: the trait is the expensive part, fixed once at v0.10.
- Target end state: CoW + content-addressed + log-structured + Merkle-verified. Filesystem-as-Git.

# What a filesystem is, below the API
Bookkeeping over a block device. Key structural idea: the **inode**. An inode *is the file* — it holds the file's metadata (size, permissions, owner, timestamps, link count) plus the pointers to its data blocks. A **filename is deliberately not in the inode** — a filename is a directory entry (name → inode number). Decoupling identity from naming is what allows hard links: many directory entries pointing at one inode. (Same identity-vs-name pattern as capabilities, interning, sockets.)

# v0.10 interface: capability-mediated access

*Depends on v0.9 IPC (endpoints), almost done. The **badged-endpoint substrate this FS needs shipped in v0.9c** (badges, `MintBadged`, cap-transfer-on-reply, badge-on-receive — see [ipc-design.md](ipc-design.md) → *Endpoint capabilities*). The cap-agnostic core is built: `fs-core` (trait), `ramfs` (first impl), `fs-proto` (badge + opcodes). The IPC front-end (`user/fs`) and the option-D copy primitive are what remain.*

## The core bet
**The kernel never learns what a "file" is.** It provides *badged endpoints*; the FS provides *all* file meaning. Mechanism in the kernel, policy in userspace — the line the [IPC design](ipc-design.md) draws.

## Two rights namespaces (don't conflate them)
| | Who defines | Who enforces | Examples |
|---|---|---|---|
| **Endpoint rights** | kernel | kernel (on the IPC op) | `SEND`, `RECV`, `MINT` (`GRANT` deferred) |
| **File rights** | the FS | the FS (per message) | `READ`, `WRITE`, `EXEC`, `LOOKUP`, `CREATE`, `REMOVE` |

The kernel **carries** file rights but never interprets them. Generic kernel attenuation narrows only *endpoint* rights; narrowing *file* rights is an FS mint operation under #2 (below), and becomes kernel-generic only in the #4 evolution.

## Capability mechanism — #2 badged endpoints (v0.9c substrate — ✅ shipped)
`Object::Endpoint { id, badge: u64 }` (`badge == 0` = bare owner/`RECV` cap). A `MINT`-righted holder derives badged `SEND` children via the `MintBadged` syscall, setting the badge + rights; the kernel delivers the badge to the receiver in register `a6` and the sender can't forge it. Endpoint rights are `SEND`/`RECV`/`MINT` (in `snitchos_abi::rights`). The owner sets a child's rights freely (it owns the object); monotonic narrowing by non-owners — client re-delegation — is deferred. Full detail + the "badge = generalized reply cap" framing: [ipc-design.md](ipc-design.md) → *Endpoint capabilities*.

## File capabilities
A **File cap** is a badged endpoint cap to the FS endpoint, `badge = pack(inode, file_rights)`. The FS holds the `MINT` cap; `Cap::File("/")` = `badge(root_inode, ALL)`; handing out a subset = minting a child `badge(inode, narrower_rights)`. Because the badge is immutable and FS-minted, **the FS is the sole authority over which inode + which file-rights a cap names** — both scope- and file-rights-narrowing go through an FS mint. (Endpoint rights like `SEND` live in the kernel `rights` mask; the file rights `READ`/`WRITE` live in the badge, FS-interpreted.) This is the *"FS holds `Cap::File("/")`, attenuates, hands out subsets"* model.

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
Receive loop: `reply_recv()` → `Received { msg, reply, badge }` (the fused hot path now carries the badge — same `Received` as `receive_with_reply`); unpack `badge → (inode, file_rights)`; decode opcode; check `file_rights` permit it (else reply `Denied` + **snitch** `MissingRight` + inode + attempted right); call the cap-agnostic trait method with `inode`. For ops that *yield a new inode* (`Lookup`/`Create`), **mint a child File cap** `badge = (child_inode, parent_rights ∩ requested)` (the FS holds `MINT`) and cap-transfer it in the reply via `reply_with_cap` — which needs no extra right: possession of the one-shot reply cap *is* the authority to answer. So **`lookup` is the cap-minting / scope-attenuating op**; `read`/`write`/`stat` are plain badged messages. Badge→inode demux lives here, above the cap-agnostic trait.

## Message framing — how args & payload cross the rendezvous
*The protocol types are realized in the `fs-proto` crate (host-testable, no kernel/IPC types): [`Op`] (one opcode per trait method, stable append-only discriminants), [`Badge`] (the Q4 packing), and [`FileRights`]. What remained open was how each op's **arguments and payload** ride the wire.*

**The constraint.** An IPC message is `[u64; 4]` — **32 bytes, copied inline at a synchronous rendezvous** (no buffer, no growth). Two things shrink the problem first:
- The **badge already carries `(inode, file_rights)`**, so the inode never goes in the message — the body is just `opcode + args`.
- The trait **returns `usize`** for `read`/`write` (bytes transferred), so **partial transfers / chunking are already in the contract** — growing payload capacity never needs a trait change.

Per-op pressure: `Stat` fits trivially; `Read`/`Write` args fit but the **bytes** don't; `Lookup`/`Create`/`Remove` carry a **name** (short ones fit, long ones don't); `Readdir` returns a **list**. Two sub-problems: **names** (bounded-ish) and **bulk bytes/lists** (unbounded).

**The four options considered:**

| | Mechanism | Buildable on today's IPC? | Cost |
|---|---|---|---|
| **A — inline, bounded** | names ≤ message size; read/write one inline chunk/call, client loops on `off`; `readdir(n)` indexed | ✅ (only needs badge-on-receive) | ~24 B/round-trip; hard name-length ceiling; clunky readdir |
| **B — shared memory region** | a `MemoryRegion` cap maps the same frames into client + FS; message carries `(op, off, len)`, bytes live in the shared buffer (virtio / io_uring model) | ❌ needs **shared** mappings (today `MapAnon` is private) + cap lifetime | zero-copy data path, but new kernel object + exposes the whole buffer |
| **C — multi-message streaming** | framed protocol: header msg + ⌈len/24⌉ data msgs, server reassembles | ✅ | N round-trips; **reassembly state on a shared endpoint** must key on the caller, not the badge |
| **D — cross-AS copy** | message carries `(ptr, len)` into the sender's AS; the kernel copies across address spaces at the rendezvous (seL4 / L4 long-IPC / Zircon-channel model) | ❌ needs a kernel user→user copy primitive | one O(n) copy per transfer; new security-sensitive kernel mechanism |

### DECISION → D (cross-address-space copy)
**Message passing over memory sharing**, the Go maxim — and *stronger* here than in Go, because the parties sit across a mutually-distrusting isolation boundary, where sharing memory is a correctness **and** a confidentiality hazard. D's properties:
- **Ownership moves, nothing is shared.** The receiver gets a private copy of *exactly* `len` bytes — no shared mutable buffer, no "is the other side done?" handshake, no over-exposure of adjacent memory (B's footgun, and against our "hand out minimal authority" identity).
- **Single round-trip, any size** — C's clean semantics without C's chatter.
- **Backpressure is free** — the synchronous rendezvous *is* the flow control.
- **No long-lived kernel object** — the grant is transient (just the copy at the call), unlike B's `MemoryRegion` cap + revocation.
- **Close to code we already have.** `SpanOpen`/`DebugWrite` already pass `(ptr, len)` and the kernel copies from user memory; D is the user→user generalization.

**The cost we accept — and the escape hatch.** D copies the payload (one O(n) copy, ~2n memory bandwidth) on *every* transfer. B, once set up, is zero-copy on the data path — so for **large, sustained, high-bandwidth** streams B eventually wins (this is why virtio/io_uring, and Zircon's bulk VMOs, put the *data plane* on shared memory and keep only the *control plane* as messages). A RAMfs is not bandwidth-bound, and the project's discipline is *don't optimize until measured* (cf. the SPSC-vs-mutex milestone). So **B is reserved as a post-v1.0 data-plane optimization if a workload proves it needs it** — and A→D→B is a `fs-proto`-only progression behind the same trait and `Op` enum: zero churn above the protocol crate. (Elegant middle, if we want it later: at a local rendezvous the kernel can *remap* page-aligned pages instead of copying — L4 grant / Mach CoW — D's semantics at B's cost, since remap *moves ownership*.)

**Per-transfer, not per-file — and it's the ordinary `read()`/`write()` cost.** The copy granularity is the **buffer in one `read`/`write` call**, not the file: the client picks whole-file (one big call) or chunked (loop on `off`, the `usize` return makes it legal). Either way it's **one copy per call**, sender→receiver directly at the rendezvous (no bounce buffer). And every content byte crossing the boundary via a copy is not special to D — it's exactly what `read(2)`/`write(2)` cost on a monolithic kernel (page-cache ↔ user buffer). SnitchOS just relocates the "page cache" into a userspace process; the copy count is the same. The zero-copy alternatives line up one-to-one: `read()`/`write()` ≈ **D**, `mmap()` ≈ **B**.

**With a persistent (hardware-backed) FS later, the copy is even cheaper relatively.** The path is **1 DMA + 1 CPU copy**: the device DMAs disk → the FS's frames (no CPU copy), then D copies FS → client (one copy) — identical to Linux `read()` (DMA → page cache, `copy_to_user` → app), so D is never worse than a normal read. Whether the copy matters splits by axis:
- **Latency-bound (cold single reads) → copy is noise.** Per ~4 KiB: HDD ~5–10 ms / SATA SSD ~75 µs / NVMe ~20 µs vs a ~100 ns copy — 0.002–0.5%. This is SnitchOS's regime; D is unambiguously fine.
- **Bandwidth-bound (sustained NVMe streaming) → copy bites.** A PCIe-4/5 drive does 7–14 GB/s; a single-core `memcpy` ~10–20 GB/s — now the copy competes for memory bandwidth and can saturate a core (why `sendfile`/`splice`/io_uring exist).
- **Cache hits → copy is the *whole* cost.** A page-cache hit does no DMA, so there's nothing to amortize against; this is where B/mmap wins big (databases mmap for exactly this).

So the rule: **latency-bound or cold → D; bandwidth-bound or cache-hot → B.** A RAMfs / persistence-later, observability-first project lives in the first regime, and every copy is a span — so if we ever hit the bandwidth wall the traces show it before we guess. B stays the *measured-need* escape hatch, not a speculative build.

**Implications.** D needs a kernel IPC extension (a `(ptr, len)` copy that validates both address spaces) layered on v0.9 rendezvous — the one piece this milestone adds below the trait. `Lookup`/`Create` replies still carry a freshly-minted child File cap (`reply_with_cap`) alongside the result, independent of payload framing.

## Revocation
- **FS-side (fine):** drop the badge→inode mapping / mark the inode dead → later invokes reply `Stale`. No kernel involvement.
- **Kernel-side (coarse):** bump the cap slot generation (existing hook) — kills the whole endpoint cap.

## Evolution to #4 (when a second server wants typed caps)
Lift `file_rights` out of the badge into a kernel-carried generic rights field the kernel **narrows monotonically but never interprets**; any holder then attenuates file-rights without an FS round-trip, while the FS still enforces meaning per op. Generalized: a server *registers an object type*, the kernel delivers `(badge, rights)` to the owner and enforces only monotonic narrowing + unforgeable delivery. Badged endpoints are the special case; #4 is the framework.

## Process isolation falls out — authority, not resources
A long-standing hope for this design was that Docker-style process isolation would fall out *for free*. It does — but only along the **authority** axis ("*what* can you touch?"), not the **resource** axis ("*how much* can you consume?"). Docker bundles both; capabilities are an authority mechanism.

| Docker mechanism | Free here? | Why |
|---|---|---|
| Filesystem isolation (chroot, mount ns, overlayfs) | ✅ free, **stronger** | A process sees only files it holds caps to; its root cap *is* its chroot, with no `/` to escape *into*. |
| Process/PID isolation | ✅ free | No ambient process namespace; you reach another process only via a cap to it. |
| IPC isolation | ✅ free | IPC is capability-gated by construction ([ipc-design.md](ipc-design.md)). |
| Network isolation | ⚠️ partial | "No network" is free; per-container virtual networks need a cap-aware network server (v1.2). |
| Resource limits (cgroups: CPU/mem/IO) | ❌ not free | Authority ≠ quantity — a correctly-scoped process can still spin the CPU or allocate to its ceiling. |
| Image/layering | ❌ orthogonal | Packaging, not isolation — but lands later via the [CoW + content-addressed](#content-addressed-storage) deepening (the layered-image model, literally). |

**Why the authority half is stronger than Docker.** Docker isolates by *subtraction* — Linux defaults to "see everything," and namespaces carve a restricted view of a global world (hence escapes: a leaked mount and you're back in the namespace you could always *name*). Capabilities isolate by *construction* — a process starts with nothing and can only reach what it was handed; there is no global world to escape into. The failure modes invert: Docker *fails open* (forget to restrict → too powerful), caps *fail closed* (forget to grant → too weak). And structurally, **a "container" is just a process plus its starting capability set** — `spawn(program, caps)` *is* the sandbox for the FS/process/IPC slice; no container runtime, no namespace plumbing.

**Caveats.**
- "Free" = no new *kernel* mechanism, but still needs **correct, scope-enforcing servers + deliberate grant wiring**: the FS must refuse to mint caps above a container's subtree root, and init must hand each container its own subtree root cap. Scope-subset is **FS-trusted**, not kernel-enforced — the isolation is exactly as strong as the FS is correct.
- **The v0.10 first cut is flat-single-root + READ/WRITE-only**, but per-container subtree isolation is closer than it first looks: hierarchical dirs are a cheap additive change and **confinement then falls out for free** — `lookup` descends only and there is no parent op, so a subtree cap can't be escaped (see [§Hierarchical directories](#hierarchical-directories--the-mechanism-is-cheap-confinement-is-free)). Only *fine-grained* directory rights remain a genuine add-on.
- **Resources need their own answer.** Memory *could* be made cap-enforced (seL4 untyped-memory style: allocate only by spending a memory cap) — SnitchOS today uses a global frame allocator + a crude per-process 16 MiB heap ceiling (`MapAnon`), not cap-counted memory. CPU time isn't ownable that cleanly; per-container CPU quotas stay a scheduler feature. Synchronous IPC backpressure (parked sender, no buffer growth) gives partial flow control for free.

*Post angle: "I didn't build containers, I just stopped handing out authority."*

## Hierarchical directories — the mechanism is cheap, confinement is free
Subdirectories were filed as "additive" (Q3), which undersold it: the `ramfs` data model is *already a tree* — `Node::Dir(BTreeMap<String, InodeId>)` over a flat inode table, with the root being one such `Dir`. `lookup`/`stat`/`readdir` are already generic over any directory inode; the *only* thing enforcing flatness is `create` refusing `NodeKind::Dir`. Flip that one arm and nested `create`/`lookup`/`readdir` work (~an hour + tests).

**Subtree confinement is then free — the part Unix pays dearly for.** Authority is the cap you hold; the only way to obtain a child cap is `lookup(dir_cap, name)`, which **descends only** (mints `(child_inode, rights)`), and **there is no parent op** in the trait. So a directory cap *is* a subtree-confined cap by construction: a holder can designate only what lies at or below it — never its parent, because it has no way to *name* one. A "chrooted" process is just one handed a subtree cap instead of the root cap; same code, no namespace machinery (the fails-closed / "stronger than Docker" property above, made concrete). The secure default is therefore to **keep `..` out of the FS** — adding a parent op is what would *break* confinement, so its absence is a feature, not a gap.

**`..` lives in the shell, not the filesystem.** Navigation still wants `cd ..` — but that is the *client's* state, not an FS capability. The shell keeps a **stack of `(name, dir-cap)`** from its root to the cwd: `cd foo` = `lookup(top, "foo")` + push; `cd ..` = **pop** (the parent cap is the one underneath — already held); `pwd` = the names joined. `..` never touches the FS and cannot escape the stack's bottom, which is whatever cap the shell was handed (root → full-system shell; a subtree cap → a confined shell, same code). This is POSIX `openat(dirfd, …)` + `RESOLVE_BENEATH` as the native and only model — the cwd is just the top-of-stack handle. (`pwd` is the shell's *memory of names*; the FS is inode-addressed, so a rename mid-session leaves the cap valid and the displayed name stale — name vs identity, [§Storage](#storage--the-inode-model).)

**What's actually left** is modest and do-when-needed: *fine-grained* directory rights — `LOOKUP`/`LIST`/`CREATE`/`REMOVE` bits so a delegated dir cap can be e.g. list-only. Coarse "hold the dir cap ⇒ may do dir ops" works now and confinement holds regardless; the bits are the same per-op gate `READ`/`WRITE` already use. So **only the granularity is additive — the isolation itself is not deferred.**

## Decisions & open items
- **Q1 — FileRights granularity → DECIDED:** start with `READ`/`WRITE` only; dir rights + `EXEC` are additive badge bits.
- **Q2 — EXEC → DECIDED:** reserve the bit, don't enforce until we load executables (~v0.11).
- **Q3 — First impl scope → DECIDED:** flat single-root directory; subdirs `Unsupported`, hierarchy additive.
- **Q4 — Badge packing → DECIDED + realized:** `inode: u32 | rights: u16 | spare: u16` in a `u64` badge; implemented in `fs-proto::Badge` (round-trip + exact-layout tests).
- **Q5 — Message framing → DECIDED:** option **D** (cross-address-space copy — message passing, not shared memory). See [§Message framing](#message-framing--how-args--payload-cross-the-rendezvous). Needs a kernel user→user copy primitive on top of v0.9 rendezvous; **B** (shared region) is the deferred post-v1.0 bulk data-plane optimization.

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

# File metadata — xattrs on the inode, not in-band sidecars
When the FS needs metadata beyond `Stat { kind, size }` — timestamps, a type hint, tags, provenance ("downloaded"), a UX nicety — the home is **extended attributes on the inode**, not in-band sidecar files. Where real systems put it: intrinsic structural metadata (type/size/times/perms) in the inode; extensible per-file metadata in **xattrs** (Linux `user.*`/`security.*`; macOS `com.apple.quarantine`); provenance in a stream/xattr (NTFS `file:Zone.Identifier` mark-of-the-web ≈ macOS quarantine); derived/cross-cutting in a central index (Spotlight); and the anti-pattern — per-folder UX state as **in-band files** (`.DS_Store`, `Thumbs.db`, `desktop.ini`).

`.DS_Store` is the cautionary tale, and it is *strictly worse under capabilities*: writing UX state in-band requires `CREATE`/`WRITE` on the **directory**, so a file browser would need directory-write authority just to remember icon positions — least authority violated for something cosmetic. Xattrs avoid that: the metadata authority **rides the file cap you already hold** (optionally its own `XATTR` right), and being inode-attached they are **rename-safe** — metadata and capability move together under rename, exactly because a cap names an inode, not a path ([§Storage](#storage--the-inode-model)). A sidecar `.meta` would *not* follow the inode. So the inode-identity model and the cap model both make xattrs the natural home and sidecars the unnatural one.

Cheap to add to `ramfs` when needed (same shape as directories: a `BTreeMap<String, Vec<u8>>` per `Node` + `get/setxattr` trait methods + two `Request` variants + server gating) — but **not built until something needs it**; `Stat { kind, size }` stays the v1 surface. Keep one line clean that `.DS_Store` blurs: **user-editable config** (a `.shellrc`, history, aliases) is legitimately a regular **file** (in-band is correct, like `~/.bashrc`); **per-file system/presentation metadata** is **xattrs**. "Is this a program?" is one instance — answered by content-sniffing for v1 (no `EXEC` bit; see [fs-executables-design.md](fs-executables-design.md)), or a `user.type` xattr later if a browser wants the type without reading bytes.

# Open / deferred
- The `Filesystem` trait surface — **designed + implemented** (`fs-core` trait, `ramfs` first impl, `fs-proto` wire protocol; all host-tested).
- Message framing — **decided** (option D, cross-AS copy); the kernel copy primitive is the implementation step that remains.
- Bulk data-plane via shared memory (option B) — deferred to post-v1.0, only if measurement demands it.
- Hash choice for content addressing.
- Merkle tree structure and verification — post-v1.0.
- Log-structured layout and the cleaner — post-v1.0.
- Encryption at rest, tamper-evident history — post-v1.0 (needs the entropy/key work).
- Persistence — v0.9 RAMfs is non-persistent; a real block device comes later.
