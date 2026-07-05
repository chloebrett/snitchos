# Adversarial kernel review — Fable, 2026-07-05

*Scope: `kernel/` + `kernel-core/` authority path (caps, IPC, syscalls, sched,
mmu, trap), judged against `capability-system-design.md`, `ipc-design.md`,
`supervision-design.md`, and the seven-questions doc. Brief targeted grep
confirmation; no build/run. Confidence is marked per finding: **verified** =
read the exact code path; **latent** = correct today, breaks under a named
future change; **observation** = design-granularity note, not a defect.*

The house pattern the seven-questions doc already named — *documented invariants
run ahead of enforced ones* — is real and I found concrete instances. The two
that matter most both cut against the project's own thesis (capabilities +
observability), so they're worth fixing before the axes build on them.

---

## F1 — Caps transferred over IPC lose their derivation identity (High, verified) — **reply-mint half FIXED 2026-07-05**

**FIXED (2026-07-05), reply-mint half only:** `reply_handle_for`
(`kernel/src/syscall/ipc.rs`) now mints one `cap_id`, stores it via
`insert_once_with_id`, and emits that same id — so the reply cap's
`CapEvent::Transferred` names an id the kernel actually holds (until the reply
consumes it) instead of a fresh id matching no slot. No new host test: the
mechanism (`insert_once_with_id`) is already host-tested in `kernel-core`, and the
bug is not wire-distinguishable (both versions emit *a* cap_id) — verified by
build. **The handout half remains open** (a badged cap moved to a client via
`Reply`/`take_reply` still lands with `cap_id 0` because `StashedReply` carries a
bare `Capability`); closing it means threading `cap_id`+`parent_cap_id` through the
transfer, the same "carry the whole holding across the boundary" fix F6 pointed at.

**The defect.** A capability handed to another process through `Reply`
(cap-in-reply, the badge-handout pattern) or the reply-cap mint lands in the
recipient's table with `cap_id = 0` (the root/unassigned sentinel), because the
transfer stores only the `Capability { object, rights }` and re-`insert`s it:

- `kernel/src/syscall/ipc.rs:216` — `handle_call` resume: `proc.caps.lock().insert(cap)` → `grant(cap, Persistent, 0, 0)`.
- `kernel/src/syscall/ipc.rs:151-154` — `reply_handle_for`: `insert_once(...)` → `cap_id 0`.

`cap_id` lives on the `Slot`, not on `Capability` (`kernel-core/src/user/cap.rs:224-239`),
so it is **dropped at every process boundary crossing**. Meanwhile the wire frame
emitted for the same transfer uses a *fresh* `next_cap_id()`
(`ipc.rs:222`, `ipc.rs:156`) that is never stored anywhere.

**Two consequences, both on-thesis:**

1. **Unrevokable orphans.** `Revoke` walks the derivation tree by `cap_id`
   (`sched::revoke_descendants_of`), and `revoke_by_cap_id` treats `0` as a no-op
   by design (`cap.rs:612-614`). So a badged `SEND` cap a server mints and hands
   to a client — *the single most common capability-distribution pattern in the
   system* (fs-client, badge-handout) — is invisible to revocation the moment it
   reaches the client. `supervision-design.md` D3 leans on "a client's minted
   `SEND` cap still names the same endpoint object" surviving restart; that part
   holds, but the claim in the cap docs that authority forms one revocable tree
   does **not** hold across a reply.

2. **The snitch lies.** `CapEvent::Transferred` reports a `cap_id` that no live
   holding carries. A host-side reconstruction of the derivation tree (the Q7
   "snitch on the snitch" check) would show a node that (a) revocation cannot act
   on and (b) does not correspond to any kernel slot. For an observability-first
   OS this is the worst kind of bug: the telemetry is *confidently wrong*, and
   nothing tests it (all wire tests are round-trips — Q2 already flagged this).

**Why it matters now, not later.** This is exactly the axis-3 (differential
observability) target: the kernel's self-report disagrees with its own state. It
also blocks axis-6 (checkpoint = re-delegation by provenance) since the provenance
edge is severed at every IPC hop. The code comments (`ipc.rs:157`, `ipc.rs:223`)
say "not tracked yet," which undersells it — it's not *untracked*, it's
*actively misreported*.

**Fix shape.** Carry `cap_id` (and `parent_cap_id`) alongside the `Capability`
through `StashedReply` / the delegate path, and re-`insert_with_id` on the
receiving side so the stored id equals the wire id and the parent edge points at
the source holding. The cap-id spine already exists; it just isn't threaded
through the IPC transfer sites.

---

## F2 — Ambient kernel-object creation is unbounded and leaks on exit (Med-High, verified)

`EndpointCreate` (`ipc.rs:365` → `ENDPOINTS.push`) and `NotifyCreate`
(`sched.rs:1227` → `NotifyTable::create`) are **ambient** (no cap required) and
**unmetered**. `ENDPOINTS` is an append-only `Vec`; `NOTIFY` is an insert-only
`BTreeMap`. Neither is ever removed from, and `reap_task` (`sched.rs:1266`)
reclaims the address space, `Process`, and stack but **not** endpoints or
notifications the process created.

Two problems:
- **Leak on churn.** A shell that repeatedly spawns programs which each
  `EndpointCreate` (init itself does this per the supervision design) leaks one
  kernel endpoint per spawn, forever. This bites the *supervisor* use case
  directly.
- **Trivial DoS.** `loop { EndpointCreate() }` from any userspace process grows
  kernel heap without bound → OOM. `MapAnon` is bounded per-process
  (`HEAP_MAX`, `mem.rs:33`); object creation has no equivalent ceiling.

This is the concrete cash-out of Q3 #3's "ambient authority re-accumulating" and
"creation has no quota." Worth a resource cap and reclaim-on-exit before the
shell makes spawn-per-command routine.

---

## F3 — No wait-channel discipline; rendezvous blocking assumes the wake was its own (Med, latent)

`block_current()` / `wake(id)` (`sched.rs:1095`, `1118`) carry no *reason*. Each
blocking syscall parks and, on resume, reads a side mailbox assuming the wake
corresponded to its own event. Critically, the **rendezvous** paths do **not**
re-validate — they take whatever the mailbox holds, defaulting to zeros:

- `receive_into_frame` (`ipc.rs:103-108`): `block_current()` then
  `take_delivered(ep, me)`, which `unwrap_or_default()`s — a spurious wake returns
  a **zeroed message as a successful receive** (`frame.a0 = 0`).
- `handle_call` (`ipc.rs:204-209`): `block_current()` then `take_reply(me)`,
  `unwrap_or(empty)` — a spurious wake returns a **zeroed reply**.

Contrast `handle_wait` / `handle_wait_notify` (`process.rs:45`, `notify.rs:97`),
which **loop and re-check** their condition. So the discipline is inconsistent:
the reap/notify waiters are robust to a stray wake; the IPC rendezvous ones are
not.

**Why it's safe today (and why that's fragile):** correctness rests on two
unenforced invariants — (a) a task is in exactly one blocking syscall at a time,
and (b) the six `wake()` sites are partitioned so a blocked receiver is only ever
woken by a matching sender, a caller by its reply, a `Wait`-blocker by its
child's exit, a notify-waiter by a signal. I verified the six sites hold that
partition today. Nothing in the code *enforces* it.

**The corner being painted:** `supervision-design.md` v2 explicitly wants
**`Kill`** (wake/terminate another task) and **timed wait** (wake on deadline).
Both introduce wakes not keyed to a rendezvous — the first thing that can wake a
task blocked in `receive` for a reason *other than* a delivered message. The
moment that lands, the non-looping rendezvous paths silently return zeroed
payloads as success. Recommend giving `block_current` a wait-reason (or making
the rendezvous paths loop-and-recheck like the others do) **before** Kill/timed
wait, not after — a silent-wrong-value bug is exactly the kind the oracle catches
late and expensively.

---

## F4 — The reply cap conveys whole-address-space R/W of the caller (Low, observation)

`CopyFromCaller` / `CopyToCaller` (`transfer.rs`) authorize on a borrowed reply
cap, then copy between arbitrary VAs, bounded only by per-page `R|U` / `W|U`
checks in `copy_across`. That means a server you `call` can read **any**
readable page and write **any** writable page in your address space for the
duration of the call — not a designated buffer. This is option-D by design and
memory-safe (the `U`-bit check prevents reaching kernel memory), but it's
broader than the "borrow" framing suggests: every RPC hands the callee total
read access to the caller's secrets. Flagging because axis-4 (observable IFC)
will have to reckon with it — a taint-tracking story can't treat a `call` as a
narrow channel when it's a whole-AS window. No action needed now; note it in the
IFC design when that lands.

---

## F5 — `Scheduler.tasks` doc claims an invariant `reap_task` violates (Low, verified)

`sched/mod.rs:379-380` documents the task table as "indexed by their position in
this vec. `id.0` equals `tasks[i].id.0`; **the vec is never reordered**." But
`reap_task` (`sched.rs:1275`) does `sched.tasks.swap_remove(idx)`, which reorders.
Nothing today indexes tasks by position (`current_task_arg`, `wake`,
`address_space_of`, `prepare_switch` all `.find(|t| t.id == ...)`), so it's not a
live bug — but the comment is a trap: the next person who writes `tasks[id.0]`
trusting it will read the wrong task after any reap. Fix the comment (or, if the
position==id invariant is ever wanted, it's already lost).

---

## F6 — Delegation drops `Multiplicity`; `Once` caps become `Persistent` across a process boundary (Med, verified) — **FIXED 2026-07-05**

*(Surfaced on a second pass — ranks with F1, above F4/F5.)*

**FIXED (2026-07-05):** `delegate()` now checks `multiplicity_of` per handle and
refuses the whole set (`CapError::NotDelegable`) if any names a `Once` cap —
all-or-nothing, like an unheld handle. A server can no longer smuggle its reply-cap
handle into a `Spawn` delegate array. TDD: `delegating_a_once_cap_is_refused` +
`a_once_cap_refuses_the_whole_delegation_set` (RED confirmed, then GREEN); all 444
kernel-core tests pass. Chose *refuse* over *preserve multiplicity* (see below);
the deeper "carry the whole holding across the boundary" fix that F1 also needs is
still open for the handout path.

`delegate()` (`kernel-core/src/user/cap.rs:410`) copies a `Capability` by value
regardless of its slot's multiplicity, and every receiving-side insertion uses
`insert_with_id` → `grant(cap, Multiplicity::Persistent, …)` (`trap/user.rs:759`).
Multiplicity lives on the `Slot`, not the `Capability`, so it **never travels with
a transfer** — a `Once` cap, delegated, is reborn `Persistent` in the child.

The only `Once` cap today is the reply cap, which makes the consequence concrete:
a server that places its reply-cap handle (received in `a5`) into a `Spawn`
delegate array hands its child a reply cap that can be invoked **repeatedly**, and
now two processes hold reply authority for the same blocked caller. This breaks
the affine invariant the cap docs assert ("holding it *is* the authority…
answering consumes it") and chains into F3: the second, out-of-band `reply`
`wake()`s the original caller, which may since have re-blocked on something else
and now takes a spurious wake / a clobbered `REPLIES` entry.

Same root cause as F1 — properties stored on the `Slot` (`cap_id`, `multiplicity`)
are lost the moment a cap crosses a process boundary. F1 loses *identity*
(observability + revocation); F6 loses *multiplicity* (a clear model-invariant
violation). Fix them together: the transfer path should carry the full holding
(cap_id, parent_cap_id, multiplicity), not a bare `Capability`.

## F7 — 16-bit generation counter wraps, defeating the stale-handle guard (Low, theoretical)

`Handle` gives 16 bits to the generation, bumped `wrapping_add(1)` on `consume`
(`cap.rs:590`). After 2^16 consume/reuse cycles on one slot the generation wraps,
and an ancient stale handle can alias the current occupant — the exact ABA the
generation exists to prevent. Currently unreachable for harm: the only
consumed-and-reused cap is the reply cap, whose holder (the server) discards the
handle immediately rather than retaining old ones. But a long-lived server churns
a slot through full wraps, so the guard is one design change away (a second `Once`
object, or any retained `Once` handle) from mattering. Widen the field or note the
bound.

## F8 — `MapAnon` never reclaims within a process (Low, semi-documented)

`heap_top` is monotonic (`syscall/mem.rs:59`); a userspace free can't return
frames to the kernel, so a long-lived churning process pins up to `HEAP_MAX`.
Bounded per-process, and the code already flags disjoint-placement + unmap as
future work — a noted limitation more than a defect.

## What I'd keep (the good bones)

- **The pure/kernel split is real and pays off.** `on_send`/`on_receive`,
  `pick_next`/`aged_priority`, `on_wait`/`on_exit`, `watermark_grow_decision`,
  `address_space_switch` — the policy is host-tested away from the asm/CSR/MMIO,
  and the tests actually pin behavior (the `on_cpu_delta` wrap sentinel, the
  aging saturation, the rendezvous never-both-waiting invariant). This is the
  single best structural decision in the codebase.
- **The cap resolver is clean and total.** `resolve` checks bounds → generation →
  liveness, never hands back the wrong cap, never panics; `Denied` vs `CapError`
  is the right split. F1 is a *transfer-site* bug, not a resolver bug.
- **Lock discipline is genuinely careful.** The "never hold a `Mutex` across a
  switch" rule is honored at every rendezvous site I checked, the `SCHEDULER → caps`
  order is consistent, and the reasons are written down where they're non-obvious
  (the `reap_task` lock-release-before-drop, the `revoke_descendants_of` ordering).
- **The generation/tombstone machinery** was built before revocation needed it and
  slotted in cleanly — the "cheap now, expensive to retrofit" call paid off.

**Caveat (the honest one):** F1/F2 are visible precisely *because* the first pass
works end-to-end — they're second-pass findings, the cost of getting mint,
handout, reply, and revoke each working in isolation before making them compose.
None is a crash or a memory-safety hole; all four functional ones are
"enforcement lags the documented model," which is the pattern the project already
knows about. The value here is that F1 and F3 are the two that will bite a
*specific named future milestone* (revocation-in-practice, and Kill/timed-wait),
so they're worth pulling forward.
