# snemu 4 — the bottleneck wasn't where the map said

- three posts in, snemu is *correct*: it boots the real kernel on two harts, and a differential oracle spent two of those posts proving its telemetry matches QEMU's byte for byte. what it isn't, is fast. it's a tree-walking interpreter — fetch, decode, execute, one instruction at a time — and QEMU on this machine is a JIT that translates blocks of RISC-V into native arm64. the whole point of snemu (post-03's promise) is to run the test suite *on it instead of QEMU*, and a suite that runs is only worth running if it doesn't crawl. so this post is about making snemu faster. it's mostly about being wrong about *where* it was slow — the textbook has a name for the first thing you build, and the profiler had a different name for the thing that actually mattered.

## measure first, or you're just decorating

- the project has one rule it applies to itself relentlessly: **measure first, then tune what you measured.** the kernel tunes its heap watermark against heap metrics; snemu should tune its interpreter against its own numbers. so before touching the hot loop I made snemu measure itself: `cargo xtask snemu-bench` runs a workload N times and reports **guest MIPS** — instructions retired over wall-clock — plus startup and a spread.
- MIPS is the honest number *because snemu is deterministic*: same workload, same seed, identical instruction count every run. only the wall-clock moves. so the harness enforces it — if two runs report a different instret, that's a determinism bug and the report refuses to average it into a lie. QEMU can't give you this: nondeterministic, no fixed instret, so there's no apples-to-apples "how many instructions per second" to put beside it.
- first number: **~20 MIPS.** and a taxonomy — startup-bound, compute-bound, memory-bound, trap-heavy — came back nearly *flat*: ~19–20 across every class. that flatness is a finding. if the cost varied with the instruction mix, some class would stand out. it doesn't, which says the per-instruction cost is **dispatch overhead**, not the work of any particular instruction. the interpreter spends its time *getting to* the instruction, not running it.

## the textbook move

- flat, dispatch-bound cost is exactly what a **decode cache** is for — JIT Tier 1, the first rung of every emulator-speedup ladder. the interpreter re-does the whole fetch pipeline for *every* executed instruction: a three-level Sv39 page walk to translate the PC, a byte fetch, a compressed-instruction expansion — before it even dispatches. in a loop that runs the same instructions a million times, that's pure waste. cache the decoded instruction by its PC, and a re-execution skips straight to dispatch.
- I built it behind an on/off flag on purpose. the interpreter stays the oracle: with the cache off, snemu runs the pure path that three posts certified; with it on, it had better produce *byte-identical* telemetry. the flag is what lets me prove that — and it forces clean factoring, because the two paths have to agree. a differential check (`--verify-cache`) runs every taxonomy workload both ways and asserts the frame streams match to the byte. they do.
- and the cache made snemu **slower.** 20 MIPS down to 15.

## the first surprise: the cache costs more than the walk

- a decode cache that loses to no cache is a cache whose *lookup* costs more than what it saves. mine was a `HashMap<u64, _>` keyed by PC — and Rust's default hasher is SipHash, built to resist hash-flooding attacks, which is the last thing a per-instruction lookup needs. hashing a program counter with SipHash, every instruction, cost more than the page walk it was meant to skip.
- the fix is to stop hashing. a **direct-mapped array**, like a real CPU's cache: index straight into a slot with `(pc >> 1) & mask`, no hash function at all, and a per-slot tag + an epoch counter so a `satp` change or an `sfence` invalidates everything in O(1). now the fast path is a shift, a mask, an array read, a compare. **21 MIPS** — finally faster than no cache. but only 1.15×. the textbook promised 2–4×. something else was eating the win.

## the second surprise: the clock was in the hot path

- I looked at what my fast path *still* did on every instruction, and there it was: it read `satp` — to notice an address-space switch and flush a stale cache — and `satp` lived in a `BTreeMap`. every instruction paid a tree lookup just to ask "did the address space change?", a question the answer to is "no" ~99.99% of the time.
- so I stopped asking per-instruction. the cache flushes on the two events that *change* translations — a write to `satp`, and `sfence.vma` — and between them, every cached entry is valid by construction. the fast path drops the `satp` read entirely; it's a single array probe now. **26 MIPS.** 1.44×. better. and it pointed a finger.

## the real bottleneck: a tree where a table belonged

- if reading *one* CSR from a `BTreeMap` every instruction was worth 5 MIPS, what about the *other* per-instruction CSR reads? there's a big one hiding in plain sight: before every fetch, the interpreter checks for a pending interrupt, and that probes `sip`, `sie`, `sstatus` — two or three `BTreeMap` lookups, on the pure path *and* the cached one, every single instruction.
- the CSR file held ten registers in a `BTreeMap<u16, u64>`. ten. a tree — with its comparisons and pointer-chasing and cache-missing — to store ten numbers that a `match` on the address maps to a flat-array index. so I replaced it: `[u64; N]`, indexed by a jump-table `match`. the read/write signatures didn't change, so nothing outside the CSR file moved. one file.
- the interpreter — cache *off* — went from 18 to **41 MIPS. 2.25×, from a change to how ten integers are stored.** that one edit beat the entire decode cache. with the cache on too: **52 MIPS.** 2.9× over where I started, and most of it had nothing to do with decoding.

## what I learned

- **the map is not the territory, and the tier ladder is a map.** "Tier 1 is a decode cache" is real, hard-won emulator wisdom — for interpreters whose decode is the bottleneck. mine wasn't. its bottleneck was the boring plumbing every instruction touches: how the CSR file is stored. I'd have built the decode cache and shipped a 1.15× and called Tier 1 done, if the spine hadn't shown me the interpreter itself had a 2.25× sitting in a data-structure choice. measure-first didn't just tune the thing I set out to build — it told me I was building the wrong thing.
- **a cache is a bet that lookup is cheaper than recompute, and you have to check you won the bet.** the SipHash cache is the whole lesson in miniature: I added a cache and got a slowdown, because I never priced the lookup. "add a cache" is not a speedup; "add a cache whose lookup costs less than the miss" is, and those are different claims that need different evidence.
- **the boring data structure is where the interpreter lives.** a `BTreeMap` for ten CSRs is invisible in a code review — it's correct, it's idiomatic, it reads fine. it's also on the path of every instruction the machine executes, and at ~50 million instructions a second the constant factor *is* the program. the hot loop of an emulator has no boring lines.
- **keep the flag.** running the cache behind an on/off switch, with a differential check that the two paths emit identical telemetry, is what made all of this safe to iterate on. every speed experiment was one flag flip away from the certified interpreter, and "did I break correctness" was never a question — the oracle answered it every time.

## what's next

- I deferred the *actual* Tier-1 finish — pre-decoding each instruction into a dispatch-ready form so `execute` skips the opcode match too — because after the CSR fix, dispatch is a cheap jump-table and the measurement says it isn't the cap anymore. it's there if a later tier needs it; the harness to prove it faithful already exists.
- snemu is ~2.9× faster and still correct to the byte. fast enough, finally, to do the thing post-03 promised and post-02 designed toward: stop running the kernel test suite on QEMU and run it on snemu. next post, it earns its keep — and the number that matters turns out not to be MIPS at all.
