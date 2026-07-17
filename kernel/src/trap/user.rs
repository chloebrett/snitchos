//! Userspace program embedding and loading (v0.7a+).
//!
//! Each userspace program (`user/hello`'s binaries + `user/fs`'s) is baked into
//! the kernel image at build time — one `include_bytes!(env!(...))` static per
//! `USER_PROGRAMS` row in `build.rs` — and described by a [`ProgramSpec`] in the
//! program registry below. [`spawn_program`] launches one via the single
//! generic [`program_entry`], which carries the spec through the task's `arg`
//! word; `kmain` selects which to spawn per workload.
//!
//! [`load`] parses an embedded ELF with [`kernel_proc::elf`] and maps its
//! segments into a fresh per-process root page table (kernel high-half
//! shared in) with the `U` bit; [`enter`] switches `satp` and drops to
//! U-mode at the entry point.

use alloc::collections::BTreeMap;

use kernel_boot::bootargs::WorkloadKind;
use kernel_proc::cap::Rights;
use kernel_proc::elf::{self, SegmentPerms};
use kernel_mem::mmu::{MapError, PtePerms};
use kernel_proc::sched::Priority;
use protocol::StringId;

use crate::frame::{self, FRAME_SIZE};
use crate::process::Process;
use crate::sync::Once;
use crate::{mmu, tracing};

/// The `workload=userspace` program: emits one telemetry syscall, then spins.
pub static HELLO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_ELF"));

/// The `workload=userspace-fault` program: emits a marker, then reads a
/// kernel VA to prove the `U`-bit firewall faults it.
pub static FAULTER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FAULTER_ELF"));
pub static BAD_PTR_ELF: &[u8] = include_bytes!(env!("SNITCHOS_BAD_PTR_ELF"));

/// The `workload=userspace-span-flood` program: opens spans with many distinct
/// names to exceed the per-process span-name quota.
pub static SPAN_FLOOD_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SPAN_FLOOD_ELF"));

/// The `workload=workers` programs: two cooperative workers that each loop
/// {open span, bump progress, yield}. Distinct binaries (own page tables,
/// own span names) so they're individually attributable as they share a hart.
pub static WORKER_A_ELF: &[u8] = include_bytes!(env!("SNITCHOS_WORKER_A_ELF"));
pub static WORKER_B_ELF: &[u8] = include_bytes!(env!("SNITCHOS_WORKER_B_ELF"));

/// The `workload=heap-grow` program: allocates past the runtime's per-region
/// map size to force on-demand heap growth via `MapAnon`.
pub static HEAP_GROW_ELF: &[u8] = include_bytes!(env!("SNITCHOS_HEAP_GROW_ELF"));

/// The `workload=user-hog` program: a tight U-mode `loop {}` (no syscalls, no
/// `yield`) — the v0.8 preemption fixture. Starves a co-located cooperative
/// peer until the timer preempts it.
pub static USER_HOG_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_HOG_ELF"));

/// The `workload=syscall-hog` program: a tight loop of cheap ambient `DebugWrite`
/// syscalls (no `yield`) — the v0.8 preemption *guard*. Proves a syscall-heavy
/// U-mode task is still preempted despite spending most of its time in
/// interrupt-masked S-mode.
pub static SYSCALL_HOG_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SYSCALL_HOG_ELF"));

/// The `workload=console-echo` program: loops `ConsoleRead` → `DebugWrite`,
/// echoing typed UART input — the Tier-0 polled-console-input demo.
pub static CONSOLE_ECHO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_CONSOLE_ECHO_ELF"));
pub static STITCH_REPL_ELF: &[u8] = include_bytes!(env!("SNITCHOS_STITCH_REPL_ELF"));

/// The `workload=probe` program: registers its own metric (`snitchos.probe.custom`)
/// through its bootstrap `TelemetrySink` cap and emits to it — the
/// userspace-defined-metrics demo (debt #2).
pub static PROBE_ELF: &[u8] = include_bytes!(env!("SNITCHOS_PROBE_ELF"));

/// The `workload=spawn-demo` parent: delegates its span cap and `Spawn`s the
/// `spawnee` child — the spawn-with-caps demo.
pub static SPAWNER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SPAWNER_ELF"));

/// The `spawnee` child (spawnable id 0): opens a span through its delegated cap.
/// Launched at runtime via `Spawn`, not at boot, so it has no `LAYOUTS` entry —
/// only a [`SPAWNABLE`] registry row.
pub static SPAWNEE_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SPAWNEE_ELF"));

/// The `workload=wait-any` parent: spawns a never-exiting `spinner` + an exiting
/// `spawnee`, then `WaitAny`s for whichever exits — the supervising-parent demo.
pub static SUPERVISOR_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SUPERVISOR_ELF"));

/// The `workload=supervised` generic supervisor root (supervision step 2): walks
/// a data-driven service table, bringing services up in dependency order and
/// consulting the pure `supervision` policy on each exit (restart/stop/escalate).
pub static SUPERVISED_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SUPERVISED_ELF"));

/// The `cap-reporter` supervised service (supervision step 4): each incarnation
/// enumerates its own `cap_list`, reports whether the supervisor's re-granted
/// endpoint landed, then exits non-zero — the cap-re-grant crash-loop oracle.
pub static CAP_REPORTER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_CAP_REPORTER_ELF"));

/// The `workload=supervised-ipc` supervisor (supervision FU2): owns a durable
/// endpoint and grants a minted `SEND` to a persistent client + a minted `RECV` to
/// a crashing server, respawning the server so the client's cap survives restarts.
pub static SUPERVISED_IPC_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SUPERVISED_IPC_ELF"));

/// The `ipc-echo-server` supervised service (spawnable id 8): serves one request
/// on its re-granted `RECV` cap, then crashes — forcing a restart per request.
pub static IPC_ECHO_SERVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_ECHO_SERVER_ELF"));

/// The `ipc-echo-client` (spawnable id 9): sends a short series over its one minted
/// `SEND` cap, each send rendezvousing with whichever server incarnation is alive.
pub static IPC_ECHO_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_ECHO_CLIENT_ELF"));

/// The `spinner` child (spawnable id 3): loops forever, never exits. A long-lived
/// sibling so `WaitAny` deterministically returns the *other* child.
pub static SPINNER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SPINNER_ELF"));

/// The `workload=supervised-shutdown` graceful-shutdown supervisor (v2a step 4):
/// brings a dependency tree up in `startup_order`, then tears it down in
/// `teardown_order` — `Signal`ing cooperative services (clean exit) and `Kill`ing a
/// forced one — so the tree comes down in the exact reverse of how it went up.
pub static SUPERVISED_SHUTDOWN_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SUPERVISED_SHUTDOWN_ELF"));

/// The `svc-worker` cooperative service (spawnable id 10): proves it came up, then
/// parks on a delegated shutdown notification and `exit(0)`s cleanly when signalled —
/// the cooperative half of the graceful-shutdown demo.
pub static SVC_WORKER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SVC_WORKER_ELF"));

/// The `workload=kill-no-cap` negative (v2a): a process holding no `Object::Process`
/// cap tries to `Kill` and is refused — proving the kill authority is real, not
/// ambient. It survives and reports the refusal.
pub static KILL_NO_CAP_ELF: &[u8] = include_bytes!(env!("SNITCHOS_KILL_NO_CAP_ELF"));

/// The `workload=user-on-hart0` de-risk (v2b step 1): a trivial userspace program the
/// kernel places on hart 0; it opens a span whose `SpanStart` carries `hart_id == 0`,
/// proving U-mode runs on the boot hart.
pub static HART_PROBE_ELF: &[u8] = include_bytes!(env!("SNITCHOS_HART_PROBE_ELF"));

/// The `hart-spinner` victim (spawnable id 11) for the cross-hart Kill itest (v2b): it
/// opens a liveness span then tight-loops, staying *running* on hart 0 so a `Kill`
/// from hart 1 exercises the `running_remote` path.
pub static HART_SPINNER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_HART_SPINNER_ELF"));

/// The `workload=xhart-kill` supervisor (v2b step 4): runs on hart 1, `SpawnOn`s the
/// `hart-spinner` victim to hart 0, then cross-hart `Kill`s + reaps it.
pub static XHART_KILLER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_XHART_KILLER_ELF"));

/// The `hung-service` victim (spawnable id 12) for hung detection (v2b): beats a
/// liveness notification a few times, then wedges (tight-loop, alive but stuck).
pub static HUNG_SERVICE_ELF: &[u8] = include_bytes!(env!("SNITCHOS_HUNG_SERVICE_ELF"));

/// The `workload=hung-detect` supervisor (v2b): `wait_timeout`s a liveness
/// notification and force-`Kill`s the service when a beat fails to arrive in time.
pub static HUNG_SUPERVISOR_ELF: &[u8] = include_bytes!(env!("SNITCHOS_HUNG_SUPERVISOR_ELF"));

/// The `workload=init` supervising root: spawns a child (delegating its span cap)
/// and reaps it via `WaitAny` — the delegation-graph root (v0.13).
pub static INIT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_INIT_ELF"));

/// The `workload=endpoint-create` program: manufactures its own endpoint via
/// `EndpointCreate` and mints a badged `SEND` cap on it — proves the syscall hands
/// back a real owning `RECV | MINT` cap (v0.13).
pub static EP_MAKER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_EP_MAKER_ELF"));

/// The `workload=spawn-reap` parent: spawns + `Wait`s a memory-hungry `memhog`
/// child 30 times. Drives the reclaim integration test — leaks (and OOMs)
/// without per-process teardown on Exit.
pub static REAPER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_REAPER_ELF"));

/// The `memhog` child (spawnable id 1): allocates + touches ~4 MiB then exits.
/// Launched at runtime via `Spawn`, so it has only a [`SPAWNABLE`] registry row.
pub static MEMHOG_ELF: &[u8] = include_bytes!(env!("SNITCHOS_MEMHOG_ELF"));

/// The `workload=notify-smoke` parent: creates a notification, `Spawn`s the
/// `notify-signaller` child (delegating the cap), then `WaitNotify`s on it.
pub static NOTIFY_WAITER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_NOTIFY_WAITER_ELF"));

/// The `notify-signaller` child (spawnable id 2): `Signal`s its delegated
/// notification cap, waking the parent, then exits. SPAWNABLE-only.
pub static NOTIFY_SIGNALLER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_NOTIFY_SIGNALLER_ELF"));

/// The `workload=ipc` programs: `ipc-sender` holds a `SEND` cap and sends one
/// inline message; `ipc-receiver` holds a `RECV` cap, receives it, and
/// re-emits the payload. They rendezvous over one kernel-brokered endpoint.
pub static IPC_SENDER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_SENDER_ELF"));
pub static IPC_RECEIVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_RECEIVER_ELF"));

/// The `workload=ipc-rpc` programs: `rpc-client` `call`s and `rpc-server`
/// `reply`s over the shared endpoint — the v0.9b RPC round-trip.
pub static RPC_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_RPC_CLIENT_ELF"));
pub static RPC_SERVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_RPC_SERVER_ELF"));

/// The `workload=badge-mint` program (v0.9c): one binary, two roles by rights —
/// a `RECV | MINT` minter and a `SEND`-only client that's refused.
pub static BADGE_MINT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_BADGE_MINT_ELF"));

/// The `workload=badge-handout` programs (v0.9c cap-transfer-in-reply): a
/// `RECV | MINT` server that mints + hands back a badged cap, and a `SEND`
/// client that `call`s and receives it.
pub static BADGE_HANDOUT_SERVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_BADGE_HANDOUT_SERVER_ELF"));
pub static BADGE_HANDOUT_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_BADGE_HANDOUT_CLIENT_ELF"));

/// The `workload=fs` programs (v0.10 RAMfs): an `fs-server` (`RECV | MINT`)
/// that mints a root File cap on connect and serves the filesystem, and an
/// `fs-client` (`SEND`) that attaches and issues requests.
pub static FS_SERVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FS_SERVER_ELF"));
pub static FS_SERVER_SEEDED_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FS_SERVER_SEEDED_ELF"));
pub static FS_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FS_CLIENT_ELF"));
pub static VIEWER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_VIEWER_ELF"));
pub static VIEW_DEMO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_VIEW_DEMO_ELF"));
pub static SHELL_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SHELL_ELF"));
pub static SPAWN_IMAGE_DEMO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SPAWN_IMAGE_DEMO_ELF"));
pub static SATISFIER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_SATISFIER_ELF"));
pub static IFACE_READER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IFACE_READER_ELF"));

/// The counter a U-mode page fault bumps — the isolation firewall doing its
/// job. Registered alongside the other userspace counters in [`init_metric`];
/// read by the fault handler.
static USER_FAULT_METRIC: Once<StringId> = Once::new();

/// The counter the kernel bumps each time it **grants** a capability —
/// authority being created. Bumped once per bootstrap grant: the
/// `TelemetrySink` + `SpanSink` pair every process gets (`run`), plus the
/// `Endpoint` cap for IPC processes (`run_ipc`) — so 2 or 3 per process. The
/// richer `CapEvent` frame is the sequenced follow-on. Registered alongside the
/// others so the grant site emits without interning.
static CAP_GRANTS_METRIC: Once<StringId> = Once::new();

/// The counter the kernel bumps when it **refuses** a capability
/// invocation — an authority decision going the other way. Bumped from the
/// syscall trap handler (hart 1), so pre-registered here to avoid interning
/// in trap context (same discipline as `faults_total`).
static CAP_DENIED_METRIC: Once<StringId> = Once::new();

/// The counter the kernel bumps when a user process **exits** (the `Exit`
/// syscall). Emitted once per exit from the trap handler, so pre-registered
/// here. Proves the process terminated cleanly rather than spinning.
static USER_EXITS_METRIC: Once<StringId> = Once::new();

/// A loaded program, ready to enter.
pub struct Loaded {
    /// The entry-point VA (`e_entry`) to put in `sepc`.
    pub entry: usize,
}

/// Why loading the embedded program failed.
#[derive(Debug)]
#[allow(dead_code, reason = "fields are surfaced via Debug in the load-failure panic")]
pub enum LoadError {
    /// The embedded image is not a valid ELF we can load.
    Parse(elf::ElfError),
    /// The image's segments cannot be mapped as asked — e.g. a page that would
    /// end up both writable and executable. Refused, not mapped.
    Plan(elf::PlanError),
    /// The frame allocator is exhausted.
    OutOfFrames,
    /// Installing a page-table entry failed.
    Map(MapError),
}

/// Register the userspace counters. Call once at boot, before entering
/// U-mode, so the syscall/fault handlers can emit without interning in trap
/// context.
pub fn init_metric() {
    USER_FAULT_METRIC.call_once(|| tracing::register_counter("snitchos.user.faults_total"));
    CAP_GRANTS_METRIC.call_once(|| tracing::register_counter("snitchos.cap.grants_total"));
    CAP_DENIED_METRIC.call_once(|| tracing::register_counter("snitchos.cap.denied_total"));
    USER_EXITS_METRIC.call_once(|| tracing::register_counter("snitchos.user.exits_total"));
}

/// The `StringId` for the U-mode fault counter (or `None` pre-init).
pub fn user_fault_metric_id() -> Option<StringId> {
    USER_FAULT_METRIC.get().copied()
}

/// The `StringId` for the capability-grant counter (or `None` pre-init).
pub fn cap_grants_metric_id() -> Option<StringId> {
    CAP_GRANTS_METRIC.get().copied()
}

/// The `StringId` for the process-exit counter (or `None` pre-init).
pub fn user_exits_metric_id() -> Option<StringId> {
    USER_EXITS_METRIC.get().copied()
}

/// The `StringId` for the denied-invocation counter (or `None` pre-init).
pub fn cap_denied_metric_id() -> Option<StringId> {
    CAP_DENIED_METRIC.get().copied()
}



// ---- Program registry --------------------------------------------------
//
// One `ProgramSpec` per userspace program describes everything that varied
// across the old per-program entry functions (ELF, endpoint rights, telemetry
// counter). [`spawn_program`] hands a spec's address to the task as its generic
// `arg` word; the single [`program_entry`] reads it back and launches — so the
// 18 near-identical `*_main_entry` functions collapsed into this one entry plus
// a `ProgramSpec` table (one `pub static` per program, below).

/// How a userspace program is launched, beyond its embedded ELF.
enum Launch {
    /// Ambient authorities only (telemetry + span sinks); no endpoint.
    Plain,
    /// Also granted an `Endpoint` cap over the shared `DEMO_ENDPOINT` with
    /// `rights_bits`. Its `TelemetrySink` is bare authority like every program's;
    /// any metric it wants it registers at runtime (debt #2).
    Ipc { rights_bits: u32 },
}

/// A userspace program: its embedded ELF plus how to launch it. One `'static`
/// per program; [`spawn_program`] hands its address to the task as the generic
/// `arg` word and [`program_entry`] reads it back.
pub struct ProgramSpec {
    elf: &'static [u8],
    launch: Launch,
}

/// `workload=userspace`: the `hello` demo — ambient telemetry only, no endpoint.
pub static HELLO: ProgramSpec = ProgramSpec { elf: HELLO_ELF, launch: Launch::Plain };

/// `workload=userspace-fault`: the isolation probe (reads a kernel VA — must fault).
pub static FAULTER: ProgramSpec = ProgramSpec { elf: FAULTER_ELF, launch: Launch::Plain };

/// `workload=userspace-bad-ptr`: the user-pointer validation probe (passes an
/// unmapped user VA to `DebugWrite` — the kernel must refuse, not fault).
pub static BAD_PTR: ProgramSpec = ProgramSpec { elf: BAD_PTR_ELF, launch: Launch::Plain };

/// `workload=userspace-span-flood`: the span-quota probe.
pub static SPAN_FLOOD: ProgramSpec = ProgramSpec { elf: SPAN_FLOOD_ELF, launch: Launch::Plain };

/// `workload=workers` / preemption-peer: cooperative demo worker A.
pub static WORKER_A: ProgramSpec = ProgramSpec { elf: WORKER_A_ELF, launch: Launch::Plain };

/// `workload=workers` / priority-demo low: cooperative demo worker B.
pub static WORKER_B: ProgramSpec = ProgramSpec { elf: WORKER_B_ELF, launch: Launch::Plain };

/// `workload=heap-grow`: the heap-growth probe.
pub static HEAP_GROW: ProgramSpec = ProgramSpec { elf: HEAP_GROW_ELF, launch: Launch::Plain };

/// `workload=user-hog`: the uncooperative CPU hog (tight U-mode loop, no syscalls).
pub static USER_HOG: ProgramSpec = ProgramSpec { elf: USER_HOG_ELF, launch: Launch::Plain };

/// `workload=syscall-hog`: the syscall-spamming hog.
pub static SYSCALL_HOG: ProgramSpec = ProgramSpec { elf: SYSCALL_HOG_ELF, launch: Launch::Plain };

/// `workload=console-echo`: the Tier-0 console echo loop (ambient — `ConsoleRead`
/// and `DebugWrite` need no caps).
pub static CONSOLE_ECHO: ProgramSpec = ProgramSpec { elf: CONSOLE_ECHO_ELF, launch: Launch::Plain };

/// `workload=stitch-repl`: the Stitch interpreter as a userspace REPL. Ambient —
/// `ConsoleRead`/`ConsoleWrite` need no caps.
pub static STITCH_REPL: ProgramSpec = ProgramSpec { elf: STITCH_REPL_ELF, launch: Launch::Plain };

/// `workload=stitch-fs`: the same REPL ELF, but IPC-launched with a `SEND | MINT`
/// cap on the FS endpoint (delivered at `a2`) so it can both read files off the
/// filesystem *and* re-delegate: a shell is a delegating authority, so it holds
/// `MINT` to grant narrower badged caps (backing `grant`/`revoke`). The plain
/// `stitch-repl` workload uses [`STITCH_REPL`] (no fs, no mint).
pub static STITCH_REPL_IPC: ProgramSpec =
    ipc_user(STITCH_REPL_ELF, Rights::SEND.bits() | Rights::MINT.bits());

/// `workload=probe`: the userspace-defined-metrics demo. Ambient launch — it
/// registers + emits through its bootstrap `TelemetrySink` cap, which it
/// receives at startup like every other program (no endpoint).
pub static PROBE: ProgramSpec = ProgramSpec { elf: PROBE_ELF, launch: Launch::Plain };

/// `workload=spawn-demo`: the spawn-with-caps parent (ambient — `Spawn` needs no
/// cap, and it delegates from its own bootstrap caps).
pub static SPAWNER: ProgramSpec = ProgramSpec { elf: SPAWNER_ELF, launch: Launch::Plain };

/// `workload=wait-any`: the supervising parent (ambient — `Spawn`/`WaitAny` need
/// no cap; it delegates its span cap to the exiting child).
pub static SUPERVISOR: ProgramSpec = ProgramSpec { elf: SUPERVISOR_ELF, launch: Launch::Plain };

/// `workload=supervised`: the generic supervisor root — data-driven service
/// table + policy-driven restarts. Holds only its bootstrap caps (Launch::Plain);
/// it spawns its services (spinner/spawnee) from the `SPAWNABLE` registry.
pub static SUPERVISED: ProgramSpec = ProgramSpec { elf: SUPERVISED_ELF, launch: Launch::Plain };

/// `workload=supervised-ipc`: the cap-survival supervisor — owns its endpoint and
/// grants from it (Launch::Plain; it `EndpointCreate`s + `Spawn`s its own services).
pub static SUPERVISED_IPC: ProgramSpec = ProgramSpec { elf: SUPERVISED_IPC_ELF, launch: Launch::Plain };

/// `workload=supervised-shutdown`: the graceful reverse-dep shutdown supervisor (v2a
/// step 4) — spawns its service tree + shutdown notifications, then tears it down
/// (Launch::Plain; it `Spawn`s/`NotifyCreate`s its own services).
pub static SUPERVISED_SHUTDOWN: ProgramSpec = ProgramSpec { elf: SUPERVISED_SHUTDOWN_ELF, launch: Launch::Plain };

/// `workload=kill-no-cap`: the negative — a lone process that tries to `Kill` without
/// holding a `Process` cap and is refused (Launch::Plain; holds only bootstrap caps).
pub static KILL_NO_CAP: ProgramSpec = ProgramSpec { elf: KILL_NO_CAP_ELF, launch: Launch::Plain };

/// `workload=user-on-hart0`: the multi-hart de-risk — a trivial program placed on hart
/// 0 that opens a span (Launch::Plain; holds only bootstrap caps).
pub static HART_PROBE: ProgramSpec = ProgramSpec { elf: HART_PROBE_ELF, launch: Launch::Plain };

/// `workload=xhart-kill`: the cross-hart Kill supervisor — runs on hart 1, `SpawnOn`s
/// its victim to hart 0 and kills it (Launch::Plain; spawns its own child at runtime).
pub static XHART_KILLER: ProgramSpec = ProgramSpec { elf: XHART_KILLER_ELF, launch: Launch::Plain };

/// `workload=hung-detect`: the hung-detection supervisor — `wait_timeout`s a liveness
/// notification and kills the wedged service (Launch::Plain; spawns its own child).
pub static HUNG_SUPERVISOR: ProgramSpec = ProgramSpec { elf: HUNG_SUPERVISOR_ELF, launch: Launch::Plain };

/// `workload=init`: the supervising root — spawns + `WaitAny`-reaps a child,
/// delegating its span cap downward. Holds only its bootstrap caps (Launch::Plain).
pub static INIT: ProgramSpec = ProgramSpec { elf: INIT_ELF, launch: Launch::Plain };

/// `workload=endpoint-create`: manufactures its own endpoint via `EndpointCreate`
/// (ambient — no kernel-created endpoint, Launch::Plain) and proves it by minting.
pub static EP_MAKER: ProgramSpec = ProgramSpec { elf: EP_MAKER_ELF, launch: Launch::Plain };

/// `workload=spawn-reap`: the reclaim-test parent (ambient — `Spawn`/`Wait` need
/// no cap; the `memhog` children it spawns inherit no delegated authority).
pub static REAPER: ProgramSpec = ProgramSpec { elf: REAPER_ELF, launch: Launch::Plain };

/// `workload=notify-smoke`: the notification-demo parent (ambient — `NotifyCreate`
/// needs no cap; it delegates the created notification cap to its child).
pub static NOTIFY_WAITER: ProgramSpec = ProgramSpec { elf: NOTIFY_WAITER_ELF, launch: Launch::Plain };

/// An IPC program on the shared `DEMO_ENDPOINT` with `rights_bits` and the
/// default user telemetry sink — now the *only* IPC launch shape (the FS server
/// registers its own denial metric at runtime rather than binding a special
/// kernel-pre-registered counter; debt #2).
const fn ipc_user(elf: &'static [u8], rights_bits: u32) -> ProgramSpec {
    ProgramSpec { elf, launch: Launch::Ipc { rights_bits } }
}

/// `workload=ipc`: the demo sender (`SEND`).
pub static IPC_SENDER: ProgramSpec = ipc_user(IPC_SENDER_ELF, Rights::SEND.bits());

/// `workload=ipc`: the demo receiver (`RECV`).
pub static IPC_RECEIVER: ProgramSpec = ipc_user(IPC_RECEIVER_ELF, Rights::RECV.bits());

/// `workload=ipc-rpc`: the RPC client (`SEND`).
pub static RPC_CLIENT: ProgramSpec = ipc_user(RPC_CLIENT_ELF, Rights::SEND.bits());

/// `workload=ipc-rpc`: the RPC server (`RECV`).
pub static RPC_SERVER: ProgramSpec = ipc_user(RPC_SERVER_ELF, Rights::RECV.bits());

/// `workload=badge-mint`: the minter (`RECV | MINT`). Same ELF as the client.
pub static BADGE_MINTER: ProgramSpec = ipc_user(BADGE_MINT_ELF, Rights::RECV.bits() | Rights::MINT.bits());

/// `workload=badge-mint`: the client (`SEND` only — its mint attempt is refused).
pub static BADGE_MINT_CLIENT: ProgramSpec = ipc_user(BADGE_MINT_ELF, Rights::SEND.bits());

/// `workload=badge-handout`: the server (`RECV | MINT`).
pub static BADGE_HANDOUT_SERVER: ProgramSpec =
    ipc_user(BADGE_HANDOUT_SERVER_ELF, Rights::RECV.bits() | Rights::MINT.bits());

/// `workload=badge-handout`: the client (`SEND`).
pub static BADGE_HANDOUT_CLIENT: ProgramSpec = ipc_user(BADGE_HANDOUT_CLIENT_ELF, Rights::SEND.bits());

/// `workload=fs`: the FS server (`RECV | MINT`). A plain bootstrap sink like
/// every other IPC program — it registers its own `snitchos.fs.denied` gauge at
/// runtime (debt #2), so the kernel no longer special-cases its telemetry.
pub static FS_SERVER: ProgramSpec =
    ipc_user(FS_SERVER_ELF, Rights::RECV.bits() | Rights::MINT.bits());

/// `workload=fs`: the FS client (`SEND`), default user telemetry.
pub static FS_CLIENT: ProgramSpec = ipc_user(FS_CLIENT_ELF, Rights::SEND.bits());

/// `workload=stitch-fs`: the FS server seeded from the build-time fs-image
/// (`RECV | MINT`). Same serve loop as [`FS_SERVER`] but its `RamFs` starts
/// pre-populated, so the Stitch REPL can `:load` a file that already exists.
pub static FS_SERVER_SEEDED: ProgramSpec =
    ipc_user(FS_SERVER_SEEDED_ELF, Rights::RECV.bits() | Rights::MINT.bits());

/// `workload=spawn-image`: reads `/bin/spawnee` off the (seeded) filesystem and
/// spawns it via `SpawnImage`. Holds `SEND` on the FS endpoint (to read the ELF).
pub static SPAWN_IMAGE_DEMO: ProgramSpec = ipc_user(SPAWN_IMAGE_DEMO_ELF, Rights::SEND.bits());

/// `workload=view-demo`: powerbox launcher — connects to the seeded FS, looks
/// up a file with READ rights, spawns the viewer with that cap delegated.
pub static VIEW_DEMO: ProgramSpec = ipc_user(VIEW_DEMO_ELF, Rights::SEND.bits());

/// `workload=shell`: interactive powerbox shell. Holds `SEND` on the seeded FS
/// endpoint, reads commands from the UART, and executes them with least authority.
pub static SHELL: ProgramSpec = ipc_user(SHELL_ELF, Rights::SEND.bits());

/// `workload=manifest-satisfy`: the generic satisfier. Reads a child's declared
/// `needs` off the seeded FS (`user.iface` xattr), matches them against its own caps
/// via `hitch::satisfy`, and `SpawnImage`s the child with the granted handles. Holds
/// `MINT | SEND` on the FS endpoint: `SEND` to read + delegate as-is (`Use`), `MINT`
/// to attenuate a wider cap down to a child's narrower need (`Mint`).
pub static SATISFIER: ProgramSpec =
    ipc_user(SATISFIER_ELF, Rights::MINT.bits() | Rights::SEND.bits());

/// `workload=manifest-iface`: reads `/bin/manifest_demo`'s `user.iface` xattr off
/// the seeded FS (over `GetXattr`), decodes it, and checks the shape — the
/// end-to-end proof of the typed-interface path. Holds `SEND` on the FS endpoint.
pub static IFACE_READER: ProgramSpec = ipc_user(IFACE_READER_ELF, Rights::SEND.bits());

/// The single entry function for every userspace program. The scheduler has
/// switched us in and our `arg` word holds our [`ProgramSpec`] address (set by
/// [`spawn_program`]); resolve it and launch. Never returns.
pub extern "C" fn program_entry() -> ! {
    let arg = crate::sched::current_task_arg();
    // SAFETY: `arg` is the address of a `'static ProgramSpec` set by
    // `spawn_program` at spawn time. A `'static` lives for the whole kernel
    // lifetime and nothing mutates it, so dereferencing it here is sound.
    let spec: &'static ProgramSpec = unsafe { &*(arg as *const ProgramSpec) };
    match &spec.launch {
        Launch::Plain => run(spec.elf),
        Launch::Ipc { rights_bits } => {
            let ep = *crate::ipc::DEMO_ENDPOINT
                .get()
                .expect("ipc endpoint created before an IPC program runs");
            let rights = Rights::from_bits(*rights_bits);
            run_ipc(spec.elf, ep, rights)
        }
    }
}

/// Spawn `program` on `hart` as task `name` at `priority`, stashing its
/// [`ProgramSpec`] address in the task's `arg` word for [`program_entry`] to
/// read back. The userspace counterpart to [`crate::sched::spawn_on`].
pub fn spawn_program(
    hart: usize,
    name: &str,
    program: &'static ProgramSpec,
    priority: Priority,
) -> kernel_proc::sched::TaskId {
    crate::sched::spawn_on_with_arg(hart, name, program_entry, core::ptr::from_ref(program) as usize, priority)
}

/// Map a kernel capability object to its wire [`protocol::CapObject`] kind, for
/// snitching a `CapEvent` on a delegated grant or revoke.
pub(crate) fn cap_object_kind(object: kernel_proc::cap::Object) -> protocol::CapObject {
    use kernel_proc::cap::Object;
    match object {
        Object::TelemetrySink => protocol::CapObject::TelemetrySink,
        Object::SpanSink => protocol::CapObject::SpanSink,
        Object::Endpoint { .. } => protocol::CapObject::Endpoint,
        Object::Reply { .. } => protocol::CapObject::Reply,
        Object::Notification { .. } => protocol::CapObject::Notification,
        Object::Process { .. } => protocol::CapObject::Process,
    }
}

/// The image a spawned child runs: an embedded program (`&'static`, shared with
/// `Spawn`) or a caller-supplied buffer copied in for `SpawnImage` (owned, freed
/// after it's mapped).
enum ProgramImage {
    Embedded(&'static [u8]),
    Owned(alloc::boxed::Box<[u8]>),
}

/// A pending spawn: the program image plus the caps the parent delegated.
/// Heap-allocated by [`spawn_process_with_caps`]/[`spawn_image_with_caps`], its
/// pointer stashed in the child task's arg, and reclaimed by [`spawned_entry`]
/// when the child first runs.
struct SpawnRequest {
    image: ProgramImage,
    /// Each delegated cap paired with its source holding's global cap id (the
    /// `parent_cap_id` for the child's `CapEvent::Transferred`).
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
}

/// Task entry for a spawned child: reclaim its [`SpawnRequest`] from the task
/// arg, then build + enter the process with bootstrap + delegated caps. The
/// `Box<SpawnRequest>` (and any owned image inside it) is freed here — for an
/// owned image, `run_image_with_caps` drops it right after the load.
pub extern "C" fn spawned_entry() -> ! {
    let arg = crate::sched::current_task_arg();
    // SAFETY: `arg` is the raw pointer of a `Box<SpawnRequest>` leaked by
    // `spawn_process_with_caps`/`spawn_image_with_caps` for exactly this task;
    // reclaimed once here.
    let req = unsafe { alloc::boxed::Box::from_raw(arg as *mut SpawnRequest) };
    let SpawnRequest { image, delegated } = *req;
    match image {
        ProgramImage::Embedded(image) => run_with_caps(image, delegated),
        ProgramImage::Owned(image) => run_image_with_caps(image, delegated),
    }
}

/// Spawn a child task running `image` with `delegated` caps, on `hart` at
/// `priority`. The userspace-`Spawn` counterpart to [`spawn_program`]: it boxes a
/// [`SpawnRequest`] and stashes its pointer in the task arg for [`spawned_entry`]
/// to pick up and reclaim.
pub fn spawn_process_with_caps(
    hart: usize,
    name: &str,
    image: &'static [u8],
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
    priority: kernel_proc::sched::Priority,
) -> kernel_proc::sched::TaskId {
    let req = alloc::boxed::Box::new(SpawnRequest { image: ProgramImage::Embedded(image), delegated });
    let arg = alloc::boxed::Box::into_raw(req) as usize;
    crate::sched::spawn_on_with_arg(hart, name, spawned_entry, arg, priority)
}

/// Spawn a child task running a **caller-supplied ELF buffer** (`image`) with
/// `delegated` caps — the `SpawnImage` counterpart to [`spawn_process_with_caps`].
/// The owned buffer rides in the [`SpawnRequest`] and is freed once the child
/// maps it (see [`run_image_with_caps`]).
pub fn spawn_image_with_caps(
    hart: usize,
    name: &str,
    image: alloc::boxed::Box<[u8]>,
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
    priority: kernel_proc::sched::Priority,
) -> kernel_proc::sched::TaskId {
    let req = alloc::boxed::Box::new(SpawnRequest { image: ProgramImage::Owned(image), delegated });
    let arg = alloc::boxed::Box::into_raw(req) as usize;
    crate::sched::spawn_on_with_arg(hart, name, spawned_entry, arg, priority)
}

/// Programs a `Spawn` syscall can launch, selected by id (v0.11 Phase 1a:
/// embedded, indexed). The shell's command set will live here; seeded with
/// `hello` until those programs land. Phase 1b swaps the id for an executable
/// File cap read from the FS.
static SPAWNABLE: &[(&str, &[u8])] = &[
    ("spawnee", SPAWNEE_ELF),         // 0
    ("memhog", MEMHOG_ELF),           // 1
    ("notify-signaller", NOTIFY_SIGNALLER_ELF), // 2
    ("spinner", SPINNER_ELF),         // 3
    ("fs-server", FS_SERVER_ELF),     // 4
    ("fs-client", FS_CLIENT_ELF),     // 5
    ("viewer", VIEWER_ELF),           // 6
    ("cap-reporter", CAP_REPORTER_ELF), // 7
    ("ipc-echo-server", IPC_ECHO_SERVER_ELF), // 8
    ("ipc-echo-client", IPC_ECHO_CLIENT_ELF), // 9
    ("svc-worker", SVC_WORKER_ELF),   // 10
    ("hart-spinner", HART_SPINNER_ELF), // 11
    ("hung-service", HUNG_SERVICE_ELF), // 12
];

/// Resolve a `Spawn` program id to its `(name, image)`, or `None` if out of range.
#[must_use]
pub fn spawnable_program(id: usize) -> Option<(&'static str, &'static [u8])> {
    SPAWNABLE.get(id).copied()
}

/// One program a userspace workload spawns: task name, spec, and priority.
pub struct ProgramSpawn {
    pub name: &'static str,
    pub program: &'static ProgramSpec,
    pub priority: kernel_proc::sched::Priority,
}

/// A userspace workload's spawn layout: whether it needs the shared IPC
/// endpoint created first, and the programs to spawn (in order — servers
/// before clients).
pub struct UserLayout {
    pub needs_endpoint: bool,
    pub programs: &'static [ProgramSpawn],
}

/// The spawn layout for a *userspace* workload, or `None` for kernel-mode /
/// storm / default selections (which `kmain` dispatches itself). A lookup into
/// [`LAYOUTS`].
pub fn user_layout(kind: WorkloadKind) -> Option<&'static UserLayout> {
    LAYOUTS.iter().find(|(k, _)| *k == kind).map(|(_, layout)| layout)
}

/// The userspace workload → spawn-layout table: the single place each userspace
/// workload's program set + endpoint need is declared. `kmain` loops over the
/// selected entry rather than carrying a per-workload spawn arm. In a `static`
/// so the nested program slices and `&SPEC` references live for the whole kernel.
/// Spawn order within a workload is significant — servers/receivers first.
static LAYOUTS: &[(WorkloadKind, UserLayout)] = &[
    (WorkloadKind::Userspace, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "user_main", program: &HELLO, priority: Priority::Normal }],
    }),
    (WorkloadKind::UserspaceBadPtr, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "bad_ptr", program: &BAD_PTR, priority: Priority::Normal }],
    }),
    (WorkloadKind::UserspaceFault, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "user_fault", program: &FAULTER, priority: Priority::Normal }],
    }),
    (WorkloadKind::UserspaceSpanFlood, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "user_span_flood", program: &SPAN_FLOOD, priority: Priority::Normal }],
    }),
    (WorkloadKind::Workers, UserLayout {
        needs_endpoint: false,
        programs: &[
            ProgramSpawn { name: "worker_a", program: &WORKER_A, priority: Priority::Normal },
            ProgramSpawn { name: "worker_b", program: &WORKER_B, priority: Priority::Normal },
        ],
    }),
    (WorkloadKind::HeapGrow, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "heap_grow", program: &HEAP_GROW, priority: Priority::Normal }],
    }),
    // Hog spawned first (runs first, never yields); the cooperative peer starves
    // until timer preemption takes the CPU back.
    (WorkloadKind::UserHog, UserLayout {
        needs_endpoint: false,
        programs: &[
            ProgramSpawn { name: "user_hog", program: &USER_HOG, priority: Priority::Normal },
            ProgramSpawn { name: "worker_a", program: &WORKER_A, priority: Priority::Normal },
        ],
    }),
    (WorkloadKind::SyscallHog, UserLayout {
        needs_endpoint: false,
        programs: &[
            ProgramSpawn { name: "syscall_hog", program: &SYSCALL_HOG, priority: Priority::Normal },
            ProgramSpawn { name: "worker_a", program: &WORKER_A, priority: Priority::Normal },
        ],
    }),
    // v0.11 Tier-0 console input: a single echo program reading typed UART input.
    (WorkloadKind::ConsoleEcho, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "console_echo", program: &CONSOLE_ECHO, priority: Priority::Normal }],
    }),
    // The Stitch interpreter as a userspace REPL — first on-target run of the
    // ported no_std tree-walker.
    (WorkloadKind::StitchRepl, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "stitch_repl", program: &STITCH_REPL, priority: Priority::Normal }],
    }),
    // Stitch REPL with a filesystem: the seeded FS server plus the REPL holding
    // the FS endpoint cap (`SEND`), so `:load <name>` reads a baked-in `.st`
    // file off the ramfs and runs it — telemetry crosses the wire as usual.
    (WorkloadKind::StitchFs, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "stitch_repl", program: &STITCH_REPL_IPC, priority: Priority::Normal },
        ],
    }),
    // SpawnImage demo: the seeded FS server (holding `/bin/spawnee`) plus a client
    // that reads that ELF off the filesystem and spawns it from the buffer.
    (WorkloadKind::SpawnImage, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "spawn_image_demo", program: &SPAWN_IMAGE_DEMO, priority: Priority::Normal },
        ],
    }),
    // Powerbox viewer demo: seeded FS server + view-demo launcher. view-demo
    // connects to the FS, looks up a file with READ-only rights, then spawns
    // the viewer (SPAWNABLE id 6) with that attenuated cap delegated.
    (WorkloadKind::ViewDemo, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "view_demo", program: &VIEW_DEMO, priority: Priority::Normal },
        ],
    }),
    // Interactive powerbox shell: seeded FS + shell. The shell reads `view
    // <path>` commands from the UART, looks up the file with READ-only rights,
    // spawns the viewer (SPAWNABLE id 6) with that cap, then revokes on exit.
    (WorkloadKind::Shell, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "shell", program: &SHELL, priority: Priority::Normal },
        ],
    }),
    // Generic satisfier: the seeded FS server (holding `/bin/fs-probe` + its
    // `user.iface` xattr) plus the `satisfier`, which reads the child's declared
    // `needs`, matches them via `hitch::satisfy`, and `SpawnImage`s the child with
    // the granted `fs` cap — data-driven delegation, named on the wire.
    (WorkloadKind::ManifestSatisfy, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "satisfier", program: &SATISFIER, priority: Priority::Normal },
        ],
    }),
    // Typed-interface end-to-end: the seeded FS server (holding
    // `/bin/manifest_demo` with a `user.iface` xattr) plus a client that reads it
    // over `GetXattr`, `decode_manifest`s it, and checks the shape.
    (WorkloadKind::ManifestIface, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER_SEEDED, priority: Priority::Normal },
            ProgramSpawn { name: "iface_reader", program: &IFACE_READER, priority: Priority::Normal },
        ],
    }),
    // Userspace-defined metrics: a single probe that names + emits its own metric.
    (WorkloadKind::Probe, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "probe", program: &PROBE, priority: Priority::Normal }],
    }),
    // v0.11 spawn-with-caps demo: the `spawner` parent boots and `Spawn`s the
    // `spawnee` child at runtime (delegating its span cap) — so only the parent
    // is in the layout; the child comes from the SPAWNABLE registry.
    (WorkloadKind::SpawnDemo, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "spawner", program: &SPAWNER, priority: Priority::Normal }],
    }),
    // v0.12 reclaim test: the `reaper` parent spawns + `Wait`s a `memhog` child
    // 30×; the child (SPAWNABLE id 1) is created at runtime, so only the parent
    // is in the layout. Proves Exit reclaims the child's address space (no OOM).
    (WorkloadKind::SpawnReap, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "reaper", program: &REAPER, priority: Priority::Normal }],
    }),
    // v0.13 wait-for-any: the `supervisor` parent spawns a `spinner` + `spawnee`
    // at runtime (SPAWNABLE ids), then `WaitAny`s — so only the parent is here.
    (WorkloadKind::WaitAny, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "supervisor", program: &SUPERVISOR, priority: Priority::Normal }],
    }),
    // v0.13 the supervising root: `init` spawns + `WaitAny`-reaps a child. Only
    // `init` is in the layout; the child comes from the SPAWNABLE registry.
    (WorkloadKind::Init, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "init", program: &INIT, priority: Priority::Normal }],
    }),
    // Supervision step 2: the generic supervisor root. Only `supervised` is in the
    // layout; it spawns its service table (spinner/spawnee) at runtime from the
    // SPAWNABLE registry and drives restarts via the pure `supervision` policy.
    (WorkloadKind::Supervised, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "supervised", program: &SUPERVISED, priority: Priority::Normal }],
    }),
    // Supervision FU2: the cap-survival supervisor. Only `supervised-ipc` is in the
    // layout; it manufactures its own endpoint and spawns its client/server (ids
    // 9/8) at runtime, respawning the crashing server so the client's cap survives.
    (WorkloadKind::SupervisedIpc, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "supervised_ipc", program: &SUPERVISED_IPC, priority: Priority::Normal }],
    }),
    // Supervision v2a step 4: the graceful-shutdown supervisor. Only the root is in
    // the layout; it spawns its service tree (svc-worker ×2 + spinner) and their
    // shutdown notifications at runtime, then tears them down in reverse-dep order.
    (WorkloadKind::SupervisedShutdown, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "supervised_shutdown", program: &SUPERVISED_SHUTDOWN, priority: Priority::Normal }],
    }),
    // Supervision v2a negative: a lone process tries to `Kill` without a Process cap
    // and is refused — no children, holds only its bootstrap caps.
    (WorkloadKind::KillNoCap, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "kill_no_cap", program: &KILL_NO_CAP, priority: Priority::Normal }],
    }),
    // Multi-hart userspace de-risk (v2b step 1): the launcher places this one on hart
    // 0 (see `kmain`); it opens a span proving U-mode runs on the boot hart.
    (WorkloadKind::UserOnHart0, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "hart_probe", program: &HART_PROBE, priority: Priority::Normal }],
    }),
    // Cross-hart Kill (v2b step 4): the killer runs on hart 1 (normal placement) and
    // `SpawnOn`s its victim to hart 0 at runtime, so only the killer is in the layout.
    (WorkloadKind::XhartKill, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "xhart_killer", program: &XHART_KILLER, priority: Priority::Normal }],
    }),
    // Hung detection (v2b): the supervisor spawns its `hung-service` child at runtime
    // and `wait_timeout`s a liveness notification, so only the supervisor is here.
    (WorkloadKind::HungDetect, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "hung_supervisor", program: &HUNG_SUPERVISOR, priority: Priority::Normal }],
    }),
    // v0.13 EndpointCreate: a single program manufactures its own endpoint and
    // proves it by minting — no kernel-created endpoint (`needs_endpoint: false`).
    (WorkloadKind::EndpointCreate, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "ep_maker", program: &EP_MAKER, priority: Priority::Normal }],
    }),
    // v0.12 notification smoke: the `notify-waiter` parent boots and `Spawn`s the
    // `notify-signaller` child (SPAWNABLE id 2) at runtime, delegating the
    // notification cap — so only the parent is in the layout.
    (WorkloadKind::NotifySmoke, UserLayout {
        needs_endpoint: false,
        programs: &[ProgramSpawn { name: "notify_waiter", program: &NOTIFY_WAITER, priority: Priority::Normal }],
    }),
    // v0.8b priority demo: a High CPU-bound `greedy` (the hog) vs a Low
    // cooperative worker — priority respected, aging keeps Low fed.
    (WorkloadKind::Priorities, UserLayout {
        needs_endpoint: false,
        programs: &[
            ProgramSpawn { name: "greedy", program: &USER_HOG, priority: Priority::High },
            ProgramSpawn { name: "worker_b", program: &WORKER_B, priority: Priority::Low },
        ],
    }),
    // IPC-family: server/receiver first so it's waiting when the peer sends.
    (WorkloadKind::Ipc, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "ipc_receiver", program: &IPC_RECEIVER, priority: Priority::Normal },
            ProgramSpawn { name: "ipc_sender", program: &IPC_SENDER, priority: Priority::Normal },
        ],
    }),
    (WorkloadKind::IpcRpc, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "rpc_server", program: &RPC_SERVER, priority: Priority::Normal },
            ProgramSpawn { name: "rpc_client", program: &RPC_CLIENT, priority: Priority::Normal },
        ],
    }),
    (WorkloadKind::BadgeMint, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "badge_minter", program: &BADGE_MINTER, priority: Priority::Normal },
            ProgramSpawn { name: "badge_client", program: &BADGE_MINT_CLIENT, priority: Priority::Normal },
        ],
    }),
    // Two clients over the *one* endpoint — each gets a distinct badge.
    (WorkloadKind::BadgeHandout, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "badge_handout_server", program: &BADGE_HANDOUT_SERVER, priority: Priority::Normal },
            ProgramSpawn { name: "badge_handout_client_a", program: &BADGE_HANDOUT_CLIENT, priority: Priority::Normal },
            ProgramSpawn { name: "badge_handout_client_b", program: &BADGE_HANDOUT_CLIENT, priority: Priority::Normal },
        ],
    }),
    (WorkloadKind::Fs, UserLayout {
        needs_endpoint: true,
        programs: &[
            ProgramSpawn { name: "fs_server", program: &FS_SERVER, priority: Priority::Normal },
            ProgramSpawn { name: "fs_client", program: &FS_CLIENT, priority: Priority::Normal },
        ],
    }),
];

/// Build a fresh address space, grant the process its bootstrap
/// capability, load `image` into it, and drop to U-mode. Never returns —
/// the hart runs userspace from here.
fn run(image: &'static [u8]) -> ! {
    run_with_caps(image, alloc::vec::Vec::new())
}

/// Like [`run`] but also grants the child each capability in `delegated` — a
/// `Spawn`'s parent-delegated caps — inserted after the bootstrap telemetry/span
/// pair (Q-A: a child is always born observable; the delegated caps occupy
/// handles `2..` in order). Never returns.
/// Build a user process with bootstrap + `delegated` caps, then load its image
/// via `load_image` and enter it. The image source is pluggable: an embedded
/// `&'static` slice ([`run_with_caps`]) or a caller-supplied owned buffer that
/// `load_image` frees once mapped ([`run_image_with_caps`]). Never returns.
fn run_loaded_with_caps(
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
    load_image: impl FnOnce(usize) -> Result<Loaded, LoadError>,
) -> ! {
    // Each process gets its own root page table (kernel high-half shared in).
    let root_pa = mmu::new_user_root().expect("userspace: no frame for user root page table");

    // Grant the bootstrap capabilities: a bare `TelemetrySink` (authority to
    // register + emit named metrics) and a `SpanSink`. The kernel snitches each
    // grant.
    let (process, bootstrap_handle, span_handle) = Process::bootstrap(root_pa);
    // The kernel snitches each grant two ways: the `cap.grants_total` counter
    // (a rate) and a rich `CapEvent::Granted` (an attributed fact carrying the
    // global cap id, holder, object, and rights — the derivation-tree seed).
    // Both bootstrap caps carry `EMIT`; they differ only in object kind.
    let holder = crate::sched::current_task_id().0;
    // Snitch each bootstrap grant with the *stored* holding cap id (set by
    // `Process::bootstrap`), so a later delegation can name it as `parent_cap_id`.
    for (handle, object) in [
        (bootstrap_handle, protocol::CapObject::TelemetrySink),
        (span_handle, protocol::CapObject::SpanSink),
    ] {
        if let Some(id) = cap_grants_metric_id() {
            tracing::emit_metric(id, 1);
        }
        let cap_id = process.caps.lock().cap_id_of(handle).unwrap_or(0);
        tracing::emit_cap_granted(
            cap_id,
            holder,
            object,
            kernel_proc::cap::Rights::EMIT.bits(),
            [0; snitchos_abi::CAP_NAME_LEN], // bootstrap telemetry/span carry no name
        );
    }

    // Grant the parent-delegated caps (a `Spawn`'s payload) on top of bootstrap.
    // They land at handles 2.. in order; each is snitched as a `Transferred`
    // linking to the parent holding it derived from (the derivation edge).
    for (cap, parent_cap_id) in &delegated {
        let cap_id = crate::process::next_cap_id();
        let _ = process.caps.lock().insert_with_id(*cap, cap_id, *parent_cap_id);
        if let Some(id) = cap_grants_metric_id() {
            tracing::emit_metric(id, 1);
        }
        let (badge, name) = match cap.object {
            kernel_proc::cap::Object::Endpoint { id, badge } => (badge, crate::ipc::name_of(id)),
            _ => (0, [0; snitchos_abi::CAP_NAME_LEN]),
        };
        tracing::emit_cap_transferred(
            cap_id,
            *parent_cap_id,
            holder,
            cap_object_kind(cap.object),
            cap.rights.bits(),
            badge,
            name,
        );
    }

    // Publish the process so the syscall trap handler can reach its
    // CapTable. `process` lives in this frame, which never returns (`enter`
    // is `-> !`), so the pointer stays valid for every trap from U-mode.
    let process_ptr = core::ptr::addr_of!(process).cast_mut();
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(process_ptr, core::sync::atomic::Ordering::Relaxed);

    // Associate this task with its address space so that when the scheduler
    // later switches *back* into it (after another userspace task ran), it
    // reloads `satp` + `CURRENT_PROCESS`. Without this, a second userspace
    // process would resume under the previous process's page table.
    crate::sched::set_current_address_space(process.root_pa, process_ptr);

    // Hand the process its bootstrap capability *by value*: the kernel sets
    // `a0` to the granted handle at entry, so the program receives its caps
    // instead of assuming a well-known handle. Neither side hardcodes a slot.
    match load_image(process.root_pa) {
        Ok(loaded) => enter(
            loaded,
            root_pa,
            bootstrap_handle.raw() as usize,
            span_handle.raw() as usize,
            0, // no endpoint cap for the non-IPC programs
        ),
        Err(e) => panic!("userspace load failed: {e:?}"),
    }
}

/// Run an embedded program image (the `Spawn` path) with `delegated` caps.
fn run_with_caps(
    image: &'static [u8],
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
) -> ! {
    run_loaded_with_caps(delegated, |root_pa| load(root_pa, image))
}

/// Run a caller-supplied ELF buffer (the `SpawnImage` path) with `delegated`
/// caps. The owned `image` is dropped the moment it's mapped — before entering
/// U-mode — so a per-spawn ELF copy isn't leaked for the process's lifetime.
fn run_image_with_caps(
    image: alloc::boxed::Box<[u8]>,
    delegated: alloc::vec::Vec<(kernel_proc::cap::Capability, u64)>,
) -> ! {
    run_loaded_with_caps(delegated, move |root_pa| {
        let loaded = load(root_pa, &image);
        drop(image);
        loaded
    })
}

/// Like [`run`], but additionally grants the process an [`Endpoint`] capability
/// over `endpoint` with `rights` (`SEND`/`RECV`/`MINT`) — the kernel-brokered
/// IPC cap, handle delivered in the third startup register (`a2`). Its bootstrap
/// `TelemetrySink` is bare authority, same as every program: any metric it wants
/// it registers at runtime (debt #2). Never returns.
///
/// [`Endpoint`]: kernel_proc::cap::Object::Endpoint
fn run_ipc(
    image: &'static [u8],
    endpoint: kernel_proc::ipc::EndpointId,
    rights: kernel_proc::cap::Rights,
) -> ! {
    use kernel_proc::cap::{Capability, Object};

    let root_pa = mmu::new_user_root().expect("ipc: no frame for user root page table");
    let (process, bootstrap_handle, span_handle) = Process::bootstrap(root_pa);

    // Grant the IPC endpoint capability on top of the bootstrap pair, stamped
    // with its own global cap id — a kernel-minted root grant (the ur-source of
    // this endpoint's derivation tree).
    let endpoint_handle = process.caps.lock().insert_with_id(
        Capability { object: Object::Endpoint { id: endpoint, badge: 0 }, rights },
        crate::process::next_cap_id(),
        0, // kernel-minted root grant: the ur-source of this endpoint's tree
    );

    // Snitch every grant (counter + rich CapEvent) with its *stored* cap id, as
    // `run` does — now three: the two bootstrap authorities plus this endpoint.
    let holder = crate::sched::current_task_id().0;
    let grants = [
        (bootstrap_handle, protocol::CapObject::TelemetrySink, kernel_proc::cap::Rights::EMIT.bits()),
        (span_handle, protocol::CapObject::SpanSink, kernel_proc::cap::Rights::EMIT.bits()),
        (endpoint_handle, protocol::CapObject::Endpoint, rights.bits()),
    ];
    for (handle, object, rights_bits) in grants {
        if let Some(id) = cap_grants_metric_id() {
            tracing::emit_metric(id, 1);
        }
        // Capture cap_id + (for an endpoint) its id under one lock; resolve the
        // name after dropping caps, so we never nest the ENDPOINTS lock inside it.
        let (cap_id, ep_id) = {
            let caps = process.caps.lock();
            let cap_id = caps.cap_id_of(handle).unwrap_or(0);
            let ep_id = match caps.resolve(handle) {
                Ok(kernel_proc::cap::Capability {
                    object: kernel_proc::cap::Object::Endpoint { id, .. },
                    ..
                }) => Some(*id),
                _ => None,
            };
            (cap_id, ep_id)
        };
        let name = ep_id.map_or([0; snitchos_abi::CAP_NAME_LEN], crate::ipc::name_of);
        tracing::emit_cap_granted(cap_id, holder, object, rights_bits, name);
    }

    let process_ptr = core::ptr::addr_of!(process).cast_mut();
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(process_ptr, core::sync::atomic::Ordering::Relaxed);
    crate::sched::set_current_address_space(process.root_pa, process_ptr);

    match load(process.root_pa, image) {
        Ok(loaded) => enter(
            loaded,
            root_pa,
            bootstrap_handle.raw() as usize,
            span_handle.raw() as usize,
            endpoint_handle.raw() as usize,
        ),
        Err(e) => panic!("ipc userspace load failed: {e:?}"),
    }
}

/// Translate ELF segment R/W/X flags into page-table perms, always with the
/// `U` bit so U-mode may access the page.
fn perms_for(p: SegmentPerms) -> PtePerms {
    let mut perms = PtePerms::U;
    if p.read {
        perms = perms.union(PtePerms::R);
    }
    if p.write {
        perms = perms.union(PtePerms::W);
    }
    if p.exec {
        perms = perms.union(PtePerms::X);
    }
    perms
}

/// Parse `image` and map its `PT_LOAD` segments into the page table rooted
/// at `root_pa`. The page planning — unioning the perms of segments that share
/// a page, enforcing W^X on the union, and splitting file bytes into per-page
/// copy windows — is pure and host-tested in [`kernel_proc::elf`]; this
/// function is the effects: allocate, map, copy. Returns the entry point.
pub fn load(root_pa: usize, image: &[u8]) -> Result<Loaded, LoadError> {
    let plan = elf::parse(image).map_err(LoadError::Parse)?;
    let perms_by_page = elf::page_perms(&plan, FRAME_SIZE).map_err(LoadError::Plan)?;

    // Allocate a zeroed frame per page and map it; remember its linear-map VA
    // so the copy pass can reach it.
    let mut dst_by_page: BTreeMap<usize, usize> = BTreeMap::new();
    for (&page_va, &perms) in &perms_by_page {
        let f = frame::alloc_zeroed().ok_or(LoadError::OutOfFrames)?;
        mmu::map_in(root_pa, page_va, f.addr(), perms_for(perms)).map_err(LoadError::Map)?;
        dst_by_page.insert(page_va, f.kernel_va());
    }

    // Copy each segment's file bytes into the mapped frames. The bss tail
    // yields no windows, so the zeroed frames stay zeroed.
    for seg in &plan.segments {
        for w in elf::copy_windows(seg, FRAME_SIZE) {
            let dst = dst_by_page[&w.page_va] + w.page_off;
            // SAFETY: `dst` is a fresh frame's linear-map VA (writable, covers
            // all RAM); a window never spans past its page; `src_off + len` is
            // in-bounds of `image` (the parser validated the segment file
            // range, and windows only narrow it).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image.as_ptr().add(w.src_off),
                    dst as *mut u8,
                    w.len,
                );
            }
        }
    }

    Ok(Loaded { entry: plan.entry })
}

// sstatus field masks for the enter sequence.
const SPP: usize = 1 << 8; // Previous Privilege: clear -> return to U
const SPIE: usize = 1 << 5; // Previous Interrupt Enable: set -> SIE=1 after sret
const SUM: usize = 1 << 18; // Supervisor User Memory access: clear -> S can't touch U pages
const FS: usize = 0b11 << 13; // FP state: clear -> Off (kernel + program are integer-only)
const SIE: usize = 1 << 1; // Interrupt Enable (live): clear before arming sscratch

/// Copy a byte buffer from user memory into `dst`, returning the copied slice,
/// or `None` if `(ptr, len)` is not a valid user range (or doesn't fit `dst`).
///
/// The user pages are mapped under the process's `satp` (the trap kept it), but
/// `SUM` is cleared while in U-mode, so a bare kernel deref of a user pointer
/// would fault. We validate the range with [`crate::mmu::user_range_readable`]
/// — both that it's wholly in the user half *and* that every page is mapped
/// `R|U` — then briefly set `sstatus.SUM` to permit the read, copy into the
/// kernel buffer, and clear it again. The copy must complete before `SUM`
/// drops: the caller dereferences `dst`, never the user pointer.
///
/// An in-range but **unmapped** pointer is now refused (`None`) rather than
/// faulting the kernel — the page-table walk catches it before the `SUM` deref.
pub fn copy_from_user(ptr: usize, len: usize, dst: &mut [u8]) -> Option<&[u8]> {
    if len > dst.len() || !crate::mmu::user_range_readable(ptr, len) {
        return None;
    }
    // SAFETY: every page in the range was just validated mapped `R|U` in the
    // active address space and the length fits `dst`. `SUM` is set only across
    // the copy and cleared immediately after.
    unsafe {
        core::arch::asm!("csrs sstatus, {}", in(reg) SUM);
        core::ptr::copy_nonoverlapping(ptr as *const u8, dst.as_mut_ptr(), len);
        core::arch::asm!("csrc sstatus, {}", in(reg) SUM);
    }
    Some(&dst[..len])
}

/// Copy `src` from kernel memory into the user buffer at `ptr` in the **current**
/// address space, returning the number of bytes written, or `None` if
/// `(ptr, src.len())` is not a valid *writable* user range.
///
/// The write mirror of [`copy_from_user`]: validate the range is wholly user-half
/// and every page mapped `W|U` (via [`crate::mmu::user_range_writable`]), briefly
/// set `sstatus.SUM` to permit the kernel write, copy, then clear `SUM` again.
/// Used by the `ConsoleRead` syscall to deliver buffered input into the caller's
/// buffer. The copy must complete before `SUM` drops.
pub fn copy_to_user(ptr: usize, src: &[u8]) -> Option<usize> {
    if !crate::mmu::user_range_writable(ptr, src.len()) {
        return None;
    }
    // SAFETY: every page in the range was just validated mapped `W|U` in the
    // active address space. `SUM` is set only across the copy and cleared
    // immediately after; the kernel writes into the user pointer, never derefs it
    // for reads. The source is a kernel slice, valid for `src.len()` bytes.
    unsafe {
        core::arch::asm!("csrs sstatus, {}", in(reg) SUM);
        core::ptr::copy_nonoverlapping(src.as_ptr(), ptr as *mut u8, src.len());
        core::arch::asm!("csrc sstatus, {}", in(reg) SUM);
    }
    Some(src.len())
}

/// Switch to the process's address space (`root_pa`) and drop to U-mode at
/// `loaded.entry`, with `a0`/`a1` set to `startup_a0`/`startup_a1` — the two
/// startup capability handles the program receives (its `crt0` passes them
/// into `__snitchos_start`, which publishes them for the runtime's `telemetry`
/// / `tracer` accessors before calling `main`). Never returns.
///
/// `satp` is switched first: the kernel high-half is shared into `root_pa`,
/// so this function's own code/stack (and the trap path it's about to enter)
/// stay mapped across the switch. Order is then load-bearing: clear `SIE`
/// (mask interrupts) *before* arming `sscratch`, so a stray timer IRQ can't
/// see a nonzero `sscratch` in S-mode and mis-take the from-user path in
/// `trap_entry`. `sret` then drops to U *and* restores `SIE` from `SPIE`.
pub fn enter(
    loaded: Loaded,
    root_pa: usize,
    startup_a0: usize,
    startup_a1: usize,
    startup_a2: usize,
) -> ! {
    let satp = mmu::satp_for(root_pa);
    // SAFETY: switches the active address space to the user root (kernel
    // high-half shared, so we keep executing), then forges a trap-return into
    // U-mode. `sscratch` is armed with this hart's kernel sp so the eventual
    // ecall trap switches onto it; sstatus is set for U-mode entry with
    // interrupts on, SUM off, FP off. `a0` carries the startup handle into the
    // program (the SysV first-arg register; sret leaves it untouched).
    unsafe {
        core::arch::asm!(
        "csrw satp, {satp}",
        "sfence.vma",
        "csrc sstatus, {clear}",
        "csrs sstatus, {set}",
        "csrw sscratch, sp",
        "csrw sepc, {entry}",
        "sret",
        satp = in(reg) satp,
        clear = in(reg) (SPP | SUM | FS | SIE),
        set = in(reg) (SPIE),
        entry = in(reg) loaded.entry,
        in("a0") startup_a0,
        in("a1") startup_a1,
        in("a2") startup_a2,
        options(noreturn));
    }
}
