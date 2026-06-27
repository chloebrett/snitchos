# The SnitchOS shell — first-iteration primitives

**Status:** **Design (captured 2026-06-28). Pre-implementation.** The concrete
primitive contract for the v0.13 shell. Sits *between* the two existing docs:
[shell-surface-and-tui-design.md](shell-surface-and-tui-design.md) is the
*identity* (powerbox "grant, then watch"; `hold`/`view`/`watch`; the ANSI TUI),
and [plans/spawn-shell-and-console.md](../plans/spawn-shell-and-console.md) is the
*mechanism* (`Spawn`/`Exit`/`Wait`/console — all shipped). This doc answers the
question those two leave open: **what, exactly, are the primitives the first shell
is built from, and how does each one bottom out in a shipped syscall?**

## The three decisions this iteration is built on

1. **Both layers, vertically sliced.** Design the user-facing verbs *and* the
   building blocks under them, with at least one command path working top to
   bottom — not a verb catalogue with no floor, nor plumbing with no verbs.
2. **A Stitch program from day one.** The shell is a Stitch REPL; Rust supplies
   the *effects* as Stitch **natives**. So "shell primitives" resolves to two
   tiers: the **native surface** (the Rust↔Stitch boundary — the real primitives)
   and the **Stitch verb layer** (the shell program, written in `.st`, composing
   natives). This is the convergence the surface doc argues for, taken from the
   start rather than ported to later.
3. **Authority visible now, enforced by grammar later.** v1 *runs* the command and
   *shows* the grant it performed (the `CapEvent`); it does not yet *require* an
   explicit authority clause. The grammar reserves the clause slot so enforcement
   is an additive change (§5).

## The shape falls out of Stitch, not into it

Stitch has **no loop keyword** but **does have `on X` dispatch**. A shell is
*read → dispatch-on-verb → repeat*. So:

- **dispatch-on-verb** *is* Stitch's `on X` — but `on` is **static, type-directed**
  (it dispatches on a value's *type*, not a runtime string), so the verb word is
  first **parsed into a command constructor** (`"view notes"` → `View("notes")`),
  and then `on View { run() = … }` dispatches on that type. The parser is the
  string→type step; `on` is the rest. Each verb is a type with a `run` method.
- **the loop** is recursion (Stitch's only iteration). Known caveat: the tree-walk
  interpreter has no TCO and leaks per run — acceptable for a REPL, noted in §6.

The shell's structure and the language's structure are the same structure. That is
the argument for "Stitch from day one" made concrete, not just aesthetic.

```
                                    ┌──────────────────────────────────────┐
   user types ──► UART RX ──────►   │ Stitch verb layer  (user/shell/*.st)  │
                                    │   on hold { ... }   on view { ... }   │
                                    │   on echo { ... }   on help { ... }   │
                                    └───────────────┬──────────────────────┘
                                                    │ calls
                                    ┌───────────────▼──────────────────────┐
   THE PRIMITIVE SURFACE  ───────►  │ Rust natives  (in the interpreter)    │
                                    │  console · term · caps · proc · fs    │
                                    └───────────────┬──────────────────────┘
                                                    │ wraps
                                    ┌───────────────▼──────────────────────┐
   already shipped  ─────────────►  │ syscalls (abi::Syscall) + FS-over-IPC │
                                    └──────────────────────────────────────┘
```

---

## 1. The native surface — the actual primitives

Each native is a thin Rust function exposed to Stitch, wrapping a syscall (or a
small composition). Grouped by effect. **Status:** `live` = syscall exists today;
`new-native` = syscall exists, binding doesn't; `new-syscall` = needs a kernel
primitive (only one).

### console — bytes to/from the human terminal
| native | signature | backs onto | status |
|---|---|---|---|
| `readLine()` | `() -> Str` | `ConsoleRead`(14) + line discipline | new-native |
| `write(s)` | `(Str) -> ()` | `ConsoleWrite`(19) | new-native |

**Line discipline lives in the native, not in Stitch.** Echo, backspace, and
enter-terminates are fiddly byte work and *host-testable in Rust* (feed a byte
stream, assert the returned line + the echo bytes written). Keeping it below the
language boundary keeps the `.st` shell clean and the discipline under `cargo
test`. `readLine` blocks (polls the RX ring) until a full line; `ConsoleRead`
itself is non-blocking, so the poll loop is the native's job.

### term — Tier-0 rendering (escape bytes)
| native | signature | backs onto | status |
|---|---|---|---|
| `color(s, c)` | `(Str, Str) -> Str` | pure (wraps in SGR) | new-native |
| `table(rows)` | `(Seq) -> Str` | pure | new-native |
| `tree(node)` | `(record) -> Str` | pure | new-native |

These are a **pure, host-testable Rust lib** (model → escape-sequence bytes,
snapshot-tested, no QEMU) exposed as natives — the Tier-0 floor from the surface
doc. They return a `Str`; the verb layer composes then `write`s. Tier-1/2
(in-place redraw, alt-screen) are a later growth path, not v1.

### caps — authority introspection + attenuation
| native | signature | backs onto | status |
|---|---|---|---|
| `hold()` | `() -> Seq<CapInfo>` | **`CapList`(26) — NEW** | new-syscall |
| `attenuate(cap, rights)` | `(cap, Str) -> cap` | `MintBadged`(11) / FS `lookup` | new-native |

`hold()` is the **one missing kernel primitive**. A process can use its caps but
cannot *enumerate its own table* — there is no syscall for "what authority do I
hold?". Startup caps arrive in registers, but the shell *mints more at runtime*
(every `lookup`), so a static startup manifest is incomplete. `CapList` reads the
caller's own `CapTable` and returns, per live slot: `{ handle, object_kind,
rights, badge }`. **This is introspection, not ambient authority** — you learn
only about caps you already hold, so it is ambient like `ClockNow`/`ConsoleRead`,
not a hole in the model. It is also itself a nice cap-OS primitive: *a process can
see its own authority*, which is exactly what `hold` renders.

`attenuate` splits by object kind, and this subtlety is worth stating once:
- **endpoint rights** (SEND/RECV/MINT) narrow via `MintBadged` — a true kernel
  derive.
- **file rights** (READ/WRITE) are *server-interpreted, packed in the badge*
  (ipc-design §"Two rights layers"), so narrowing a file cap is an **FS `lookup`
  mint**, not a kernel derive. `attenuate(dir, "read")` on a file path therefore
  *is* a `lookup` call to the FS server. The native hides which mechanism fires.

### proc — spawn + reap (the delegation act)
| native | signature | backs onto | status |
|---|---|---|---|
| `spawn(program, caps)` | `(Str, Seq<cap>) -> child` | `Spawn`(15) | new-native |
| `wait(child)` | `(child) -> Int` | `Wait`(18) / `WaitAny`(24) | new-native |

**There is no standalone `grant`.** Authority is conferred *at spawn* —
`spawn(prog, [caps])` delegates exactly that set and nothing ambient. "Grant"
isn't a verb, it's the second argument to `spawn`. This is the powerbox model in
one signature: a child is *born* holding precisely what you pass, which is why the
demo trace is clean.

### fs — name resolution (mint-on-lookup)
| native | signature | backs onto | status |
|---|---|---|---|
| `lookup(dir, name)` | `(cap, Str) -> file_cap` | FS IPC `call` (fs-proto) | new-native |

`lookup` is the cap-minting op: the shell holds a dir cap, asks the FS server, and
gets back a freshly-minted badged `(inode, rights)` File cap (ipc-design
§"Cap-transfer on the reply path"). This is where the shell's broad authority
becomes a narrow child-sized authority.

### observe — deferred to a tap
`watch(metric)` / `spans()` are **stubbed in v1**. Telemetry is emit-only today
(no read-path syscall), so a system-wide live pane needs the *telemetry-tap*
capability (surface doc §"telemetry-tap"). v1's only honest observation is the
shell **narrating its own actions** — the grants it performs and the children it
waits on, which it knows directly. That is exactly enough for the demo; the
system-wide tap is earned later.

---

## 2. The Stitch verb layer — first-iteration vocabulary

Four verbs. Two are trivial (prove the loop + arg parsing); one introspects
(proves `hold`/render with no spawn); one is the headline (proves the whole
delegation thesis).

| verb | does | primitives used | new mechanism |
|---|---|---|---|
| `help` | list verbs | `write`, `term` | none |
| `echo <text>` | print args | `write` | none |
| `hold` | render my cap table | `hold()`, `term.table` | `CapList`(26) |
| `view <name>` | run a viewer over a read-only file cap | `lookup`→`attenuate read`→`spawn`→`wait`, narrate the grant | none (all shipped) |

`view` is the demo: `lookup` mints `(name, READ)`, `spawn` hands a fresh viewer
*only* that cap, `wait` reaps it, and the shell prints the grant + a "touched only
X" verdict — backed by the `CapEvent::Transferred` on the wire. Sketch 1 from the
surface doc, made real:

```
∴ view notes
  ├ grant read(notes) → view#7        ← a CapEvent on the wire
  │ buy milk
  │ fix sched bug
  └ view#7 exited 0 · touched only notes ✓
```

(`give`, `watch`, `who can touch X?` are the natural next verbs — deliberately out
of the first iteration. `give` is `view` with `read,write`; the rest need the tap.)

---

## 3. The vertical slice — what ships first, end to end

The "vertically sliced" decision means we don't build the whole native surface
before anything runs. Two slices, each working top to bottom:

**Slice 1 — a breathing terminal (one new syscall).**
`CapList`(26) + the console/term/caps natives + the `.st` REPL with `help`, `echo`,
`hold`. This is unmistakably SnitchOS the moment `hold` prints your authority, and
it needs **no spawn, no FS, no viewer program** — only `CapList`. Smallest
end-to-end proof the architecture works.

**Slice 2 — the demo (`view`, no new kernel work).**
Add the `lookup`/`spawn`/`wait` natives + a tiny `view` viewer program in the
`SPAWNABLE` registry + the `CapEvent::Transferred` emission in `Spawn` (item 5 in
the spawn plan, "mostly free"). Now `view notes` produces the visible
mint→grant→read→exit chain. *That trace is the milestone.*

`init` (spawns the FS server + the Stitch-shell with its session caps) is the
prerequisite for both and is already named as the next `[CP]` item in the spawn
plan.

---

## 4. The startup-cap ABI (how the shell finds its world)

A spawned Stitch program receives its delegated caps as handles in its `CapTable`.
The natives need a defined layout. Convention (extends the existing `a0`=telemetry
/ `a1`=span bootstrap):

- handles `0..k` = the auto-granted bootstrap caps (telemetry, span) — Q-A's
  documented ambient grant.
- handles `k..` = the session caps `init` delegated, **in the order init passed
  them to `spawn`**. For the shell: its FS dir cap, then any others.

`hold()` makes this self-describing — the shell can *render* its own startup world
rather than hard-coding handle indices, which is the point of having `CapList` at
all.

---

## 5. Grammar: visible now, enforced later

v1 grammar is `verb arg*`. The authority-clause slot is **reserved but optional**:

```
view notes                      # v1: runs, shell shows the grant it chose
view notes using read(notes)    # later: clause REQUIRED; spawn caps ⊆ clause
```

The parser accepts (and in v1, ignores/omits) a trailing `using <grant-expr>`.
Enforcement is then a *closed* additive change: make the clause required and assert
the spawned cap set is a subset of what the clause names. No grammar rewrite — the
slot is already there. This is precisely the "visible now, enforced by grammar
later" decision, paid for up front with one optional production.

---

## 6. Known caveats (carried forward, not solved here)

- **REPL leaks per run + no TCO.** The recursive loop grows; the interpreter leaks
  per evaluation ([project_stitch_on_target]). Fine for an interactive REPL;
  flagged so it isn't mistaken for a bug. A bounded-arena or reset-per-line pass is
  future work.
- **Verb dispatch is parse-to-constructor, not `on` on a string** (resolved).
  `on` is static + type-directed (confirmed: `interp.rs` `eval_method_call`), so it
  cannot dispatch on a runtime verb string. The shell parses the verb word into a
  command constructor (`"view" → View(args)`) and lets static `on View { run() }`
  dispatch on the type. No new dispatch mechanism needed; the parser is the only
  string-handling step.
- **`watch`/system-wide observation** waits on the telemetry-tap cap (deferred).
- **No resource quota on spawn** (Q-D) — unchanged; a process can still spawn
  unbounded children. Out of scope.

---

## 7. What this iteration actually asks for (the build list)

In dependency order:

1. **`CapList`(26) syscall** — read the caller's own `CapTable` → `[{handle, kind,
   rights, badge}]`. The one new kernel primitive. Pure decode is host-testable in
   `kernel-core`; the kernel side reads the live table.
2. **Console + term natives** — `readLine`/`write` (line discipline host-tested) +
   the escape-byte render lib (snapshot-tested).
3. **caps/proc/fs natives** — `hold`/`attenuate`/`spawn`/`wait`/`lookup` bindings
   in the interpreter.
4. **`user/shell/*.st`** — the REPL: recursion loop + `on`-dispatch + the four
   verbs.
5. **`init`** — spawn FS server + Stitch-shell with session caps (spawn plan).
6. **`view` viewer program** + **`CapEvent::Transferred` in `Spawn`** — slice 2,
   the demo trace.

Everything except (1) reuses shipped mechanism. The first iteration of the shell
costs **one syscall** plus glue.
```
