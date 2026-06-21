//! Userspace program embedding and loading (v0.7a+).
//!
//! Each userspace program (`user/hello`'s binaries + `user/fs`'s) is baked into
//! the kernel image at build time — one `include_bytes!(env!(...))` static per
//! `USER_PROGRAMS` row in `build.rs` — and described by a [`ProgramSpec`] in the
//! program registry below. [`spawn_program`] launches one via the single
//! generic [`program_entry`], which carries the spec through the task's `arg`
//! word; `kmain` selects which to spawn per workload.
//!
//! [`load`] parses an embedded ELF with [`kernel_core::elf`] and maps its
//! segments into a fresh per-process root page table (kernel high-half
//! shared in) with the `U` bit; [`enter`] switches `satp` and drops to
//! U-mode at the entry point.

use alloc::collections::BTreeMap;

use kernel_core::cap::Rights;
use kernel_core::elf::{self, LoadSegment, SegmentPerms};
use kernel_core::mmu::{MapError, PtePerms};
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
pub static FS_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_FS_CLIENT_ELF"));

/// The metric every userspace process emits to through its bootstrap
/// `TelemetrySink` (`snitchos.user.telemetry_total`): the `Invoke` syscall
/// resolves the sink to this `StringId` and emits `a1` to it. Registered once
/// on hart 0 (`init_metric`) so first use doesn't intern (and emit a
/// `StringRegister` frame) in trap context; bound into the cap at process
/// setup via [`user_metric_id`] (see `run` / `run_ipc`).
static USER_METRIC: Once<StringId> = Once::new();

/// The counter a U-mode page fault bumps — the isolation firewall doing its
/// job. Registered alongside [`USER_METRIC`]; read by the fault handler.
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

/// The gauge the **FS server** emits when its rights gate refuses an op — the
/// userspace filesystem snitching its own authority decision (the kernel only
/// carries the badge rights, never interprets them). A *gauge*, not a counter:
/// the value is the structured `fs_proto::Denial` (inode + attempted right)
/// packed into an `i64`, so "the last denial" is the meaningful reading, not a
/// rate. Bound into the FS server's bootstrap telemetry sink (its only one),
/// read here via [`fs_denied_metric_id`].
static FS_DENIED_METRIC: Once<StringId> = Once::new();

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
    /// The frame allocator is exhausted.
    OutOfFrames,
    /// Installing a page-table entry failed.
    Map(MapError),
}

/// Register the userspace counters. Call once at boot, before entering
/// U-mode, so the syscall/fault handlers can emit without interning in trap
/// context.
pub fn init_metric() {
    USER_METRIC.call_once(|| tracing::register_counter("snitchos.user.telemetry_total"));
    USER_FAULT_METRIC.call_once(|| tracing::register_counter("snitchos.user.faults_total"));
    CAP_GRANTS_METRIC.call_once(|| tracing::register_counter("snitchos.cap.grants_total"));
    CAP_DENIED_METRIC.call_once(|| tracing::register_counter("snitchos.cap.denied_total"));
    USER_EXITS_METRIC.call_once(|| tracing::register_counter("snitchos.user.exits_total"));
    FS_DENIED_METRIC.call_once(|| tracing::register_gauge("snitchos.fs.denied"));
}

/// The `StringId` for the userspace telemetry counter (or `None` pre-init).
pub fn user_metric_id() -> Option<StringId> {
    USER_METRIC.get().copied()
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

/// The `StringId` for the FS rights-gate denial gauge (or `None` pre-init).
pub fn fs_denied_metric_id() -> Option<StringId> {
    FS_DENIED_METRIC.get().copied()
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
    /// `rights_bits`, its bootstrap telemetry sink bound to `counter`.
    Ipc { rights_bits: u32, counter: CounterKind },
}

/// Which pre-registered counter a program's bootstrap telemetry sink binds to.
enum CounterKind {
    /// The shared `snitchos.user.telemetry_total`.
    User,
    /// The FS rights-gate denial gauge `snitchos.fs.denied` (the FS server's
    /// sole telemetry).
    FsDenied,
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

/// An IPC program on the shared `DEMO_ENDPOINT` with `rights_bits` and default
/// user telemetry — the common case (the FS server is the lone exception, with
/// its own counter).
const fn ipc_user(elf: &'static [u8], rights_bits: u32) -> ProgramSpec {
    ProgramSpec { elf, launch: Launch::Ipc { rights_bits, counter: CounterKind::User } }
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

/// `workload=fs`: the FS server (`RECV | MINT`), telemetry bound to the denial
/// gauge — its only telemetry (see [`fs_denied_metric_id`]).
pub static FS_SERVER: ProgramSpec = ProgramSpec {
    elf: FS_SERVER_ELF,
    launch: Launch::Ipc {
        rights_bits: Rights::RECV.bits() | Rights::MINT.bits(),
        counter: CounterKind::FsDenied,
    },
};

/// `workload=fs`: the FS client (`SEND`), default user telemetry.
pub static FS_CLIENT: ProgramSpec = ipc_user(FS_CLIENT_ELF, Rights::SEND.bits());

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
        Launch::Ipc { rights_bits, counter } => {
            let ep = *crate::ipc::DEMO_ENDPOINT
                .get()
                .expect("ipc endpoint created before an IPC program runs");
            let rights = Rights::from_bits(*rights_bits);
            let counter = match counter {
                CounterKind::User => user_metric_id(),
                CounterKind::FsDenied => fs_denied_metric_id(),
            }
            .expect("program telemetry counter registered before entry");
            run_ipc_counter(spec.elf, ep, rights, counter)
        }
    }
}

/// Spawn `program` on `hart` as task `name`, stashing its [`ProgramSpec`]
/// address in the task's `arg` word for [`program_entry`]. The userspace
/// counterpart to [`crate::sched::spawn_on`].
pub fn spawn_program(hart: usize, name: &str, program: &'static ProgramSpec) -> kernel_core::sched::TaskId {
    spawn_program_with_priority(hart, name, program, kernel_core::sched::Priority::Normal)
}

/// Like [`spawn_program`] but at an explicit scheduling priority.
pub fn spawn_program_with_priority(
    hart: usize,
    name: &str,
    program: &'static ProgramSpec,
    priority: kernel_core::sched::Priority,
) -> kernel_core::sched::TaskId {
    crate::sched::spawn_on_with_arg(hart, name, program_entry, core::ptr::from_ref(program) as usize, priority)
}

/// Build a fresh address space, grant the process its bootstrap
/// capability, load `image` into it, and drop to U-mode. Never returns —
/// the hart runs userspace from here.
fn run(image: &'static [u8]) -> ! {
    // Each process gets its own root page table (kernel high-half shared in).
    let root_pa = mmu::new_user_root().expect("userspace: no frame for user root page table");

    // Grant the bootstrap capability: one `TelemetrySink` bound to the
    // userspace telemetry counter. The cap *names* the sink, so the syscall
    // (Step 5) needs no string from U-mode. The kernel snitches the grant.
    let counter = user_metric_id().expect("userspace telemetry counter registered before entry");
    let (process, bootstrap_handle, span_handle) = Process::bootstrap(root_pa, counter);
    // The kernel snitches each grant two ways: the `cap.grants_total` counter
    // (a rate) and a rich `CapEvent::Granted` (an attributed fact carrying the
    // global cap id, holder, object, and rights — the derivation-tree seed).
    // Both bootstrap caps carry `EMIT`; they differ only in object kind.
    let holder = crate::sched::current_task_id().0;
    for object in [protocol::CapObject::TelemetrySink, protocol::CapObject::SpanSink] {
        if let Some(id) = cap_grants_metric_id() {
            tracing::emit_metric(id, 1);
        }
        tracing::emit_cap_granted(
            crate::process::next_cap_id(),
            holder,
            object,
            kernel_core::cap::Rights::EMIT.bits(),
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
    match load(process.root_pa, image) {
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

/// Like [`run`], but additionally grants the process an [`Endpoint`] capability
/// over `endpoint` with `rights` (`SEND`/`RECV`/`MINT`) — the kernel-brokered
/// IPC cap, handle delivered in the third startup register (`a2`) — and binds
/// the bootstrap telemetry sink to `counter` (the default user counter for most
/// programs; the FS server points its sole sink at `snitchos.fs.denied`). Never
/// returns.
///
/// [`Endpoint`]: kernel_core::cap::Object::Endpoint
fn run_ipc_counter(
    image: &'static [u8],
    endpoint: kernel_core::ipc::EndpointId,
    rights: kernel_core::cap::Rights,
    counter: StringId,
) -> ! {
    use kernel_core::cap::{Capability, Object};

    let root_pa = mmu::new_user_root().expect("ipc: no frame for user root page table");
    let (process, bootstrap_handle, span_handle) = Process::bootstrap(root_pa, counter);

    // Grant the IPC endpoint capability on top of the bootstrap pair.
    let endpoint_handle =
        process.caps.lock().insert(Capability { object: Object::Endpoint { id: endpoint, badge: 0 }, rights });

    // Snitch every grant (counter + rich CapEvent), as `run` does — now three:
    // the two bootstrap authorities plus this endpoint cap.
    let holder = crate::sched::current_task_id().0;
    let grants = [
        (protocol::CapObject::TelemetrySink, kernel_core::cap::Rights::EMIT.bits()),
        (protocol::CapObject::SpanSink, kernel_core::cap::Rights::EMIT.bits()),
        (protocol::CapObject::Endpoint, rights.bits()),
    ];
    for (object, rights_bits) in grants {
        if let Some(id) = cap_grants_metric_id() {
            tracing::emit_metric(id, 1);
        }
        tracing::emit_cap_granted(crate::process::next_cap_id(), holder, object, rights_bits);
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

/// The page-aligned VAs a segment occupies in memory.
fn pages_of(seg: &LoadSegment) -> impl Iterator<Item = usize> {
    let start = seg.vaddr & !(FRAME_SIZE - 1);
    let end = (seg.vaddr + seg.mem_size + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
    (start..end).step_by(FRAME_SIZE)
}

/// Parse `image` and map its `PT_LOAD` segments into the page table rooted
/// at `root_pa`. Two segments may share a page (e.g. R-X code + R rodata in
/// the first page), so perms are unioned per page and each page is mapped
/// once; file bytes are then copied in and the bss tail left zero. Returns
/// the entry point.
pub fn load(root_pa: usize, image: &[u8]) -> Result<Loaded, LoadError> {
    let plan = elf::parse(image).map_err(LoadError::Parse)?;

    // Union perms over every page each segment touches.
    let mut perms_by_page: BTreeMap<usize, PtePerms> = BTreeMap::new();
    for seg in &plan.segments {
        let perms = perms_for(seg.perms);
        for page_va in pages_of(seg) {
            perms_by_page
                .entry(page_va)
                .and_modify(|p| *p = p.union(perms))
                .or_insert(perms);
        }
    }

    // Allocate a zeroed frame per page and map it; remember its linear-map VA
    // so the copy pass can reach it.
    let mut dst_by_page: BTreeMap<usize, usize> = BTreeMap::new();
    for (&page_va, &perms) in &perms_by_page {
        let f = frame::alloc_zeroed().ok_or(LoadError::OutOfFrames)?;
        mmu::map_in(root_pa, page_va, f.addr(), perms).map_err(LoadError::Map)?;
        dst_by_page.insert(page_va, f.kernel_va());
    }

    // Copy each segment's file bytes into the mapped frames.
    for seg in &plan.segments {
        let file_lo = seg.vaddr;
        let file_hi = seg.vaddr + seg.file_size;
        for page_va in pages_of(seg) {
            let lo = file_lo.max(page_va);
            let hi = file_hi.min(page_va + FRAME_SIZE);
            if lo >= hi {
                continue;
            }
            let dst = dst_by_page[&page_va] + (lo - page_va);
            let src = seg.file_offset + (lo - file_lo);
            // SAFETY: `dst` is a fresh frame's linear-map VA (writable, covers
            // all RAM); the copy length is at most one page; `src` is in-bounds
            // of `image` (the parser validated the segment file range).
            unsafe {
                core::ptr::copy_nonoverlapping(image.as_ptr().add(src), dst as *mut u8, hi - lo);
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
/// The user pages are mapped — the trap kept the process's `satp` — but `SUM`
/// is cleared while in U-mode, so a bare kernel deref of a user pointer would
/// fault. We range-check with `user_range_ok`, then briefly set `sstatus.SUM`
/// to permit the read, copy into the kernel buffer, and clear it again. The
/// copy must complete before `SUM` drops: the caller dereferences `dst`, never
/// the user pointer. Fault-graceful copy (an in-range but unmapped pointer) is
/// a deferred refinement; today such a pointer faults to S-mode (kernel bug
/// panic) rather than refusing — userspace can only pass mapped names.
pub fn copy_from_user(ptr: usize, len: usize, dst: &mut [u8]) -> Option<&[u8]> {
    if !kernel_core::mmu::user_range_ok(ptr, len) || len > dst.len() {
        return None;
    }
    // SAFETY: range validated wholly within the user half and `<= dst.len()`;
    // the process page table is active so the bytes are mapped. `SUM` is set
    // only across the copy and cleared immediately after.
    unsafe {
        core::arch::asm!("csrs sstatus, {}", in(reg) SUM);
        core::ptr::copy_nonoverlapping(ptr as *const u8, dst.as_mut_ptr(), len);
        core::arch::asm!("csrc sstatus, {}", in(reg) SUM);
    }
    Some(&dst[..len])
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
