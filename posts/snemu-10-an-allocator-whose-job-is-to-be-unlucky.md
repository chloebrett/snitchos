# snemu 10 — an allocator whose job is to be unlucky

[Post 55](post-55-killing-across-cores-and-a-fidelity-hunt.md) ended on a cliffhanger
I left myself: one gate scenario, `supervised` under the release kernel, dropped three
telemetry frames under snemu that QEMU emitted, and I'd narrowed it to "snemu
mis-emulates *some* opt-3 instruction" without knowing which. This is the dig. It went
down further than I expected — to a single instruction straddling a page boundary — and
it came back up with a tool I didn't set out to build: an allocator whose entire purpose
is to be unlucky, deterministically, forever.

## the descent

The bug announced itself three abstraction layers above where it lived, and every step
of the hunt was me translating down one more layer by hand.

- **Layer 0 — telemetry.** `snemu diff` said `supervised --opt mid` dropped
  `supervised.halted`, `svc.crasher.escalated`, and
  `escalate.crasher.intensity-exceeded` — the "escalate trio" the supervisor emits when
  a crash-looping service exhausts its restart budget. snemu ran 112k frames over 6
  seconds and never escalated; QEMU escalated at 1.75s.
- **Layer 1 — the scheduler.** Heartbeats kept flowing in snemu's stream, so its clock
  *was* advancing — which ruled out the busy-wait backoff I first suspected. The
  supervisor wasn't spinning; it was **gone**. The last thing it did on hart 1 was bump
  `snitchos.user.faults_total`. It took a U-mode page fault and the kernel *parked* it
  (`trap/mod.rs:191` — v0.7a has no process teardown, so a faulting task just stops).
  No supervisor, no escalate.
- **Layer 2 — the fault.** I taught snemu to narrate its own U-mode faults (something I
  should have had all along): `cause=13 sepc=0x100012a6 stval=0x10`. A load from address
  `0x10`. A null-pointer-plus-offset dereference — `ld a1, 0x10(s2)` with `s2 = 0`.
- **Layer 3 — the register.** `s2` is a callee-saved copy of an argument the supervisor
  passed into a hashmap lookup: a pointer to a global table at `0x10005000`, built with
  the classic `auipc a0, 0x4; addi a0, a0, 6`. Under snemu, `a0` came out **0**.

So snemu had turned `addi a0, a0, 6` into `a0 = 0`. That's not a scheduling bug or an
MMU bug. That's snemu fetching the wrong *instruction*.

## the instruction that got corrupted in flight

A detector at the fetch site printed the smoking gun:

```
pc=0x10000ffe  pc_pa=0x807d1ffe  hi_pa=0x807d4000  naive=0x513  correct=0x650513
```

The `addi` sits at virtual address `0x10000ffe` — a 4-byte instruction whose last two
bytes spill across the page boundary at `0x10001000`. RISC-V's compressed extension
lets a 4-byte instruction land on a 2-byte-aligned address, so this is completely
legal. But the two halves live on **non-contiguous physical frames**: the low half at
`0x807d1ffe`, and the next virtual page mapped to `0x807d4000` — *not* `0x807d2000`.

snemu's fetch translated the PC once and read four contiguous *physical* bytes. So it
got the low half right and read the high half out of the physically-adjacent frame,
which belonged to some other virtual page — here, zeros. `0x00650513` (`addi a0,a0,6`)
became `0x00000513`, which decodes to `addi a0, x0, 0` — **`li a0, 0`**. The pointer got
zeroed at the moment it was being computed, and everything downstream followed.

It's a pure-interpreter bug — `snemu boot` runs the plain interpreter, no JIT, and hits
it — so all of Backend B and the native-op machinery from [snemu
09](snemu-09-the-fast-part-wasnt-the-native-part.md) were innocent. The oracle itself
was wrong.

## why it hid for 114 green tests

It needs two coincidences at once: a 4-byte instruction parked exactly at a `…ffe`
offset, *and* the two pages on non-contiguous physical frames. The release build's
opt-1-pinned userspace happened to produce both. Every other layout — debug userspace,
the other 114 itest scenarios — either put the instruction somewhere else or got lucky
with a contiguous allocator. The bug wasn't a regression; it was a latent fidelity gap
waiting for the frame allocator to fragment in exactly the wrong way.

Which is a maddening property for a bug to have. "Runs green until the allocator gets
unlucky" means you can't trust green. So the fix and the *proof* of the fix are two
different problems, and the second one is the interesting one.

## the allocator whose job is to be unlucky

If the bug fires only when consecutive pages land on non-contiguous frames, then the
way to stop trusting luck is to **guarantee** non-contiguity — everywhere, every boot.

snemu can't change the guest kernel's frame allocator, but it owns the storage those
frames live in. So I added a deterministic frame permutation to `Memory`: guest frame
`f` is stored at physical frame `(f · k) mod N`, with `k` coprime to `N` and near `N/2`
so adjacent guest frames land far apart. It's a bijection, so the guest is completely
oblivious — every access is remapped uniformly — *except* that "physically contiguous"
is now almost never true. Every straddling access reads its tail from the wrong frame.
And it's fixed-per-size with no RNG, so snemu stays deterministic: the same run scrambles
the same way, and the whole snapshot / state-hash discipline still holds.

The subtle part — the part that took a second try — is that not all memory accesses can
be permuted the same way:

- **Width-typed accesses** (the fetch/load/store path) permute the *base* frame once and
  read contiguously. That's what *preserves* the hazard: a straddling read spills into
  `permute(f)+1`'s storage, which is no longer the guest's `permute(f+1)`.
- **Bulk `write_bytes`** (the ELF loader, DMA) must go **per page** — each guest frame to
  its own scrambled storage frame. Permute a multi-page blob once and you scatter the
  kernel image wrong, and now everything breaks for reasons that have nothing to do with
  the bug you're hunting. The false-destruction trap; I walked into it once before
  splitting the two paths.

Then I turned it on. Same kernel binary, one environment variable:

| | instret before stop | reached boot? | telemetry frames |
|---|---|---|---|
| scramble **off** | 130,694,209 | ✅ `I am alive` → `entering heartbeat` | 5,242 |
| scramble **on** | 334,668 | ❌ **kernel panic** (`slice::get_unchecked` out of bounds) | **0** |

With scrambled frames the kernel can't survive 334k instructions of its own early boot —
because the *kernel* has straddling instructions too, we just never made them land badly
before. That's the whole test suite reduced to rubble by flipping one flag. Exactly what
I wanted: the bug's precondition, weaponised.

## the fix, and the proof

The fix is small and obvious once you've seen the disassembly: when a 4-byte fetch would
cross a page (`pc & 0xfff > 0xffc`), translate `pc + 2` on its own and combine the two
16-bit halves — and if the upper half faults, trap it as an instruction-page-fault, the
way real hardware does. Two fetch sites needed it: the interpreter's slow path and the
block-JIT frontend, which must stay byte-identical to it.

The proof is the tool. With the fix in and scramble still on:

- the scrambled demo boot went from a 334k-instret panic to **270M instret and 12,566
  frames** — a full, healthy boot under the maximally hostile layout;
- the itest suite passes **120/120 both plain and scrambled**;
- and `supervised --opt mid` — the scenario that started post 55 — now reads `0 only-qemu,
  0 only-snemu`. The escalate trio is back.

The scramble is now a standing gate pass: `cargo xtask test && cargo xtask itest && cargo
xtask itest --scramble`. The contiguous run is what real hardware produces; the scrambled
run is the regression guard. The whole *class* of "assumes physical contiguity" bug can't
silently come back, because every commit boots the kernel on a physically shredded RAM
and demands it still work.

## what I learned

- **A green test suite is a claim about luck as much as correctness.** 114 scenarios
  passed not because the fetch was right but because the allocator never fragmented into
  the failing case. The fix for "runs green until unlucky" isn't a better assertion, it's
  *removing the luck* — forcing the worst case to be the common case. The scramble found
  not just this bug but, in principle, its entire family.

- **The debugger fought at the wrong altitude the whole way.** `snemu diff` sees
  telemetry frames; the bug lived in instruction fetch, three layers down. Every step was
  hand-translating between them. The trivialising tools are the ones that let snemu
  observe at the layer the bug actually lives at — and snemu being *my* emulator is the
  superpower a black-box QEMU can't offer: it can narrate its own divergence. Auto-report
  U-mode faults with a symbolised PC; a debug-mode assert that screams when a straddling
  fetch's two halves disagree (the detector that found the bug, promoted to a permanent
  invariant). I added those *after* the hunt. They'd have collapsed a multi-hour dig into
  one panic line.

- **The prevention belongs in a type.** The fetch translated a PC into a bare physical
  address, which erased the page boundary the instant the translation finished — and that
  erasure *is* the bug. Return the physical address *plus bytes-remaining-in-page*, and
  any reader with `width > remaining` is forced to handle the split. The constraint stops
  being something you have to remember.

- **A stress test can create its own false failures.** The per-page `write_bytes` split
  is load-bearing: without it, scramble corrupts the kernel image and every failure is a
  loader artifact, not the hazard. A test harness that can't distinguish "I broke the
  thing I'm testing" from "I broke my own scaffolding" is worse than no harness.

## what's next

- The **data-side straddle** (misaligned loads/stores crossing a page) has the same latent
  shape, but naturally-aligned accesses never cross a page, so scramble is green without
  the fix. Defensive, not urgent — closing it is mostly about the fat-translation-result
  type above, which makes fetch and data share one correct path.
- The debugging-robustness tools — auto-faults, the straddle self-check, a
  snapshot-resume debug mode so each hypothesis costs a second instead of a rebuild — are
  scoped in `plans/snemu-page-straddle-fix.md`. This hunt was the argument for building
  them.
