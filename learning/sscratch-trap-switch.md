# sscratch trap stack-switch — session note + cheat sheet

*Round 1 (v0.7a Step 1 prep). Reached Analyze/Evaluate level — learner
derived the full entry sequence and the security motivation from first
principles with light Socratic prompting. Strong session; minimal
scaffolding needed.*

## The problem (why sscratch must exist)

On a trap **from U-mode**, `sp` holds the *user* stack pointer — an
attacker-chosen value. The first real instructions of trap entry
(`addi sp,sp,-288; sd ra,0(sp)`) would **store** kernel register values
to that address.

- A trap touches **no GPRs** — `ra`, `sp`, etc. still hold the user's
  values verbatim. So the attacker controls the **address** (`sp`) *and*
  the **value** (`ra`).
- The store runs in **S-mode**, so the `U` permission bit does **not**
  constrain it. Target can be kernel page tables, kernel stacks, a saved
  return address — anything mapped.
- ⇒ a kernel **write-what-where** primitive (strong form, both halves
  attacker-chosen) → overwrite a function pointer / return address →
  kernel control-flow hijack. Game over.

The deadlock: to point `sp` at a kernel stack you need the kernel sp
*in a register*, but every GPR holds a live unsaved user value. Need a
facility that (a) holds a known-good kernel stack and (b) is unreachable
from U-mode. → the **`sscratch`** CSR.

## The mechanism

`csrrw sp, sscratch, sp` — **atomic swap**, not a copy. One instruction:
`sp ← sscratch`, `sscratch ← old sp`. Nothing destroyed; the user sp is
parked in `sscratch`.

**Convention:** `sscratch == 0` while in the kernel; `sscratch == this
thread's kernel sp (KSTK)` while in user.

| Trap from | before swap | after `csrrw sp,sscratch,sp` | tell |
|---|---|---|---|
| **User** | sp=USTK, sscratch=KSTK | sp=KSTK, sscratch=USTK | sp ≠ 0 |
| **Kernel** | sp=KSTK, sscratch=0 | sp=0, sscratch=KSTK | sp == 0 |

The swap is **unconditional** (first instruction). The branch tests the
*result*: `sp == 0` ⇒ came from kernel ⇒ **undo** by running the *same*
`csrrw` again (it's **self-inverse**), which also restores `sscratch=0`
(convention self-heals).

```asm
trap_entry:
    csrrw sp, sscratch, sp   # unconditional swap
    bnez  sp, 1f             # sp != 0 -> from user (sp = KSTK, sscratch = USTK)
    csrrw sp, sscratch, sp   # sp == 0 -> from kernel: undo (sp = ksp, sscratch = 0)
1:
    addi sp, sp, -288        # safe on both paths now
```

Because the kernel path is transparent, **every existing S-mode trap
flows through unchanged → the existing itest suite is the regression
gate for the rewrite** (no fake-U context needed).

## Not yet covered (Step 1 implementation will need)

- **Exit / `sret` side:** before returning to user, `sscratch` must be
  set back to `KSTK` so the *next* trap from user works. (On the
  from-kernel path it's already 0 — correct.)
- **Saving the user sp into the frame:** on the from-user path the user
  sp lives in `sscratch` after the swap; it must be read out and stored
  into the trap frame's sp slot (offset 8), and restored symmetrically.
  Today's code computes "caller sp = sp + 288" — that stays correct only
  for the from-kernel path.
- **Boot wiring:** set `sscratch = 0` per hart at boot (both harts).

## Open Feynman check (self-test)

"Why does `sscratch` hold zero in the kernel, and what breaks if we
leave whatever was there from the last trap?" (Answer: 0 is the
unambiguous, cheap-to-test sentinel that means 'already on a kernel
stack'; a stale nonzero value would make a kernel trap mis-detect as
from-user and 'switch' onto a bogus/previous stack.)

## Links

- `docs/v0.7-userspace-concepts.md` §2 (prose version)
- `plans/v0.7a-first-userspace.md` Step 1
- `kernel/src/trap.S` (the code being rewritten)
