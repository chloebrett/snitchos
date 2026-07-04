//! Kernel command-line (`/chosen/bootargs`) parsing — pure logic.
//!
//! The kernel selects its boot workload at runtime from a `workload=`
//! key in the bootargs string QEMU passes via `-append`. Parsing is
//! pure and host-tested here; `kmain` reads the raw string from the
//! DTB and feeds it in. See `docs/runtime-workload-selection-design.md`.

/// Which boot workload to run. `kmain` maps each variant to a set of
/// task spawns (and, for some, heartbeat behaviour). The *default* (no
/// selection, `select` returns `None`) boots **`init`** — the userspace
/// delegation-graph root (v0.13). The former default — the kernel scheduler
/// demo (`task_a`/`task_b` + producer/consumer + the cross-hart probe) — is
/// kept as the explicit [`Demo`](Self::Demo) workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadKind {
    /// Cross-hart producer/consumer over `Mutex<VecDeque>`: producer on
    /// hart 0, consumer on hart 1 (v0.6 step 11).
    Smp,
    /// As [`Smp`](Self::Smp) but over a lock-free `heapless::spsc`
    /// queue instead of `Mutex<VecDeque>` (v0.6 step 12). The A/B
    /// counterpart for the lock-contention measurement. Fences
    /// per-item.
    SmpSpsc,
    /// As [`SmpSpsc`](Self::SmpSpsc) but over a batched ring
    /// (`kernel_core::batch_ring`) that fences once *per batch* — the
    /// controlled third variant isolating per-item vs per-batch
    /// cross-hart fence cost.
    SmpSpscBatch,
    /// Frame-allocator OOM: keep the default demo tasks, but the
    /// heartbeat leaks frames each tick until the pool exhausts.
    FrameOom,
    /// Kernel-heap OOM: default demo tasks, but the heartbeat leaks
    /// heap blocks each tick until the heap exhausts.
    HeapOom,
    /// Cross-hart spawn storm: hart 0 runs a serialised `spawn_on(1, …)`
    /// loop; hart 1 stays idle until poked. Heartbeat-driven.
    SpawnStorm,
    /// Tight cross-hart IPI wakeup loop (hart 0 → hart 1).
    /// Heartbeat-driven.
    IpiPong,
    /// Tight `mmu::shootdown` loop from hart 0. Heartbeat-driven.
    ShootdownStorm,
    /// Two tasks (one per hart) hammer a shared `Mutex`. Task-driven.
    MutexStorm,
    /// hart 0 emit-storm over the virtio TX path, hart 1 atomic spin.
    /// Task-driven.
    VirtioStorm,
    /// Cross-hart TLB-shootdown *correctness* workload: hart 0 remaps a
    /// shared VA between two frames and shoots down; hart 1 reads
    /// through the VA each round and must never see the stale frame.
    /// Task-driven; the initiator yields so the heartbeat keeps
    /// draining the round / stale-read counters (so *not* a storm).
    TlbShootdownVisible,
    /// Cross-hart ping-pong: ping (hart 0) and pong (hart 1) alternate
    /// turns via a shared turn flag, each handing off with an
    /// `IPI_WAKEUP` so the idle partner re-wakes. Task-driven; an
    /// alternation/wakeup cadence oracle.
    PingPong,
    /// v0.7a first userspace: load the embedded `user/hello` program,
    /// drop to U-mode on hart 1, and handle its one ambient telemetry
    /// syscall. Hart 0 keeps heartbeating. Not a storm. (Available in
    /// any build, not just `itest-workloads` — it's the real feature.)
    Userspace,
    /// v0.7a isolation probe: like [`Userspace`](Self::Userspace) but runs
    /// the `faulter` program, which reads a kernel VA from U-mode — the
    /// `U`-bit firewall must fault it. Not a storm.
    UserspaceFault,
    /// User-pointer validation probe: runs the `bad-ptr` program, which passes
    /// an in-range but **unmapped** user VA to `DebugWrite`. The kernel's
    /// `copy_from_user` must *refuse* it (`BadUserRange`) rather than fault —
    /// `bad-ptr` survives and emits a marker. Not a storm.
    UserspaceBadPtr,
    /// Span-quota probe: runs the `span-flood` program, which opens spans with
    /// many distinct names to exceed the per-process `SpanNameTable` quota — the
    /// kernel must refuse the surplus (`SyscallRefused{Quota}`) without panicking.
    UserspaceSpanFlood,
    /// Userspace demo workers: cooperative `worker` processes that loop
    /// {open a span, bump progress, `yield`}, the userspace successors to the
    /// kernel-mode `task_a`/`task_b`. (v0.7 follow-on; the road to v0.8.)
    Workers,
    /// Userspace heap-growth probe: runs the `heap-grow` program, which
    /// allocates far past the runtime's per-region map size — forcing the
    /// `talc` allocator to `map_anon` more frames from the kernel on demand.
    HeapGrow,
    /// v0.8 preemption fixture: a `user-hog` program that runs a tight U-mode
    /// `loop {}` (no syscalls, no `yield`) co-located with a cooperative
    /// `worker_a` peer. Without preemption the hog never relinquishes the CPU
    /// and the peer starves; the timer-driven preemption (Step 4) is what lets
    /// the peer make progress.
    UserHog,
    /// v0.8 preemption guard: a `syscall-hog` program that loops issuing a cheap
    /// ambient syscall (`DebugWrite`) back-to-back, spending most of its time in
    /// S-mode with interrupts masked, co-located with a cooperative `worker_a`
    /// peer. Documents that a *syscall-heavy* U-mode task is still preempted: the
    /// timer can't fire mid-syscall (`SIE == 0` throughout trap handling), so it
    /// fires the instant the syscall `sret`s back to U-mode (`SPP == 0`). Guards
    /// against a regression that re-enables interrupts inside long syscalls
    /// without a `need_resched` drain. See `plans/v0.8c-need-resched-on-syscall-return.md`.
    SyscallHog,
    /// v0.11 Tier-0 console input: a `console-echo` program that loops
    /// `ConsoleRead` → `DebugWrite`, echoing typed UART input back as `Log`
    /// frames. Proves the polled-RX path (UART → timer drain → ring →
    /// `ConsoleRead` → userspace) end to end. See
    /// `plans/console-tier0-polled-rx.md`.
    ConsoleEcho,
    /// The Stitch tree-walk interpreter running as a userspace REPL on the metal:
    /// boots a self-test (`1 + 2`), then loops `ConsoleRead` → evaluate →
    /// `ConsoleWrite`. First on-target run of the ported `no_std` interpreter.
    StitchRepl,
    /// The Stitch REPL with a filesystem: a seeded FS server plus the REPL
    /// holding the FS endpoint cap, so `:load <name>` reads a baked-in `.st`
    /// file off the ramfs and runs it.
    StitchFs,
    /// `SpawnImage` demo: a seeded FS server plus a client that reads
    /// `/bin/spawnee` off the filesystem and spawns it from the buffer via the
    /// `SpawnImage` syscall (vs the embedded `Spawn` registry).
    SpawnImage,
    /// Typed-interface end-to-end: a client reads `/bin/manifest_demo`'s
    /// `user.iface` xattr off the seeded FS, `decode_manifest`s it, and checks
    /// the shape — proving the `#[entry]` → note → xattr → IPC → decode chain.
    ManifestIface,
    /// v0.11 spawn-with-caps demo: a `spawner` parent that `Spawn`s a `spawnee`
    /// child at runtime, delegating its span cap. Proves the `Spawn` syscall
    /// carries delegated authority into a freshly-created process. See
    /// `plans/spawn-shell-and-console.md`.
    SpawnDemo,
    /// v0.12 reclaim test: a `reaper` parent that `Spawn`s + `Wait`s a
    /// memory-hungry `memhog` child 30×. Proves Exit reclaims the child's user
    /// address space — without teardown the leak OOMs the kernel.
    SpawnReap,
    /// v0.13 wait-for-any: a `supervisor` parent that `Spawn`s a never-exiting
    /// `spinner` + an exiting `spawnee`, then `WaitAny`s for whichever exits.
    /// Proves a supervising parent reaps any child without naming it.
    WaitAny,
    /// v0.13 the supervising root: an `init` process that `Spawn`s a child
    /// (delegating its span cap) and reaps it via `WaitAny` — the root of the
    /// capability delegation graph. The first userspace process's eventual shape.
    Init,
    /// v0.13 EndpointCreate: a single program manufactures its own IPC endpoint
    /// via the `EndpointCreate` syscall and proves the returned cap is a real
    /// owning `RECV | MINT` by minting a badged `SEND` on it.
    EndpointCreate,
    /// The former default boot: the kernel scheduler demo — `task_a`/`task_b`
    /// cooperative tasks + the producer/consumer pair + the cross-hart spawn
    /// probe. Kept as an explicit workload now that the no-bootarg default boots
    /// `init` (v0.13). Exercises the scheduler (context switch, yield, per-task
    /// spans) and SMP bring-up — the `sched-*`/`smp-*` scenarios run here.
    Demo,
    /// v0.8b priority demo: a `High`-priority and a `Low`-priority cooperative
    /// worker share one hart. The High worker runs far more often (priority
    /// respected), but the Low worker still makes progress (aging prevents
    /// starvation) — "ordered, but fair."
    Priorities,
    /// v0.9 block/wake smoke: a `blocker` kernel task calls `block_current`
    /// and a `waker` peer wakes it, proving a task can leave the CPU off the
    /// runqueue and be resumed. The substrate IPC's blocking `send`/`receive`
    /// ride on. Task-driven, single hart.
    BlockWake,
    /// v0.9 IPC: two userspace processes (`ipc-sender`, `ipc-receiver`) share a
    /// kernel-brokered endpoint — A holds a `SEND` cap, B a `RECV` cap. A sends
    /// an inline message; B receives it and re-emits the payload. Time-sliced on
    /// one hart. The milestone-heart workload.
    Ipc,
    /// v0.9b RPC: an `rpc-client` `call`s an `rpc-server` over an endpoint; the
    /// server `receive`s, does work, and `reply`s through a one-shot reply cap.
    /// The client blocks across the round-trip (nested-span trace). One hart.
    IpcRpc,
    /// v0.9c badged endpoints: two processes share one endpoint. A `minter`
    /// holds `RECV | MINT` and mints a badged `SEND` cap (observed as a
    /// `CapEvent::Transferred` carrying the badge); a `client` holds `SEND` only
    /// and is refused when it tries to mint (`SyscallRefused`). Same binary,
    /// outcome differs by capability. One hart.
    BadgeMint,
    /// v0.9c cap-transfer-in-reply: a `badge-handout-server` (`RECV | MINT`)
    /// mints a badged `SEND` cap per request and **hands it back in the reply**;
    /// a `badge-handout-client` `call`s, receives the badged cap, and signals
    /// success. Proves a server can return capabilities to a client. One hart.
    BadgeHandout,
    /// Userspace-defined-metrics demo (debt #2): a `probe` program registers
    /// its own metric (`snitchos.probe.custom`, a gauge) through its bootstrap
    /// `TelemetrySink` cap and emits to it via the handle the kernel hands back —
    /// then deliberately emits through an *unregistered* handle, which the kernel
    /// must refuse (`SyscallRefused`), not silently emit. Proves a process names
    /// its own metrics without the kernel knowing them ahead of time, and that
    /// the per-process metric table is the forgery boundary. Not a storm.
    Probe,
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
    /// Boot-stack (task 0) guard smoke: a kernel task stores into the boot stack's
    /// unmapped guard page, faulting at the store; the trap handler recognizes the
    /// boot guard region and snitches `Log("kernel stack overflow: boot stack …")`.
    /// Proves `mmu::guard_boot_stack` unmapped the page + the handler names it.
    /// `itest-workloads` only.
    BootStackGuard,
    /// v0.10 `RAMfs`: an `fs` server (`RECV | MINT`) serves a flat in-memory
    /// filesystem to an `fs-client` over one endpoint. The client connects
    /// (badge 0) to be minted a root File cap (`pack(root, READ)`), then issues
    /// FS requests against it; the server demuxes inode + rights by badge. One
    /// hart.
    Fs,
    /// v0.12 notification smoke: a `notify-waiter` parent creates a notification,
    /// `Spawn`s a `notify-signaller` child delegating the cap, then `WaitNotify`s
    /// on it; the child `Signal`s a known bit mask. Proves the async kernel→user
    /// wake crosses the task boundary — a `NotifySignal`→`NotifyWait` edge on the
    /// wire. One hart.
    NotifySmoke,
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
            Self::SpawnStorm
                | Self::IpiPong
                | Self::ShootdownStorm
                | Self::MutexStorm
                | Self::VirtioStorm
                | Self::TlbShootdownVisible
                | Self::PingPong
        )
    }
}

/// Parse a `workload=<name>` selection out of the bootargs string.
/// Returns `None` when no `workload=` key is present (run the default
/// demo) or when the value is unrecognised (also default — a typo
/// should fail safe to default rather than silently match something).
pub fn select(bootargs: &str) -> Option<WorkloadKind> {
    bootargs
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("workload="))
        .and_then(|name| match name {
            "smp" => Some(WorkloadKind::Smp),
            "smp-spsc" => Some(WorkloadKind::SmpSpsc),
            "smp-spsc-batch" => Some(WorkloadKind::SmpSpscBatch),
            "frame-oom" => Some(WorkloadKind::FrameOom),
            "heap-oom" => Some(WorkloadKind::HeapOom),
            "spawn-storm" => Some(WorkloadKind::SpawnStorm),
            "ipi-pong" => Some(WorkloadKind::IpiPong),
            "shootdown-storm" => Some(WorkloadKind::ShootdownStorm),
            "mutex-storm" => Some(WorkloadKind::MutexStorm),
            "virtio-storm" => Some(WorkloadKind::VirtioStorm),
            "tlb-shootdown" => Some(WorkloadKind::TlbShootdownVisible),
            "ping-pong" => Some(WorkloadKind::PingPong),
            "userspace" => Some(WorkloadKind::Userspace),
            "userspace-fault" => Some(WorkloadKind::UserspaceFault),
            "userspace-bad-ptr" => Some(WorkloadKind::UserspaceBadPtr),
            "userspace-span-flood" => Some(WorkloadKind::UserspaceSpanFlood),
            "workers" => Some(WorkloadKind::Workers),
            "heap-grow" => Some(WorkloadKind::HeapGrow),
            "user-hog" => Some(WorkloadKind::UserHog),
            "syscall-hog" => Some(WorkloadKind::SyscallHog),
            "console-echo" => Some(WorkloadKind::ConsoleEcho),
            "stitch-repl" => Some(WorkloadKind::StitchRepl),
            "stitch-fs" => Some(WorkloadKind::StitchFs),
            "spawn-image" => Some(WorkloadKind::SpawnImage),
            "manifest-iface" => Some(WorkloadKind::ManifestIface),
            "probe" => Some(WorkloadKind::Probe),
            "stack-guard" => Some(WorkloadKind::StackGuard),
            "stack-overflow-deep" => Some(WorkloadKind::StackOverflowDeep),
            "boot-stack-guard" => Some(WorkloadKind::BootStackGuard),
            "spawn-demo" => Some(WorkloadKind::SpawnDemo),
            "spawn-reap" => Some(WorkloadKind::SpawnReap),
            "wait-any" => Some(WorkloadKind::WaitAny),
            "init" => Some(WorkloadKind::Init),
            "endpoint-create" => Some(WorkloadKind::EndpointCreate),
            "demo" => Some(WorkloadKind::Demo),
            "priorities" => Some(WorkloadKind::Priorities),
            "block-wake" => Some(WorkloadKind::BlockWake),
            "ipc" => Some(WorkloadKind::Ipc),
            "ipc-rpc" => Some(WorkloadKind::IpcRpc),
            "badge-mint" => Some(WorkloadKind::BadgeMint),
            "badge-handout" => Some(WorkloadKind::BadgeHandout),
            "fs" => Some(WorkloadKind::Fs),
            "notify-smoke" => Some(WorkloadKind::NotifySmoke),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_smp_from_workload_key() {
        assert_eq!(select("workload=smp"), Some(WorkloadKind::Smp));
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
    fn selects_frame_oom() {
        assert_eq!(select("workload=frame-oom"), Some(WorkloadKind::FrameOom));
    }

    #[test]
    fn selects_notify_smoke() {
        assert_eq!(select("workload=notify-smoke"), Some(WorkloadKind::NotifySmoke));
    }

    #[test]
    fn selects_heap_oom() {
        assert_eq!(select("workload=heap-oom"), Some(WorkloadKind::HeapOom));
    }

    #[test]
    fn selects_fs() {
        assert_eq!(select("workload=fs"), Some(WorkloadKind::Fs));
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
    fn selects_storm_workloads() {
        assert_eq!(select("workload=spawn-storm"), Some(WorkloadKind::SpawnStorm));
        assert_eq!(select("workload=ipi-pong"), Some(WorkloadKind::IpiPong));
        assert_eq!(select("workload=shootdown-storm"), Some(WorkloadKind::ShootdownStorm));
        assert_eq!(select("workload=mutex-storm"), Some(WorkloadKind::MutexStorm));
        assert_eq!(select("workload=virtio-storm"), Some(WorkloadKind::VirtioStorm));
    }

    #[test]
    fn selects_tlb_shootdown() {
        assert_eq!(
            select("workload=tlb-shootdown"),
            Some(WorkloadKind::TlbShootdownVisible),
        );
    }

    #[test]
    fn tlb_shootdown_is_a_storm() {
        // Heartbeat-driven (hart 0's round loop runs once on the first
        // tick) and spawns its own hart-1 reader — so it is
        // storm-classified: the default `hart_1_probe` is skipped and
        // its driver runs from `emit_storm_metrics`.
        assert!(WorkloadKind::TlbShootdownVisible.is_storm());
    }

    #[test]
    fn selects_userspace() {
        assert_eq!(select("workload=userspace"), Some(WorkloadKind::Userspace));
    }

    #[test]
    fn selects_workers() {
        assert_eq!(select("workload=workers"), Some(WorkloadKind::Workers));
    }

    #[test]
    fn selects_heap_grow() {
        assert_eq!(select("workload=heap-grow"), Some(WorkloadKind::HeapGrow));
    }

    #[test]
    fn selects_user_hog() {
        assert_eq!(select("workload=user-hog"), Some(WorkloadKind::UserHog));
    }

    #[test]
    fn selects_priorities() {
        assert_eq!(select("workload=priorities"), Some(WorkloadKind::Priorities));
    }

    #[test]
    fn selects_syscall_hog() {
        assert_eq!(select("workload=syscall-hog"), Some(WorkloadKind::SyscallHog));
    }

    #[test]
    fn syscall_hog_is_not_a_storm() {
        assert!(!WorkloadKind::SyscallHog.is_storm());
    }

    #[test]
    fn selects_console_echo() {
        assert_eq!(select("workload=console-echo"), Some(WorkloadKind::ConsoleEcho));
    }

    #[test]
    fn selects_stitch_repl() {
        assert_eq!(select("workload=stitch-repl"), Some(WorkloadKind::StitchRepl));
    }

    #[test]
    fn selects_spawn_image() {
        assert_eq!(select("workload=spawn-image"), Some(WorkloadKind::SpawnImage));
    }

    #[test]
    fn selects_manifest_iface() {
        assert_eq!(select("workload=manifest-iface"), Some(WorkloadKind::ManifestIface));
    }

    #[test]
    fn selects_stitch_fs() {
        assert_eq!(select("workload=stitch-fs"), Some(WorkloadKind::StitchFs));
    }

    #[test]
    fn selects_probe() {
        assert_eq!(select("workload=probe"), Some(WorkloadKind::Probe));
        assert!(!WorkloadKind::Probe.is_storm());
    }

    #[test]
    fn selects_stack_guard() {
        assert_eq!(select("workload=stack-guard"), Some(WorkloadKind::StackGuard));
    }

    #[test]
    fn selects_boot_stack_guard() {
        assert_eq!(
            select("workload=boot-stack-guard"),
            Some(WorkloadKind::BootStackGuard)
        );
    }

    #[test]
    fn selects_stack_overflow_deep() {
        assert_eq!(
            select("workload=stack-overflow-deep"),
            Some(WorkloadKind::StackOverflowDeep)
        );
    }

    #[test]
    fn selects_spawn_demo() {
        assert_eq!(select("workload=spawn-demo"), Some(WorkloadKind::SpawnDemo));
    }

    #[test]
    fn selects_spawn_reap() {
        assert_eq!(select("workload=spawn-reap"), Some(WorkloadKind::SpawnReap));
    }

    #[test]
    fn selects_block_wake() {
        assert_eq!(select("workload=block-wake"), Some(WorkloadKind::BlockWake));
    }

    #[test]
    fn selects_ipc() {
        assert_eq!(select("workload=ipc"), Some(WorkloadKind::Ipc));
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
    fn selects_userspace_fault() {
        assert_eq!(select("workload=userspace-fault"), Some(WorkloadKind::UserspaceFault));
    }

    #[test]
    fn selects_userspace_span_flood() {
        assert_eq!(
            select("workload=userspace-span-flood"),
            Some(WorkloadKind::UserspaceSpanFlood)
        );
    }

    #[test]
    fn userspace_workloads_are_not_storms() {
        assert!(!WorkloadKind::Userspace.is_storm());
        assert!(!WorkloadKind::UserspaceFault.is_storm());
    }

    #[test]
    fn selects_userspace_bad_ptr() {
        assert_eq!(select("workload=userspace-bad-ptr"), Some(WorkloadKind::UserspaceBadPtr));
        assert!(!WorkloadKind::UserspaceBadPtr.is_storm());
    }

    #[test]
    fn selects_ping_pong() {
        assert_eq!(select("workload=ping-pong"), Some(WorkloadKind::PingPong));
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
}
