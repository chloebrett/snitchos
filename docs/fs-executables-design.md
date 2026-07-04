# 📦 Executables in the filesystem — design

**Status:** **Design; partly SHIPPED (updated 2026-07-04).** The `fs-image/`
drop-in + build-time ELF injection are real: `kernel/build.rs` builds the `hello`
bins and copies chosen ELFs (`spawnee`, `manifest_demo`) into `fs-image/bin/`, which
`user/fs/build.rs` bakes into the seed — and the seed is now a **3-tuple**
`&[(&str, &[u8], &[(&str, &[u8])])]` carrying content *plus* extracted xattrs (a
program's `.snitch.iface` note → `user.iface`; see
[typed-processes-and-the-data-model-design.md](typed-processes-and-the-data-model-design.md)).
Still design: a *formal declared-executables list* (today it's ad-hoc copies in
`kernel/build.rs`) and running a program *loaded from the FS by the shell* under a
cap (`SpawnImage` exists; the shell doesn't yet). The floor under
[`spawn`](shell-primitives-design.md#proc--spawn--reap-the-delegation-act): before
the shell can meaningfully *run a program*, programs must **live in the
filesystem** rather than be baked into the kernel image. Sits beside
[filesystem-design.md](filesystem-design.md) (the ramfs/cap mechanism),
[shell-primitives-design.md](shell-primitives-design.md) (`hold`/`spawn`/`lookup`/
`view`), and [stitch-test-library-design.md](stitch-test-library-design.md) (the
`Platform` seam).

## The thesis

A real OS keeps executables in the filesystem; the shell **loads and runs them**.
Today "spawnable" means *compiled into the kernel*: every program is a
`ProgramSpec { elf: &'static [u8], … }` (`kernel/src/trap/user.rs`) selected by a
registry id, `include_bytes!`'d at build time. That is a stand-in. The powerbox
`view` demo only becomes honest when the viewer is a genuine file the shell reads
(under a cap) and spawns — not a kernel-embedded stub.

This doc pins **how a program gets from a host source tree into the ramfs, and how
`spawn` runs it from there** — for both **Rust** executables (riscv ELFs) and
**Stitch** programs (`.st`, run by the interpreter). **The FS work comes first**
(§1–2); the kernel primitive (§3) and the run paths (§4) build on it.

---

## 1. The host playground directory → ramfs (the seed, generalized)

This mechanism **already exists** and is the answer to "a host directory I can drop
things into": **`fs-image/`** at the repo root.

- `user/fs/build.rs` walks `fs-image/`, `include_bytes!`s each file into a
  generated `SEED: &[(&str, &[u8])]` manifest.
- `fs-server-seeded` (a distinct binary from the empty `fs-server`) boots
  `RamFs::seeded(SEED)`, so the flat root is pre-populated.
- The `stitch-fs` workload spawns `fs-server-seeded`; the plain `fs` workload stays
  empty (so its tests still assert an empty root).

Today `fs-image/` holds `primes.st`. **Drop any file in and it appears in the
ramfs** of the seeded-fs workloads — this is the playground. It works now for
*static* content: `.st` programs, text, data.

### The gap: Rust executables are build artifacts, not source files

You can't (and shouldn't) commit a built riscv ELF to `fs-image/`. So a Rust
executable needs the build to **compile it for the target and inject its ELF into
the seed** — exactly what `kernel/build.rs` already does for the embedded programs.
Two seed sources, composed into one `SEED`:

| source | what | mechanism | status |
|---|---|---|---|
| **`fs-image/` (drop-in)** | `.st` programs, data, text | `include_bytes!` each file (auto) | ✅ exists |
| **declared executables** | Rust crates → riscv ELF | build crate, inject ELF under a name | new |

**Decision:** keep `fs-image/` as the zero-ceremony drop-in (data + `.st` land
instantly); add a small *declared* list of executables the build compiles and
injects (e.g. `view`). `.st` programs need no build step — they're static files,
already covered by the drop-in. So the playground the user asked for is **live
today for `.st` + data**, and Rust binaries are the build-integrated case.

### FS considerations to settle here

- **Flat for v1 by choice, not necessity.** The ramfs is *already a tree*
  (`Node::Dir` over an inode table); only `create` refusing `NodeKind::Dir` keeps
  it flat. A `/bin` vs `/home` split is ~an hour's change, and **subtree
  confinement then comes free** — `lookup` descends only, there is no parent op,
  and `..` lives in the *shell* as a cap-stack, not in the FS. So hierarchy is a
  cheap, safe additive change; v1 stays flat only for simplicity. Full reasoning:
  [filesystem-design.md §Hierarchical directories](filesystem-design.md).
- **No executable bit — by design.** "Is this runnable?" is **content sniffing**
  (§4), never a permission bit: in a capability OS the `+x` bit is vestigial
  (authority to run = holding a *read* cap to load it; intent/how = sniffing
  ELF-vs-`.st`). Other per-file metadata, when wanted, lives in **inode xattrs**,
  not a mode bit and not an in-band sidecar
  ([filesystem-design.md §File metadata](filesystem-design.md)).
- **Which workloads see the seed.** Only seeded-fs workloads. The shell workload
  (v0.13) should spawn `fs-server-seeded` so the playground *is* the shell's
  filesystem.
- **Size.** ELFs are larger than `.st` text; the seed is `include_bytes!`'d into
  the `fs-server-seeded` image and copied into ramfs at boot. Fine at the handful-
  of-small-programs scale; a real image format is far future.

---

## 2. What "runs" means for each program kind

A Rust program and a Stitch program are **not** symmetric, and conflating them is
the main trap:

- **Rust riscv program = an ELF.** Has an entry point; the kernel maps `PT_LOAD`
  and jumps to it. A first-class process.
- **Stitch program = source.** No entry point the kernel can jump to. It runs
  *inside the interpreter*. To run a `.st` program **as an isolated process** (the
  whole point of `spawn` — a separate cap-domain), you spawn the **interpreter as
  the process**, handed the `.st` file's cap: *interpreter-as-loader*, exactly like
  `python script.py` or a `#!` shebang.

> Running a `.st` program *in-process* in the shell's own interpreter is trivial
> (`:load` already does it) but gives **no separate process and no cap isolation** —
> which defeats the powerbox model. For isolation, `.st` must go through a spawned
> interpreter process.

So there is a small **`stitch-runner`** ELF: the interpreter packaged as a
*non-interactive* spawnable process that, at startup, takes a `.st` file cap +
delegated caps, reads + runs the program (`eval_program_with_backends`, installing
a `RuntimePlatform`/`RuntimeTelemetry`), then exits. It is to `.st` what the ELF
loader is to Rust.

---

## 3. The one new kernel primitive: spawn-from-bytes

Today `Spawn(15)` loads a `&'static [u8]` chosen by registry id. FS executables
need a `Spawn` that loads an ELF from a **runtime buffer the caller supplies**.

**Model (decided): the shell reads the ELF, the kernel loads the bytes.** The shell
holds a read cap to the program file → `lookup` + chunked `read` → an ELF image in
*its own* memory → `SpawnImage(elf_ptr, elf_len, caps)`; the kernel `copy_from_user`s
the image and runs the existing loader on it.

- **Why not "kernel reads the path":** the fs is a *userspace* server reached over
  IPC. Making the kernel an IPC client of userspace to fetch an ELF inverts the
  layering. Keep the kernel out of the fs; the shell is already the fs client.
- **Reuse:** the ELF **loader already exists** (it maps `PT_LOAD` for the embedded
  programs); the new work is "load from a copied-in buffer" + the copy itself. Cap
  delegation also already exists — `Spawn` delegates a handle set today
  (spawn-demo), so `SpawnImage` reuses that translation (shell-table handle →
  child-table cap).

**Proposed ABI** (a new syscall rather than overloading `Spawn`'s id arg):

```
SpawnImage(a0 = elf_ptr, a1 = elf_len, a2 = caps_ptr, a3 = caps_len) -> child_task_id | error
```

- `elf_ptr/len`: the ELF image in the caller's address space (kernel copies it).
- `caps_ptr/len`: an array of the caller's own handles to delegate; the child is
  born holding exactly those (the powerbox grant), nothing ambient.
- Returns the child task id (pairs with the shipped `Wait`/`WaitAny`), or a refusal
  (snitched, like every other syscall denial).

**Cost / open:** copying a possibly-large ELF (the interpreter ELF is the big one)
across the syscall boundary, once per spawn. Acceptable with a size cap (e.g.
≤ 2 MiB, refuse larger) for v1; a shared-memory/region handoff is the optimization
if it ever bites. The compile-time registry path stays for kernel-spawned
infrastructure (`init`, the fs server); `SpawnImage` is the *fs* path.

---

## 4. Resolution: how the shell runs a name

`spawn("view", caps)` in the shell resolves against the fs by **content sniffing**
(no executable bit needed), mirroring how `execve` distinguishes ELF from `#!`:

1. `lookup("view")` → read cap to the program file; read its leading bytes.
2. **`\x7fELF`** → a Rust executable → `SpawnImage(view.elf_bytes, caps)`.
3. **otherwise** → treat as Stitch → `SpawnImage(stitch_runner.elf, caps + view_file_cap)`;
   the runner reads + runs the `.st`.

So the verb layer doesn't care which kind a program is — the native hides it. A
`.st` "binary" and a Rust binary are both just files you `view`/`spawn`.

### The full powerbox flow, with fs executables

`view notes` makes the two authorities explicit:

- the **program** `view` needs a **read cap** (the shell's authority to load
  programs from the fs) — used to fetch the ELF;
- the **data** `notes` is looked up with `READ` → a freshly-minted cap that is the
  **only** authority delegated to the child.

```
∴ view notes
  lookup view          → read cap (load the program)
  lookup notes (READ)  → mint read(notes)              ← the grant
  SpawnImage(view, [read(notes)])  → view#7            ← CapEvent::Transferred on the wire
  wait view#7          → 0 · touched only notes ✓
```

The child is born holding precisely `read(notes)`. That is the powerbox: the
command is the grant, the program comes from the fs, and the delegation is
observable.

---

## 5. Sequencing (FS first)

1. **FS playground / seed generalization.** Confirm + document `fs-image/` as the
   drop-in (works for `.st`/data now); add the declared-executable build path that
   compiles a Rust crate for riscv and injects its ELF into `SEED`. Point the shell
   workload at `fs-server-seeded`. *No kernel work; unblocks "put a program in the
   fs."*
2. **`stitch-runner` ELF.** The non-interactive interpreter-as-loader: takes a
   `.st` file cap + caps, runs it, exits. Reuses the stitch lib. *Lets `.st`
   programs be spawned as isolated processes — testable on host via the existing
   `Platform` fake before any new syscall.*
3. **`SpawnImage` syscall.** Load an ELF from a copied-in buffer + delegate caps.
   The one new kernel primitive. *Now the shell can run a Rust ELF read from the
   fs.*
4. **Content-sniffing `spawn` native + `view`.** The shell reads the program,
   sniffs ELF-vs-`.st`, spawns the right way, delegates the data cap, narrates the
   grant. The headline demo, end to end from the fs.

Steps 1–2 are pure FS/userspace (host-testable, no kernel change); step 3 is the
single kernel addition; step 4 is the verb. This is why FS-first is the right call:
most of the value (programs in the fs, `.st` as spawnable) lands before any kernel
work, and `SpawnImage` is then a small, well-scoped primitive with a clear consumer.

---

## 6. Open questions

- **Subdirectories / `/bin`.** *Resolved:* cheap additive change, confinement
  free, `..` is shell-side — flat for v1 only for simplicity
  ([filesystem-design.md §Hierarchical directories](filesystem-design.md)).
- **ELF copy cost.** Size cap for v1; region handoff if it bites (§3).
- **Executable metadata.** *Resolved:* no `+x` bit (vestigial under caps),
  content-sniffing for run-vs-data, xattrs for any other per-file metadata
  ([filesystem-design.md §File metadata](filesystem-design.md)).
- **`stitch-runner` arg passing.** How the `.st` file cap + any argv reach the
  runner at startup — startup-handle slots (like the fs endpoint today) vs a small
  argv mechanism. Deferred to step 2.
- **Where built executables are declared.** A list in `xtask`/`kernel/build.rs`
  vs a manifest in `fs-image/`. Decide at step 1; lean toward an explicit build
  list (mirrors `USER_PROGRAMS`).
</content>
