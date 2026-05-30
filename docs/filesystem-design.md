# 🗄️ Filesystem design

*Stub. The FS arrives at v0.9 (minimal RAMfs behind a stable trait); CoW + content-addressing + Merkle are post-v1.0 deepening. Captured here: decisions and findings so far.*

# Roadmap position
- **v0.9 — minimal RAMfs.** A RAM-backed filesystem as a userspace component, accessed via capabilities. **The deliverable is the `Filesystem` trait** (`open / read / write / stat` etc.) with a trivial in-memory implementation behind it. Not persistent, no snapshots.
- **Post-v1.0 — FS deepening:** CoW + snapshots, then content-addressed + Merkle. All *additive behind the v0.9 trait* — new methods (snapshot, verify), existing methods stable. This is what makes punting them low-rework: the trait is the expensive part, fixed once at v0.9.
- Target end state: CoW + content-addressed + log-structured + Merkle-verified. Filesystem-as-Git.

# What a filesystem is, below the API
Bookkeeping over a block device. Key structural idea: the **inode**. An inode *is the file* — it holds the file's metadata (size, permissions, owner, timestamps, link count) plus the pointers to its data blocks. A **filename is deliberately not in the inode** — a filename is a directory entry (name → inode number). Decoupling identity from naming is what allows hard links: many directory entries pointing at one inode. (Same identity-vs-name pattern as capabilities, interning, sockets.)

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
