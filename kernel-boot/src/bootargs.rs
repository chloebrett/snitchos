//! Kernel command-line (`/chosen/bootargs`) parsing — pure logic.
//!
//! The kernel selects its boot workload at runtime from a `workload=`
//! key in the bootargs string QEMU passes via `-append`. Parsing is
//! pure and host-tested here; `kmain` reads the raw string from the
//! DTB and feeds it in. See `docs/runtime-workload-selection-design.md`.
//!
//! There is no table of bootarg names: a workload's name *is* the kebab-case of
//! its [`WorkloadKind`] variant, derived at lookup time by `kebab_eq`. Adding a
//! workload means adding one variant — nothing else to keep in sync. Variants are
//! declared in sorted order, which the `workload_variants_are_declared_in_sorted_order`
//! test enforces by reading this file's own source.

/// Declare the workload registry. Expands to the enum as written, plus an `ALL`
/// table pairing each variant with its own identifier (via `stringify!`) and a
/// `from_kebab` lookup over it.
///
/// The point is that the identifier is the *only* place a workload is named — the
/// bootarg spelling is derived from it — so an enum and a name list cannot drift
/// apart, because there is no name list. The invocation must derive `Copy`.
macro_rules! workloads {
    (
        $(#[$enum_meta:meta])*
        pub enum $enum_name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident, )+
        }
    ) => {
        $(#[$enum_meta])*
        pub enum $enum_name {
            $( $(#[$variant_meta])* $variant, )+
        }

        impl $enum_name {
            /// Every variant paired with its identifier. Generated from the
            /// declaration, so it cannot omit one.
            const ALL: &'static [($enum_name, &'static str)] =
                &[ $( ($enum_name::$variant, stringify!($variant)), )+ ];

            /// The variant whose identifier kebab-cases to `name`.
            fn from_kebab(name: &str) -> Option<Self> {
                Self::ALL
                    .iter()
                    .find(|(_, ident)| kebab_eq(ident, name))
                    .map(|(kind, _)| *kind)
            }
        }
    };
}

/// True when the `PascalCase` `ident` kebab-cases to `name` — `SmpSpscBatch`
/// matches `smp-spsc-batch`. Each interior capital must be met by a `-`, so
/// `Smp` does not match `smp-spsc` and digits stay attached to the word they
/// follow (`UserOnHart0` is `user-on-hart0`, not `user-on-hart-0`).
///
/// Compares in one pass rather than building the kebab string: this crate's
/// production code doesn't allocate.
fn kebab_eq(ident: &str, name: &str) -> bool {
    let mut name = name.chars();
    for (i, c) in ident.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() && name.next() != Some('-') {
            return false;
        }
        if name.next() != Some(c.to_ascii_lowercase()) {
            return false;
        }
    }
    name.next().is_none()
}

workloads! {
    /// Which boot workload to run. `kmain` maps each variant to a set of
    /// task spawns (and, for some, heartbeat behaviour). The *default* (no
    /// selection, `select` returns `None`) boots **`init`** — the userspace
    /// delegation-graph root (v0.13). The former default — the kernel scheduler
    /// demo (`task_a`/`task_b` + producer/consumer + the cross-hart probe) — is
    /// kept as the explicit [`Demo`](Self::Demo) workload.
    ///
    /// Each variant's name on the kernel command line is the kebab-case of its
    /// identifier — `SmpSpscBatch` is selected by `workload=smp-spsc-batch`. So a
    /// variant may only be renamed if its bootarg is meant to change with it.
    ///
    /// Variants are declared in sorted order (test-enforced); the ordering carries
    /// no meaning — nothing depends on the discriminant values.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WorkloadKind {
        /// v0.9c cap-transfer-in-reply: a `badge-handout-server` (`RECV | MINT`)
        /// mints a badged `SEND` cap per request and **hands it back in the reply**;
        /// a `badge-handout-client` `call`s, receives the badged cap, and signals
        /// success. Proves a server can return capabilities to a client. One hart.
        BadgeHandout,
        /// v0.9c badged endpoints: two processes share one endpoint. A `minter`
        /// holds `RECV | MINT` and mints a badged `SEND` cap (observed as a
        /// `CapEvent::Transferred` carrying the badge); a `client` holds `SEND` only
        /// and is refused when it tries to mint (`SyscallRefused`). Same binary,
        /// outcome differs by capability. One hart.
        BadgeMint,
        /// v0.9 block/wake smoke: a `blocker` kernel task calls `block_current`
        /// and a `waker` peer wakes it, proving a task can leave the CPU off the
        /// runqueue and be resumed. The substrate IPC's blocking `send`/`receive`
        /// ride on. Task-driven, single hart.
        BlockWake,
        /// Boot-stack (task 0) guard smoke: a kernel task stores into the boot stack's
        /// unmapped guard page, faulting at the store; the trap handler recognizes the
        /// boot guard region and snitches `Log("kernel stack overflow: boot stack …")`.
        /// Proves `mmu::guard_boot_stack` unmapped the page + the handler names it.
        /// `itest-workloads` only.
        BootStackGuard,
        /// v0.11 Tier-0 console input: a `console-echo` program that loops
        /// `ConsoleRead` → `DebugWrite`, echoing typed UART input back as `Log`
        /// frames. Proves the polled-RX path (UART → timer drain → ring →
        /// `ConsoleRead` → userspace) end to end. See
        /// `plans/legacy/console-tier0-polled-rx.md`.
        ConsoleEcho,
        /// Just the single-hart cooperative producer/consumer pair — the
        /// `workload-cooperative-baseline` correctness oracle without the demo's
        /// `task_a`/`task_b`. Those burn LCG that the baseline doesn't need, and by
        /// eating half the scheduler's turns they slow the producer/consumer's
        /// progress, so carving them out lets the baseline reach its sample threshold
        /// in far fewer instructions. Same assertion, cheaper to reach.
        Cooperative,
        /// The former default boot: the kernel scheduler demo — `task_a`/`task_b`
        /// cooperative tasks + the producer/consumer pair + the cross-hart spawn
        /// probe. Kept as an explicit workload now that the no-bootarg default boots
        /// `init` (v0.13). Exercises the scheduler (context switch, yield, per-task
        /// spans) and SMP bring-up — the `sched-*`/`smp-*` scenarios run here.
        Demo,
        /// v0.13 `EndpointCreate`: a single program manufactures its own IPC endpoint
        /// via the `EndpointCreate` syscall and proves the returned cap is a real
        /// owning `RECV | MINT` by minting a badged `SEND` on it.
        EndpointCreate,
        /// FP context-switch oracle: **two** processes, each filling all 32 FP
        /// registers with a distinct pattern, spinning long enough to be preempted,
        /// then reading them back and reporting mismatches. Two processes because one
        /// proves nothing — the kernel is zero-FP, so with a single FP process nobody
        /// can clobber the register file. The mismatch count is the assertion, so a
        /// broken save/restore is a *wrong number* rather than a silence.
        FpChurn,
        /// Frame-allocator OOM: keep the default demo tasks, but the
        /// heartbeat leaks frames each tick until the pool exhausts.
        FrameOom,
        /// v0.10 `RAMfs`: an `fs` server (`RECV | MINT`) serves a flat in-memory
        /// filesystem to an `fs-client` over one endpoint. The client connects
        /// (badge 0) to be minted a root File cap (`pack(root, READ)`), then issues
        /// FS requests against it; the server demuxes inode + rights by badge. One
        /// hart.
        Fs,
        /// `glitch` v1: the userspace audio server holding the PWMDAC as a
        /// capability. A `glitch-server` (`RECV` on the endpoint **plus** an
        /// `AudioSink` cap) synthesizes tones on request; a `beep` client
        /// (`SEND`) `call`s it to play 440 Hz. Proves the full client → IPC →
        /// cap-gated `AudioWrite` → `WDATA` path, re-casting the in-kernel Tier-0
        /// beep as a client of a disciplined server. See `plans/glitch.md`.
        GlitchBeep,
        /// `glitch` v2 under-run probe: a `starve` program holding the `AudioSink`
        /// enqueues one short batch declaring **more samples are coming**, then never
        /// feeds again. The ring drains within milliseconds and every audio deadline
        /// after that is a genuine missed feed — so `snitchos.audio.xruns_total`
        /// climbs and `AudioXRun` frames reach the wire.
        ///
        /// This exists because the `XRun` observable was *wired but dormant* until the
        /// stream-active signal landed: nothing set it, so an empty ring always read as
        /// idle silence and the fault path could not fire at all. A real-time
        /// observable nobody has watched fail is indistinguishable from a healthy
        /// system. `itest-workloads` only. See `plans/glitch-v2-async-ring.md`.
        GlitchStarve,
        /// JH7110 GMAC reconnaissance (M2.5): a **read-only** boot that dumps what
        /// U-Boot already left configured — clock gates, reset status, the MAC's own
        /// registers and the `phy_intf_sel` syscon field — then stops. It writes
        /// nothing, because the state it reads is destroyed by the driver's first
        /// reset assert, so the reads cannot live inside bring-up.
        ///
        /// Answers Phase 0's board-side questions in one boot, and de-risks the first
        /// MMIO read of a possibly-gated peripheral — which can hang the bus — away
        /// from any code that has other suspects. See `docs/vf2-gmac-design.md`
        /// ("Rung −1") and `plans/vf2-gmac-driver.md`. `itest-workloads` only.
        GmacProbe,
        /// JH7110 GMAC transmit smoke (M2.5): programs the TX descriptor ring, hands
        /// the MAC one hand-built Ethernet frame, and reports whether the engine
        /// took it — the `T3`/`T4` rungs of the design note's tracer-bullet ladder.
        ///
        /// Unlike [`GmacProbe`](Self::GmacProbe) this **writes**, so it is a separate
        /// workload rather than a phase of the probe: the probe's value is that it
        /// cannot perturb the state it reads. `itest-workloads` only.
        GmacTx,
        /// Userspace heap-growth probe: runs the `heap-grow` program, which
        /// allocates far past the runtime's per-region map size — forcing the
        /// `talc` allocator to `map_anon` more frames from the kernel on demand.
        HeapGrow,
        /// Kernel-heap OOM: default demo tasks, but the heartbeat leaks
        /// heap blocks each tick until the heap exhausts.
        HeapOom,
        /// Hung detection (v2b): a supervisor holds a liveness `Notification` and
        /// `wait_timeout`s it; a `hung-service` beats a few times then wedges (alive but
        /// stuck). The absent beat times out ⇒ the supervisor `Kill`s the wedged service.
        /// Proves timed `WaitNotify` + the per-hart timeout queue.
        HungDetect,
        /// v0.13 the supervising root: an `init` process that `Spawn`s a child
        /// (delegating its span cap) and reaps it via `WaitAny` — the root of the
        /// capability delegation graph. The first userspace process's eventual shape.
        Init,
        /// v0.9 IPC: two userspace processes (`ipc-sender`, `ipc-receiver`) share a
        /// kernel-brokered endpoint — A holds a `SEND` cap, B a `RECV` cap. A sends
        /// an inline message; B receives it and re-emits the payload. Time-sliced on
        /// one hart. The milestone-heart workload.
        Ipc,
        /// v0.9b RPC: an `rpc-client` `call`s an `rpc-server` over an endpoint; the
        /// server `receive`s, does work, and `reply`s through a one-shot reply cap.
        /// The client blocks across the round-trip (nested-span trace). One hart.
        IpcRpc,
        /// Tight cross-hart IPI wakeup loop (hart 0 → hart 1).
        /// Heartbeat-driven.
        IpiPong,
        /// Supervision v2a negative: a process holding no `Object::Process` cap tries to
        /// `Kill` and is refused (`SyscallRefused{Kill}`) — proving the kill authorization
        /// is real, not ambient. It survives the refusal and reports it.
        KillNoCap,
        /// The completion endpoint served by a model with **no weights**: a
        /// `kvetch` server backed by `babble` (rung 0 of the generative ladder)
        /// plus a client asking for one completion of a fixed prefix. Proves
        /// the whole serving path — protocol, serve loop, telemetry, seed
        /// discipline — before any checkpoint exists. `docs/babble-design.md`.
        KvetchBabble,
        /// The same endpoint served from **weights**: a `kvetch-drivel-server`
        /// (rung 1, the trained `drivel-b9b10-30k` checkpoint embedded in its own
        /// image) plus the same fixed-prefix client. Deliberately the `KvetchBabble`
        /// layout with a different server behind it, so the two rungs are comparable
        /// on one prompt. `plans/kvetch-drivel-on-target.md`.
        KvetchDrivel,
        /// Many long-**lived** tasks: spawn a large fixed set of tasks that each
        /// loop-yield forever (never exit), so the scheduler's task table genuinely
        /// holds N *live* entries. Stresses the O(1) `TaskDirectory` lookup — the
        /// `sched-task-lookup-is-o1` scenario asserts probes-per-switch stays constant
        /// as the live-task count grows.
        LiveTasks,
        /// Typed-interface end-to-end: a client reads `/bin/manifest_demo`'s
        /// `user.iface` xattr off the seeded FS, `decode_manifest`s it, and checks
        /// the shape — proving the `#[entry]` → note → xattr → IPC → decode chain.
        ManifestIface,
        /// Generic satisfier: a `satisfier` reads `/bin/fs-probe`'s declared `needs`
        /// off the seeded FS, matches them against its own caps via `hitch::satisfy`,
        /// and `SpawnImage`s the child with the granted `fs` cap — data-driven
        /// delegation (not hardcoded), named on the wire as `satisfy.<role>` spans.
        ManifestSatisfy,
        /// Two tasks (one per hart) hammer a shared `Mutex`. Task-driven.
        MutexStorm,
        /// v0.12 notification smoke: a `notify-waiter` parent creates a notification,
        /// `Spawn`s a `notify-signaller` child delegating the cap, then `WaitNotify`s
        /// on it; the child `Signal`s a known bit mask. Proves the async kernel→user
        /// wake crosses the task boundary — a `NotifySignal`→`NotifyWait` edge on the
        /// wire. One hart.
        NotifySmoke,
        /// Minimal crash workload: a kernel task calls `panic!()` immediately on its
        /// first run — no guard page, no MMU, no fault. Exists purely to isolate the
        /// snemu-vs-QEMU divergence the stack-guard family shows (only-snemu
        /// `kernel.heartbeat`): if a bare panic reproduces it, the divergence is a
        /// crash-vs-heartbeat *timing* artifact, not anything about guard pages.
        /// `itest-workloads` only.
        PanicNow,
        /// Cross-hart ping-pong: ping (hart 0) and pong (hart 1) alternate
        /// turns via a shared turn flag, each handing off with an
        /// `IPI_WAKEUP` so the idle partner re-wakes. Task-driven; an
        /// alternation/wakeup cadence oracle.
        PingPong,
        /// v0.8b priority demo: a `High`-priority and a `Low`-priority cooperative
        /// worker share one hart. The High worker runs far more often (priority
        /// respected), but the Low worker still makes progress (aging prevents
        /// starvation) — "ordered, but fair."
        Priorities,
        /// Userspace-defined-metrics demo (debt #2): a `probe` program registers
        /// its own metric (`snitchos.probe.custom`, a gauge) through its bootstrap
        /// `TelemetrySink` cap and emits to it via the handle the kernel hands back —
        /// then deliberately emits through an *unregistered* handle, which the kernel
        /// must refuse (`SyscallRefused`), not silently emit. Proves a process names
        /// its own metrics without the kernel knowing them ahead of time, and that
        /// the per-process metric table is the forgery boundary. Not a storm.
        Probe,
        /// Interactive powerbox shell: a seeded FS server + `shell`. The shell reads
        /// `view <path>` commands from the UART console, looks up files with
        /// READ-only rights, spawns the viewer, and revokes the cap on exit.
        Shell,
        /// Tight `mmu::shootdown` loop from hart 0. Heartbeat-driven.
        ShootdownStorm,
        /// Cross-hart producer/consumer over `Mutex<VecDeque>`: producer on
        /// hart 0, consumer on hart 1 (v0.6 step 11).
        Smp,
        /// Four-hart demonstration: a worker task on every secondary hart
        /// (`1..num_harts`), each opening a `smp4.tick` span (tagged with its
        /// `hart_id`) and yielding — proof that all brought-up harts execute work.
        /// Selected by `workload=smp4`; the itest harness pairs it with a 4-cpu DTB
        /// and `hart_count=4`.
        Smp4,
        /// As [`Smp`](Self::Smp) but over a lock-free `heapless::spsc`
        /// queue instead of `Mutex<VecDeque>` (v0.6 step 12). The A/B
        /// counterpart for the lock-contention measurement. Fences
        /// per-item.
        SmpSpsc,
        /// As [`SmpSpsc`](Self::SmpSpsc) but over a batched ring
        /// (`kernel_obs::batch_ring`) that fences once *per batch* — the
        /// controlled third variant isolating per-item vs per-batch
        /// cross-hart fence cost.
        SmpSpscBatch,
        /// v0.11 spawn-with-caps demo: a `spawner` parent that `Spawn`s a `spawnee`
        /// child at runtime, delegating its span cap. Proves the `Spawn` syscall
        /// carries delegated authority into a freshly-created process. See
        /// `plans/legacy/spawn-shell-and-console.md`.
        SpawnDemo,
        /// `SpawnImage` demo: a seeded FS server plus a client that reads
        /// `/bin/spawnee` off the filesystem and spawns it from the buffer via the
        /// `SpawnImage` syscall (vs the embedded `Spawn` registry).
        SpawnImage,
        /// v0.12 reclaim test: a `reaper` parent that `Spawn`s + `Wait`s a
        /// memory-hungry `memhog` child 30×. Proves Exit reclaims the child's user
        /// address space — without teardown the leak OOMs the kernel.
        SpawnReap,
        /// Cross-hart spawn storm: hart 0 runs a serialised `spawn_on(1, …)`
        /// loop; hart 1 stays idle until poked. Heartbeat-driven.
        SpawnStorm,
        /// Kernel-stack guard Tier-B smoke: a kernel task deliberately stores into its
        /// own (unmapped) guard page from a context with full stack headroom, faulting
        /// at the exact store; the trap handler recognizes the guard region, snitches a
        /// `Log` ("kernel stack overflow: task …"), and panics. Proves the
        /// fault→name→halt path deterministically (no deep-overflow double-fault).
        /// `itest-workloads` only.
        StackGuard,
        /// Kernel-stack guard Tier-B *deep* smoke: a kernel task recurses until it
        /// genuinely overflows its stack into the guard page. Proves the **per-hart
        /// exception stack** — the fault handler builds its frame on a clean stack and
        /// reports cleanly, where without it a deep overflow would double-fault on the
        /// overflowed stack. `itest-workloads` only.
        StackOverflowDeep,
        /// The Stitch REPL with **trained** completion: the drivel-backed server plus
        /// the REPL holding `SEND` on it, so Tab at the prompt is answered by weights
        /// rather than by a uniform walk. `StitchKvetch`'s twin, and the comparison
        /// the ladder exists to make.
        StitchDrivel,
        /// The Stitch REPL with a filesystem: a seeded FS server plus the REPL
        /// holding the FS endpoint cap, so `:load <name>` reads a baked-in `.st`
        /// file off the ramfs and runs it.
        StitchFs,
        /// The Stitch REPL with **completion**: a `kvetch` server (babble behind
        /// an endpoint) plus the REPL holding a `SEND` cap on it, so Tab at the
        /// prompt asks a real completion service.
        ///
        /// Deliberately *instead of* the filesystem, not alongside it: a
        /// `run_ipc`-launched program is granted one endpoint, and the REPL
        /// reads it positionally. Holding both would need an init-style spawn
        /// tree delegating two caps — which is exactly the positional-startup
        /// fragility `docs/manifest-design.md` exists to kill, so it is left for
        /// that work rather than worked around here.
        StitchKvetch,
        /// The Stitch tree-walk interpreter running as a userspace REPL on the metal:
        /// boots a self-test (`1 + 2`), then loops `ConsoleRead` → evaluate →
        /// `ConsoleWrite`. First on-target run of the ported `no_std` interpreter.
        StitchRepl,
        /// Supervision step 2: the generic supervisor root. A `supervised` process
        /// walks a data-driven service table — bringing services up in dependency
        /// order, reaping via `WaitAny`, and consulting the pure `supervision` policy
        /// (restart with backoff, stop, or escalate) — instead of hardcoding it. Its
        /// `crasher` service crash-loops past its intensity budget and escalates.
        Supervised,
        /// Supervision FU2: the cap-survival supervisor. A persistent client and a
        /// crashing IPC server share the supervisor's durable endpoint; the client's
        /// minted `SEND` keeps working across server restarts (a real IPC round-trip
        /// lands on each fresh server incarnation), proving a minted cap survives its
        /// server dying because it names the durable object, not the process.
        SupervisedIpc,
        /// Supervision v2a: the graceful-shutdown supervisor. Brings a small dependency
        /// tree up in `startup_order`, then tears it down in `teardown_order` — stopping
        /// cooperative services via a `Signal`ed shutdown notification (clean exit) and a
        /// forced one via `Kill`. The stops land in the exact reverse of startup, each an
        /// observable event on the wire (a forced stop also a `CapEvent::Revoked`).
        SupervisedShutdown,
        /// v0.8 preemption guard: a `syscall-hog` program that loops issuing a cheap
        /// ambient syscall (`DebugWrite`) back-to-back, spending most of its time in
        /// S-mode with interrupts masked, co-located with a cooperative `worker_a`
        /// peer. Documents that a *syscall-heavy* U-mode task is still preempted: the
        /// timer can't fire mid-syscall (`SIE == 0` throughout trap handling), so it
        /// fires the instant the syscall `sret`s back to U-mode (`SPP == 0`). Guards
        /// against a regression that re-enables interrupts inside long syscalls
        /// without a `need_resched` drain. See `plans/legacy/v0.8c-need-resched-on-syscall-return.md`.
        SyscallHog,
        /// Cross-hart TLB-shootdown *correctness* workload: hart 0 remaps a
        /// shared VA between two frames and shoots down; hart 1 reads
        /// through the VA each round and must never see the stale frame.
        /// Task-driven; the initiator yields so the heartbeat keeps
        /// draining the round / stale-read counters (so *not* a storm).
        /// Distinct from [`ShootdownStorm`](Self::ShootdownStorm), which measures
        /// rather than checks.
        TlbShootdown,
        /// v0.8 preemption fixture: a `user-hog` program that runs a tight U-mode
        /// `loop {}` (no syscalls, no `yield`) co-located with a cooperative
        /// `worker_a` peer. Without preemption the hog never relinquishes the CPU
        /// and the peer starves; the timer-driven preemption (Step 4) is what lets
        /// the peer make progress.
        UserHog,
        /// Multi-hart userspace de-risk (v2b step 1): a single userspace program placed on
        /// **hart 0** (userspace normally runs on hart 1). It opens a span; the `SpanStart`
        /// carries `hart_id == 0`, proving U-mode runs on the boot hart too — the
        /// foundation for a cross-hart Kill consumer.
        UserOnHart0,
        /// v0.7a first userspace: load the embedded `user/hello` program,
        /// drop to U-mode on hart 1, and handle its one ambient telemetry
        /// syscall. Hart 0 keeps heartbeating. Not a storm. (Available in
        /// any build, not just `itest-workloads` — it's the real feature.)
        Userspace,
        /// User-pointer validation probe: runs the `bad-ptr` program, which passes
        /// an in-range but **unmapped** user VA to `DebugWrite`. The kernel's
        /// `copy_from_user` must *refuse* it (`BadUserRange`) rather than fault —
        /// `bad-ptr` survives and emits a marker. Not a storm.
        UserspaceBadPtr,
        /// v0.7a isolation probe: like [`Userspace`](Self::Userspace) but runs
        /// the `faulter` program, which reads a kernel VA from U-mode — the
        /// `U`-bit firewall must fault it. Not a storm.
        UserspaceFault,
        /// Unhandled-user-trap probe: runs the `illegal` program, which executes
        /// `unimp` from U-mode. The kernel must terminate the process and keep
        /// running — before this existed, any exception from U-mode other than a
        /// page fault reached the trap dispatcher's catch-all `panic!`, so one
        /// illegal instruction from any user program halted the machine (which is
        /// what a float at the Stitch REPL did — see
        /// `docs/floating-point-design.md`). Not a storm.
        UserspaceIllegal,
        /// Span-quota probe: runs the `span-flood` program, which opens spans with
        /// many distinct names to exceed the per-process `SpanNameTable` quota — the
        /// kernel must refuse the surplus (`SyscallRefused{Quota}`) without panicking.
        UserspaceSpanFlood,
        /// Powerbox viewer demo: a seeded FS server + `view-demo` launcher. The
        /// launcher connects to the FS, looks up a file with READ-only rights, then
        /// spawns the viewer (`Spawn` registry id 6) with that attenuated cap
        /// delegated. First end-to-end demo of the powerbox hand-off pattern.
        ViewDemo,
        /// hart 0 emit-storm over the virtio TX path, hart 1 atomic spin.
        /// Task-driven.
        VirtioStorm,
        /// v0.13 wait-for-any: a `supervisor` parent that `Spawn`s a never-exiting
        /// `spinner` + an exiting `spawnee`, then `WaitAny`s for whichever exits.
        /// Proves a supervising parent reaps any child without naming it.
        WaitAny,
        /// Userspace demo workers: cooperative `worker` processes that loop
        /// {open a span, bump progress, `yield`}, the userspace successors to the
        /// kernel-mode `task_a`/`task_b`. (v0.7 follow-on; the road to v0.8.)
        Workers,
        /// Cross-hart Kill (v2b step 4): a supervisor on hart 1 `SpawnOn`s a victim to
        /// **hart 0**, lets it run, then `Kill`s it — the `running_remote` case. The kill
        /// flags the victim + IPIs hart 0, which self-terminates it at its return-to-user
        /// checkpoint; the supervisor reaps it. Proves the last deferred row of the kill
        /// matrix.
        XhartKill,
    }
}

/// Look up a `key=<usize>` parameter in the bootargs string (e.g.
/// `burst=128`). Returns `None` if the key is absent or the value
/// doesn't parse. Whole-token match, so `burst` does not match
/// `bursty=5`.
pub fn param_usize(bootargs: &str, key: &str) -> Option<usize> {
    bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

impl WorkloadKind {
    /// True for the `*-storm` workloads, which drive hart 1 themselves
    /// (spawn their own hart-1 task, or poke an intentionally-idle
    /// hart 1). `kmain` uses this to decide whether to spawn the
    /// default `hart_1_probe`.
    pub fn is_storm(self) -> bool {
        matches!(
            self,
            Self::IpiPong
                | Self::MutexStorm
                | Self::PingPong
                | Self::ShootdownStorm
                | Self::SpawnStorm
                | Self::TlbShootdown
                | Self::VirtioStorm
        )
    }

    /// True for workloads that drive the UART terminal themselves — the
    /// interactive REPLs and the powerbox shell, which write a prompt and read a
    /// line back. The kernel must not print to a console a workload owns: one
    /// writer per stream, or the two interleave and shred each other's output
    /// (an `hb` pulse landing mid-prompt at the Stitch REPL is the case that
    /// motivated this).
    ///
    /// [`ConsoleEcho`](Self::ConsoleEcho) is deliberately absent: it *reads* the
    /// console but writes to the telemetry stream via `DebugWrite`, so it owns no
    /// UART output there is anything to corrupt.
    ///
    /// Orthogonal to [`ConsoleMode`]: that says *where* kernel output goes, this
    /// says whether it should exist at all. A console-owning workload in
    /// `ConsoleMode::Frames` is already safe (kernel lines go to the wire), but
    /// so is its own output — which is why the REPL is run in `Text` and needs
    /// this predicate.
    pub fn owns_console(self) -> bool {
        matches!(
            self,
            Self::Shell
                | Self::StitchDrivel
                | Self::StitchFs
                | Self::StitchKvetch
                | Self::StitchRepl
        )
    }
}

/// Where the kernel's human log output goes, selected by the `console=` bootarg.
/// Orthogonal to [`WorkloadKind`] in the parser — any pairing is accepted, e.g.
/// `workload=init console=frames` for a headless board on one cable.
///
/// Orthogonal is not *independent*, though: pairing [`Frames`](Self::Frames) with a
/// [console-owning workload](WorkloadKind::owns_console) is legal and almost never
/// what you want. See that variant's note. An interactive board session is
/// `console=text`; `owns_console` is what keeps it quiet, not the mode.
///
/// See `docs/uart-telemetry-design.md` Decision 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleMode {
    /// Human-readable text on the UART — the default. A dumb terminal (`screen`)
    /// reads it; no collector required. The bring-up and "something is wrong" mode.
    #[default]
    Text,
    /// Every post-init kernel log line becomes a `Frame::Log` on the telemetry
    /// wire, which the collector renders. One content type on the wire — the
    /// board's one-cable telemetry mode.
    ///
    /// **Takes userspace output with it.** `ConsoleWrite` reuses the kernel's
    /// `print!` path (`kernel/src/syscall/console.rs`), so this diverts a REPL's
    /// prompt and results to the wire too — on a dumb terminal that is
    /// indistinguishable from a dead board. Correct when the collector is
    /// reading; wrong for a `screen` session.
    Frames,
}

/// Parse `console=<mode>` from the bootargs. Unknown or absent → [`ConsoleMode::Text`]:
/// fail safe to the mode that always works and needs no collector (and so that a
/// Linux-style `console=ttyS0` never accidentally selects frames).
#[must_use]
pub fn console_mode(bootargs: &str) -> ConsoleMode {
    bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("console="))
        .map_or(ConsoleMode::Text, |v| match v {
            "frames" => ConsoleMode::Frames,
            _ => ConsoleMode::Text,
        })
}

/// Parse a `workload=<name>` selection out of the bootargs string, where `<name>`
/// is the kebab-case of a [`WorkloadKind`] variant.
///
/// Returns `None` when no `workload=` key is present (run the default
/// demo) or when the value is unrecognised (also default — a typo
/// should fail safe to default rather than silently match something).
pub fn select(bootargs: &str) -> Option<WorkloadKind> {
    bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("workload="))
        .and_then(WorkloadKind::from_kebab)
}

/// Static addressing for the network telemetry sink, parsed from the `net=`
/// bootarg. Kernel-boot-local (this crate has no deps); `kmain` converts it to a
/// `kernel_net::NetConfig` when installing the UDP sink. The source port is not
/// on the wire format — the kernel defaults it at conversion. See
/// `docs/network-telemetry-design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetBoot {
    pub src_ip: [u8; 4],
    pub src_mac: [u8; 6],
    pub dst_ip: [u8; 4],
    pub dst_mac: [u8; 6],
    pub dst_port: u16,
}

/// Why a `net=` value was rejected. The kernel snitches on this rather than
/// booting with half-configured networking (refusals are observable, never silent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetParseError {
    /// Not exactly five comma-separated fields.
    FieldCount,
    /// An IP address wasn't four decimal octets, each in `0..=255`.
    Ip,
    /// A MAC wasn't six colon-separated hex bytes.
    Mac,
    /// The port wasn't a decimal `u16`.
    Port,
}

/// Parse `net=<our-ip>,<our-mac>,<dst-ip>,<dst-mac>,<dst-port>` from the bootargs.
///
/// `None` = no `net=` key (default transport unchanged); `Some(Ok)` = a valid
/// config; `Some(Err)` = a malformed value the caller must snitch on rather than
/// silently ignore.
#[must_use]
pub fn net(bootargs: &str) -> Option<Result<NetBoot, NetParseError>> {
    let value = bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("net="))?;
    Some(parse_net(value))
}

fn parse_net(value: &str) -> Result<NetBoot, NetParseError> {
    let mut fields = value.split(',');
    let src_ip = parse_bytes::<4>(field(&mut fields)?, '.', 10, NetParseError::Ip)?;
    let src_mac = parse_bytes::<6>(field(&mut fields)?, ':', 16, NetParseError::Mac)?;
    let dst_ip = parse_bytes::<4>(field(&mut fields)?, '.', 10, NetParseError::Ip)?;
    let dst_mac = parse_bytes::<6>(field(&mut fields)?, ':', 16, NetParseError::Mac)?;
    let dst_port = field(&mut fields)?.parse().map_err(|_| NetParseError::Port)?;
    if fields.next().is_some() {
        return Err(NetParseError::FieldCount);
    }
    Ok(NetBoot { src_ip, src_mac, dst_ip, dst_mac, dst_port })
}

fn field<'a>(fields: &mut impl Iterator<Item = &'a str>) -> Result<&'a str, NetParseError> {
    fields.next().ok_or(NetParseError::FieldCount)
}

/// Parse exactly `N` `sep`-separated bytes in the given `radix` (10 for IPv4
/// octets, 16 for MAC bytes). Any wrong count, out-of-range value, or bad digit
/// is `err`.
fn parse_bytes<const N: usize>(
    s: &str,
    sep: char,
    radix: u32,
    err: NetParseError,
) -> Result<[u8; N], NetParseError> {
    let mut parts = s.split(sep);
    let mut out = [0u8; N];
    for slot in &mut out {
        *slot = u8::from_str_radix(parts.next().ok_or(err)?, radix).map_err(|_| err)?;
    }
    if parts.next().is_some() {
        return Err(err);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_parses_a_full_config() {
        let got = net("net=10.0.0.2,52:54:00:12:34:56,10.0.0.1,aa:bb:cc:dd:ee:ff,9000");
        assert_eq!(
            got,
            Some(Ok(NetBoot {
                src_ip: [10, 0, 0, 2],
                src_mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
                dst_ip: [10, 0, 0, 1],
                dst_mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
                dst_port: 9000,
            }))
        );
    }

    #[test]
    fn net_parses_when_mixed_with_other_bootargs() {
        // Whole-token, order-independent — like `workload=`/`console=`.
        let got = net("workload=smp net=1.2.3.4,00:11:22:33:44:55,5.6.7.8,66:77:88:99:aa:bb,1234 console=frames");
        assert_eq!(got.unwrap().unwrap().dst_port, 1234);
    }

    #[test]
    fn net_absent_is_none() {
        assert_eq!(net("workload=smp console=frames"), None);
        assert_eq!(net(""), None);
    }

    #[test]
    fn net_rejects_a_too_large_octet() {
        assert_eq!(
            net("net=10.0.0.256,52:54:00:12:34:56,10.0.0.1,aa:bb:cc:dd:ee:ff,9000"),
            Some(Err(NetParseError::Ip))
        );
    }

    #[test]
    fn net_rejects_an_ip_with_wrong_octet_count() {
        assert_eq!(
            net("net=10.0.0,52:54:00:12:34:56,10.0.0.1,aa:bb:cc:dd:ee:ff,9000"),
            Some(Err(NetParseError::Ip))
        );
    }

    #[test]
    fn net_rejects_a_short_mac() {
        assert_eq!(
            net("net=10.0.0.2,52:54:00:12:34,10.0.0.1,aa:bb:cc:dd:ee:ff,9000"),
            Some(Err(NetParseError::Mac))
        );
    }

    #[test]
    fn net_rejects_a_non_hex_mac_byte() {
        assert_eq!(
            net("net=10.0.0.2,52:54:00:12:34:gg,10.0.0.1,aa:bb:cc:dd:ee:ff,9000"),
            Some(Err(NetParseError::Mac))
        );
    }

    #[test]
    fn net_rejects_a_port_above_u16() {
        assert_eq!(
            net("net=10.0.0.2,52:54:00:12:34:56,10.0.0.1,aa:bb:cc:dd:ee:ff,70000"),
            Some(Err(NetParseError::Port))
        );
    }

    #[test]
    fn net_rejects_the_wrong_field_count() {
        assert_eq!(
            net("net=10.0.0.2,52:54:00:12:34:56,10.0.0.1"),
            Some(Err(NetParseError::FieldCount))
        );
    }
    use alloc::string::String;
    use alloc::vec::Vec;

    /// This file's own source, so the registry can assert its own shape.
    const SRC: &str = include_str!("bootargs.rs");

    /// The `WorkloadKind` variant names, in declaration order. Doc comments,
    /// blank lines and attributes are skipped — only `Variant,` lines count.
    fn declared_variants(src: &str) -> Vec<&str> {
        src.lines()
            .map(str::trim)
            .skip_while(|l| *l != "pub enum WorkloadKind {")
            .skip(1)
            .take_while(|l| *l != "}")
            .filter_map(|l| l.strip_suffix(','))
            .filter(|name| !name.is_empty() && name.chars().all(char::is_alphanumeric))
            .collect()
    }

    /// `SmpSpscBatch` -> `smp-spsc-batch`. Built independently of `kebab_eq`, which
    /// compares without allocating — so the two agreeing is a real cross-check.
    fn kebab(variant: &str) -> String {
        variant
            .char_indices()
            .flat_map(|(i, c)| {
                let dash = (i > 0 && c.is_ascii_uppercase()).then_some('-');
                dash.into_iter().chain(c.to_lowercase())
            })
            .collect()
    }

    #[test]
    fn kebab_eq_matches_a_single_word() {
        assert!(kebab_eq("Smp", "smp"));
        assert!(kebab_eq("Fs", "fs"));
        assert!(!kebab_eq("Smp", "fs"));
    }

    #[test]
    fn kebab_eq_splits_on_each_word_boundary() {
        assert!(kebab_eq("SmpSpscBatch", "smp-spsc-batch"));
        assert!(kebab_eq("UserspaceBadPtr", "userspace-bad-ptr"));
        // The boundary is required, not optional.
        assert!(!kebab_eq("SmpSpsc", "smpspsc"));
    }

    #[test]
    fn kebab_eq_keeps_digits_attached_to_their_word() {
        assert!(kebab_eq("UserOnHart0", "user-on-hart0"));
        assert!(!kebab_eq("UserOnHart0", "user-on-hart-0"));
    }

    #[test]
    fn kebab_eq_rejects_a_prefix_of_either_side() {
        // A shorter ident must not match a longer name, or vice versa —
        // this is what keeps `smp` from swallowing `smp-spsc`.
        assert!(!kebab_eq("Smp", "smp-spsc"));
        assert!(!kebab_eq("SmpSpsc", "smp"));
        assert!(!kebab_eq("Ipc", "ipc-rpc"));
    }

    #[test]
    fn every_workload_selects_by_its_kebab_name() {
        for (kind, ident) in WorkloadKind::ALL {
            let name = kebab(ident);
            let bootargs = alloc::format!("workload={name}");
            assert_eq!(select(&bootargs), Some(*kind), "{ident} did not select via {name:?}");
        }
    }

    #[test]
    fn workload_variants_are_declared_in_sorted_order() {
        let variants = declared_variants(SRC);
        assert_eq!(
            variants.len(),
            WorkloadKind::ALL.len(),
            "source parser disagrees with the generated table",
        );
        let out_of_order: Vec<_> = variants
            .windows(2)
            .filter(|w| w[0] >= w[1])
            .map(|w| (w[0], w[1]))
            .collect();
        assert_eq!(out_of_order, [], "WorkloadKind variants must stay sorted");
    }

    #[test]
    fn selects_glitch_beep() {
        assert_eq!(select("workload=glitch-beep"), Some(WorkloadKind::GlitchBeep));
    }

    #[test]
    fn selects_glitch_starve() {
        assert_eq!(select("workload=glitch-starve"), Some(WorkloadKind::GlitchStarve));
    }

    #[test]
    fn selects_gmac_probe() {
        assert_eq!(select("workload=gmac-probe"), Some(WorkloadKind::GmacProbe));
    }

    #[test]
    fn selects_gmac_tx() {
        assert_eq!(select("workload=gmac-tx"), Some(WorkloadKind::GmacTx));
    }

    #[test]
    fn selects_stitch_kvetch() {
        assert_eq!(select("workload=stitch-kvetch"), Some(WorkloadKind::StitchKvetch));
    }

    #[test]
    fn selects_kvetch_babble() {
        assert_eq!(select("workload=kvetch-babble"), Some(WorkloadKind::KvetchBabble));
    }

    #[test]
    fn selects_fp_churn() {
        assert_eq!(select("workload=fp-churn"), Some(WorkloadKind::FpChurn));
    }

    #[test]
    fn selects_kvetch_drivel() {
        assert_eq!(select("workload=kvetch-drivel"), Some(WorkloadKind::KvetchDrivel));
        // The two rungs must not alias: they are the comparison.
        assert_ne!(select("workload=kvetch-drivel"), Some(WorkloadKind::KvetchBabble));
    }

    #[test]
    fn selects_stitch_drivel() {
        assert_eq!(select("workload=stitch-drivel"), Some(WorkloadKind::StitchDrivel));
        assert_ne!(select("workload=stitch-drivel"), Some(WorkloadKind::StitchKvetch));
    }

    #[test]
    fn selects_smp4() {
        assert_eq!(select("workload=smp4"), Some(WorkloadKind::Smp4));
        // The digit is part of the word: `smp4` must not parse as `smp`, nor
        // `smp` as `smp4`.
        assert_ne!(select("workload=smp4"), Some(WorkloadKind::Smp));
        assert_ne!(select("workload=smp"), Some(WorkloadKind::Smp4));
    }

    #[test]
    fn selects_smp_spsc() {
        assert_eq!(select("workload=smp-spsc"), Some(WorkloadKind::SmpSpsc));
        // `smp-spsc` must not be mis-parsed as `smp`.
        assert_ne!(select("workload=smp-spsc"), Some(WorkloadKind::Smp));
    }

    #[test]
    fn selects_smp_spsc_batch() {
        assert_eq!(select("workload=smp-spsc-batch"), Some(WorkloadKind::SmpSpscBatch));
        // Distinct from the per-item spsc variant.
        assert_ne!(select("workload=smp-spsc-batch"), Some(WorkloadKind::SmpSpsc));
    }

    #[test]
    fn owns_console_classifies_terminal_owning_workloads() {
        // The interactive REPLs and the powerbox shell write their prompt and
        // results to the UART, so an unsolicited kernel line corrupts them.
        assert!(WorkloadKind::Shell.owns_console());
        assert!(WorkloadKind::StitchRepl.owns_console());
        assert!(WorkloadKind::StitchFs.owns_console());
        assert!(WorkloadKind::StitchKvetch.owns_console());
        assert!(WorkloadKind::StitchDrivel.owns_console());

        // `console-echo` *reads* the console but writes to the telemetry stream
        // (`DebugWrite`), so it owns no UART output for the kernel to shred.
        assert!(!WorkloadKind::ConsoleEcho.owns_console());

        // Headless workloads: the liveness pulse is the whole point on these.
        assert!(!WorkloadKind::Init.owns_console());
        assert!(!WorkloadKind::Smp.owns_console());
        assert!(!WorkloadKind::SpawnStorm.owns_console());
    }

    #[test]
    fn is_storm_classifies_each_kind() {
        assert!(WorkloadKind::SpawnStorm.is_storm());
        assert!(WorkloadKind::IpiPong.is_storm());
        assert!(WorkloadKind::ShootdownStorm.is_storm());
        assert!(WorkloadKind::MutexStorm.is_storm());
        assert!(WorkloadKind::VirtioStorm.is_storm());
        assert!(!WorkloadKind::Smp.is_storm());
        assert!(!WorkloadKind::FrameOom.is_storm());
        assert!(!WorkloadKind::HeapOom.is_storm());
    }

    #[test]
    fn tlb_shootdown_is_a_storm() {
        // Heartbeat-driven (hart 0's round loop runs once on the first
        // tick) and spawns its own hart-1 reader — so it is
        // storm-classified: the default `hart_1_probe` is skipped and
        // its driver runs from `emit_storm_metrics`.
        assert!(WorkloadKind::TlbShootdown.is_storm());
    }

    #[test]
    fn syscall_hog_is_not_a_storm() {
        assert!(!WorkloadKind::SyscallHog.is_storm());
    }

    #[test]
    fn probe_is_not_a_storm() {
        assert!(!WorkloadKind::Probe.is_storm());
    }

    #[test]
    fn selects_supervised() {
        assert_eq!(select("workload=supervised"), Some(WorkloadKind::Supervised));
        // Must not be mis-parsed as the one-way `supervised` workload.
        assert_eq!(select("workload=supervised-ipc"), Some(WorkloadKind::SupervisedIpc));
        assert_ne!(select("workload=supervised-ipc"), Some(WorkloadKind::Supervised));
    }

    #[test]
    fn selects_ipc_rpc() {
        assert_eq!(select("workload=ipc-rpc"), Some(WorkloadKind::IpcRpc));
        // Must not be mis-parsed as the one-way `ipc` workload.
        assert_ne!(select("workload=ipc-rpc"), Some(WorkloadKind::Ipc));
    }

    #[test]
    fn selects_badge_mint() {
        assert_eq!(select("workload=badge-mint"), Some(WorkloadKind::BadgeMint));
        // Must not be mis-parsed as the RPC workload.
        assert_ne!(select("workload=badge-mint"), Some(WorkloadKind::IpcRpc));
    }

    #[test]
    fn selects_badge_handout() {
        assert_eq!(select("workload=badge-handout"), Some(WorkloadKind::BadgeHandout));
        // Must not be mis-parsed as the mint-into-own-table workload.
        assert_ne!(select("workload=badge-handout"), Some(WorkloadKind::BadgeMint));
    }

    #[test]
    fn userspace_workloads_are_not_storms() {
        assert!(!WorkloadKind::Userspace.is_storm());
        assert!(!WorkloadKind::UserspaceFault.is_storm());
        assert!(!WorkloadKind::UserspaceBadPtr.is_storm());
    }

    #[test]
    fn ping_pong_is_a_storm() {
        // Task-driven cross-hart workload that spawns its own hart-1
        // task (pong) and skips the default probe.
        assert!(WorkloadKind::PingPong.is_storm());
    }

    #[test]
    fn param_usize_reads_burst() {
        assert_eq!(param_usize("burst=128", "burst"), Some(128));
        assert_eq!(param_usize("workload=smp burst=64", "burst"), Some(64));
        assert_eq!(param_usize("burst=64 workload=smp", "burst"), Some(64));
    }

    #[test]
    fn param_usize_absent_or_bad_is_none() {
        assert_eq!(param_usize("workload=smp", "burst"), None);
        assert_eq!(param_usize("", "burst"), None);
        assert_eq!(param_usize("burst=", "burst"), None);
        assert_eq!(param_usize("burst=abc", "burst"), None);
        assert_eq!(param_usize("bursty=5", "burst"), None);
    }

    #[test]
    fn empty_bootargs_is_default() {
        assert_eq!(select(""), None);
    }

    #[test]
    fn no_workload_key_is_default() {
        assert_eq!(select("console=ttyS0 loglevel=7"), None);
    }

    #[test]
    fn unknown_workload_value_is_default() {
        assert_eq!(select("workload=does-not-exist"), None);
    }

    #[test]
    fn finds_workload_key_among_other_tokens() {
        assert_eq!(select("console=ttyS0 workload=smp loglevel=7"), Some(WorkloadKind::Smp));
    }

    #[test]
    fn workload_key_position_independent() {
        assert_eq!(select("loglevel=7 workload=smp"), Some(WorkloadKind::Smp));
    }

    #[test]
    fn console_mode_defaults_to_text_when_absent() {
        assert_eq!(console_mode("workload=init"), ConsoleMode::Text);
        assert_eq!(console_mode(""), ConsoleMode::Text);
    }

    #[test]
    fn console_mode_parses_frames() {
        assert_eq!(console_mode("console=frames"), ConsoleMode::Frames);
    }

    #[test]
    fn console_mode_parses_text_explicitly() {
        assert_eq!(console_mode("console=text"), ConsoleMode::Text);
    }

    #[test]
    fn console_mode_unknown_value_falls_back_to_text() {
        // `console=ttyS0` is Linux's serial-console selector, not ours — a value we
        // don't recognise must fail safe to the mode that always works.
        assert_eq!(console_mode("console=ttyS0"), ConsoleMode::Text);
    }

    #[test]
    fn console_mode_coexists_with_workload() {
        // The board boots e.g. `workload=stitch-repl console=frames` — one bootargs
        // string, two independent keys.
        assert_eq!(console_mode("workload=stitch-repl console=frames"), ConsoleMode::Frames);
        assert_eq!(select("workload=stitch-repl console=frames"), Some(WorkloadKind::StitchRepl));
    }
}
