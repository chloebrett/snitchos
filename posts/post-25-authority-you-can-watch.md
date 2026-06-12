# Post 25 — Authority you can watch

- v0.7b: the ambient syscall from last time becomes a **capability invocation**. The kernel surface collapses to one idea — *invoke a capability you were granted* — and because SnitchOS snitches, every authority decision is on the wire: the grant, the use, and the refusal. A process reaches for power it was never given and gets **refused and reported in the same breath**. Along the way the userspace stopped being a checked-in binary blob and grew a real runtime library, a way to exit, and — finally — the right to be *handed* its capabilities instead of guessing them.

## the ambient sin

- last milestone's syscall was built wrong on purpose. the program did `ecall`, the kernel emitted a metric, no questions asked. any U-mode code could call it. that's **ambient authority**: the power to do a thing is available to anyone who knows the thing exists. it's the Unix model, and it's the thing this milestone exists to kill.
- the before/after is one trap arm:

```
v0.7a:  ecall(EmitMetric, value)        → kernel emits, no check
v0.7b:  ecall(Invoke, handle, value)    → kernel: resolve handle in MY table
                                           → check the EMIT right
                                           → emit, or DENY (and snitch it)
```

- that's the whole idea. "syscalls" stop being a menu of powers and become messages to objects you hold a reference to. seL4's framing; now SnitchOS's too.

## a handle is not a key

- a capability is an unforgeable `{ object, rights }` pair. a process holds a **capability table**, and names a capability by an opaque `u32` **handle** — like a Unix file descriptor, except the kernel validates *every* use against *that process's own table*. `handle 3` means nothing on its own; it's an index into your table and no one else's.
- the load-bearing word is *unforgeable*. holding the integer `3` isn't authority — the kernel checks whether slot 3 of *your* table actually holds a capability with the right you're invoking. a program can name any handle it likes; naming is free, using is checked.
- the whole security decision is a pure function — `resolve the handle → check the right → return the bound resource, or refuse` — which means it lives in `kernel-core` and is **host-tested without a kernel at all**. resolve a granted handle: works. resolve an out-of-bounds handle: `NoSuchCapability`. resolve a handle whose generation is stale: refused, *not* aliased to whatever now lives in that slot. invoke a cap that lacks the right: `MissingRight`. four tests, mutation-clean — the authority logic is proven before it ever touches a CSR.
- the generation tag is the one bit of forward-armour I paid for now: a handle is `index + generation`, not just an index. nothing bumps a generation in v0.7b, but when revocation lands, "bump the slot's generation and every old handle to it fails `resolve`" is the cheapest possible primitive — and retrofitting it into the handle layout later would be the expensive kind of change.

## both pillars, one counter

- here's where it stops being a generic microkernel and becomes *SnitchOS*. capabilities are about authority; observability is about watching. the two pillars meet at a single idea: **every authority decision is an event.**
- the kernel snitches when it **grants** — `snitchos.cap.grants_total`, plus a first-class `CapEvent` frame carrying the global cap id, the holder, the object kind, and the rights. it snitches when it **denies** — `snitchos.cap.denied_total`, bumped from the trap handler the instant an ungranted invocation is refused. grants *create* authority; invocations merely *spend* it; so the grant stream is the richest, most security-relevant signal on the box.
- the cap twin of last milestone's firewall: there, a U-mode load of a kernel page faulted and the kernel counted it. here, `hello` invokes a handle it holds (works), then deliberately invokes handle 1 — which its table doesn't contain — and the kernel refuses and counts `denied_total`. *userspace reached for authority it didn't have, and the page table couldn't have stopped it — only the capability table could.*
- and the `CapEvent` frame is shaped for something bigger than a counter. it carries a **global** cap id (distinct from the per-process handle) and a `parent_cap_id` — `0` for now, because nothing is derived yet. the kernel never stores a tree; it emits the events and the **collector reconstructs the capability derivation tree host-side**, exactly the way Tempo reconstructs a trace from span start/end frames. the kernel stays microkernel-thin; the security policy becomes a view you can scrub through in Grafana. that view is a v0.8 deliverable — at v0.7b the "tree" is a single root — but the frame that makes it possible is on the wire today.

## the handle was a lie

- there was one cheat left, and it bugged me. the program named its capability with a **constant**: `TELEMETRY_SINK_HANDLE = 0`. the kernel granted the sink into a fresh, empty table so it landed at slot 0; the program hardcoded 0; both sides "agreed." but they never *communicated* — they shared an assumption about insert order, dressed up as a constant. the enforcement was real; the **handing-over was faked**.
- it works for exactly one well-known capability and falls apart the instant there are two. so: the kernel now **hands** the process its capability. `enter` sets `a0` to the granted handle right before `sret`; the crt0 forwards `a0` straight into `rust_main(startup: Startup)`; the program calls `startup.telemetry().emit(...)`. neither side hardcodes a slot — grant the cap anywhere and the program uses what it was *told*.
- I teeth-checked it the only way that convinces me: fed `enter` the wrong handle and watched the telemetry vanish. if the program had been quietly hardcoding 0, it would've kept working. it didn't — it genuinely reads `a0`. the lie is gone.
- this is older than it looks. it's seL4's `seL4_BootInfo`, and structurally it's **Linux's `auxv`** — the kernel writes startup facts onto a new process at entry and then forgets them; the runtime captures them. (env vars are the same shape one layer up: `envp` is a blob the kernel copies onto the new stack at `execve` and never tracks again; `getenv` is pure libc over it. there is no `getenv` syscall.) one register carries one handle today; when capabilities multiply, `a0` becomes a pointer to an in-memory `BootInfo` page — but the program-facing `Startup` type doesn't change, so the programs won't either.

## the side quests that earned their keep

- **the userspace builds itself now.** the kernel used to embed a *checked-in* ELF of the user program, with `xtask` orchestrating a pre-build. that's a stale-binary trap waiting to happen — and last milestone it sprang, running the wrong kernel for an hour. so the kernel's `build.rs` now compiles the user programs itself (into an isolated target dir, so it doesn't deadlock on cargo's build lock) and **hard-fails** if they don't build. no checked-in binaries, no pre-step. I teeth-checked the rebuild tracking too: edit the program, the kernel re-embeds, the test catches it. a build that ignores its own exit code is worse than no build; this one can't.
- **a runtime library.** every program was hand-rolling the same `_start`, the same panic handler, the same inline `ecall`. that's `snitchos-user` now — a crate that owns the crt0, the panic handler, and a **capability-shaped** API: you hold a typed `TelemetrySink`, not a file descriptor; `from_raw_handle` is how you *reach for* authority and get refused. `hello` collapsed to its actual logic: emit through the cap you hold, reach for one you don't, exit.
- **the program can exit.** it used to `loop { spin }` forever, pegging a core. now it makes its syscall and calls `Exit` — the kernel marks it done and hands the hart back to its idle loop, which `wfi`s. honest, too: `hello` has exactly one job, so "I'm finished" beats "spin to stay alive." the payoff was concrete — the userspace tests stopped being CPU-bound and started fanning out in the parallel pool.

## what i learned

- **a capability is a file descriptor that the kernel doesn't trust you about.** the fd number is meaningless; the kernel's table is the truth. once that clicks, "ambient vs. capability" stops being philosophy and becomes "is the power a global verb, or a reference you were handed."
- **the richest thing to observe isn't the operation — it's the grant.** anyone can log what happened. logging *who was allowed to make it happen, and where that permission came from* is the security story, and it's a graph the host can rebuild from events the kernel never stores.
- **provenance hides in constants.** `TELEMETRY_SINK_HANDLE = 0` looked like an ABI detail and was actually a lie about who told whom. the fix — pass it in a register — is three lines, but the lesson is that "both sides agree on a constant" and "one side told the other" are different systems that happen to behave the same until they don't.
- **the door down is the door back, run backwards — still.** last milestone it was `sret` into userspace. this milestone, `a0` rides that same forged trap-return to carry the program its first capability. the entry sequence keeps paying dividends.

## what's next

- **v0.8: IPC over capabilities.** two processes, endpoints, and the first time a capability *moves* — granted from one process to another through a channel. that's when `parent_cap_id` stops being `0`, the derivation tree grows real edges, and the Grafana node-graph that reconstructs it finally earns its keep. the `CapEvent` frame is already shaped for it; the `BootInfo` page replaces the single register; the runtime grows an `Endpoint` to sit next to `TelemetrySink`.
- v0.7b was the milestone where the second pillar landed and the project's identity finished crystallizing: a capability-secured kernel where you can watch authority be born, spent, and refused. the syscall layer got rewritten, and the rewrite has a *before* to point at — which was the whole reason to build it wrong first.
