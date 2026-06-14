//! Userspace program embedding and loading (v0.7a).
//!
//! Two programs are baked into the kernel image at build time: `user/hello`
//! (the `workload=userspace` demo — emits one telemetry syscall) and
//! `faulter` (the `workload=userspace-fault` isolation probe — reads a
//! kernel VA, which must fault). `build.rs` resolves each path: the
//! freshly-built artifact when building via `cargo xtask build`, else the
//! committed fixture under `kernel-core/fixtures/`.
//!
//! [`load`] parses an embedded ELF with [`kernel_core::elf`] and maps its
//! segments into a fresh per-process root page table (kernel high-half
//! shared in) with the `U` bit; [`enter`] switches `satp` and drops to
//! U-mode at the entry point.

use alloc::collections::BTreeMap;

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

/// The `workload=ipc` programs: `ipc-sender` holds a `SEND` cap and sends one
/// inline message; `ipc-receiver` holds a `RECV` cap, receives it, and
/// re-emits the payload. They rendezvous over one kernel-brokered endpoint.
pub static IPC_SENDER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_SENDER_ELF"));
pub static IPC_RECEIVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_IPC_RECEIVER_ELF"));

/// The `workload=ipc-rpc` programs: `rpc-client` `call`s and `rpc-server`
/// `reply`s over the shared endpoint — the v0.9b RPC round-trip.
pub static RPC_CLIENT_ELF: &[u8] = include_bytes!(env!("SNITCHOS_RPC_CLIENT_ELF"));
pub static RPC_SERVER_ELF: &[u8] = include_bytes!(env!("SNITCHOS_RPC_SERVER_ELF"));

/// The counter the `EmitMetric` syscall bumps. Registered once on hart 0
/// (`init_metric`) so the `MetricRegister` frame isn't emitted from inside
/// the trap handler; the handler (on hart 1) reads it via [`user_metric_id`].
static USER_METRIC: Once<StringId> = Once::new();

/// The counter a U-mode page fault bumps — the isolation firewall doing its
/// job. Registered alongside [`USER_METRIC`]; read by the fault handler.
static USER_FAULT_METRIC: Once<StringId> = Once::new();

/// The counter the kernel bumps each time it **grants** a capability —
/// authority being created. v0.7b grants exactly once (the bootstrap
/// `TelemetrySink`), so this reaches 1; the richer `CapEvent` frame is the
/// sequenced follow-on. Registered alongside the others so the grant site
/// emits without interning.
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

/// The `StringId` for the denied-invocation counter (or `None` pre-init).
pub fn cap_denied_metric_id() -> Option<StringId> {
    CAP_DENIED_METRIC.get().copied()
}

/// Hart-1 entry for `workload=userspace`: run the `hello` program.
pub extern "C" fn user_main_entry() -> ! {
    run(HELLO_ELF)
}

/// Hart-1 entry for `workload=userspace-fault`: run the isolation probe.
pub extern "C" fn faulter_main_entry() -> ! {
    run(FAULTER_ELF)
}

/// Hart-1 entry for `workload=userspace-span-flood`: run the span-quota probe.
pub extern "C" fn span_flood_main_entry() -> ! {
    run(SPAN_FLOOD_ELF)
}

/// Hart-1 entry for `workload=workers`: run cooperative demo worker A.
pub extern "C" fn worker_a_main_entry() -> ! {
    run(WORKER_A_ELF)
}

/// Hart-1 entry for `workload=workers`: run cooperative demo worker B (the
/// twin process sharing the hart with worker A).
pub extern "C" fn worker_b_main_entry() -> ! {
    run(WORKER_B_ELF)
}

/// Hart-1 entry for `workload=heap-grow`: run the heap-growth probe.
pub extern "C" fn heap_grow_main_entry() -> ! {
    run(HEAP_GROW_ELF)
}

/// Hart-1 entry for `workload=user-hog`: run the uncooperative CPU hog.
pub extern "C" fn user_hog_main_entry() -> ! {
    run(USER_HOG_ELF)
}

/// Entry for `workload=ipc`: run the IPC demo sender, granted a `SEND` cap to
/// the shared kernel-brokered endpoint.
pub extern "C" fn ipc_sender_main_entry() -> ! {
    let ep = *crate::ipc::DEMO_ENDPOINT.get().expect("ipc endpoint created before sender runs");
    run_ipc(IPC_SENDER_ELF, ep, kernel_core::cap::Rights::SEND)
}

/// Entry for `workload=ipc`: run the IPC demo receiver, granted a `RECV` cap to
/// the shared kernel-brokered endpoint.
pub extern "C" fn ipc_receiver_main_entry() -> ! {
    let ep = *crate::ipc::DEMO_ENDPOINT.get().expect("ipc endpoint created before receiver runs");
    run_ipc(IPC_RECEIVER_ELF, ep, kernel_core::cap::Rights::RECV)
}

/// Entry for `workload=ipc-rpc`: run the RPC client, granted a `SEND` cap (it
/// `call`s through the endpoint; the kernel mints its reply cap into the server).
pub extern "C" fn rpc_client_main_entry() -> ! {
    let ep = *crate::ipc::DEMO_ENDPOINT.get().expect("ipc endpoint created before rpc client runs");
    run_ipc(RPC_CLIENT_ELF, ep, kernel_core::cap::Rights::SEND)
}

/// Entry for `workload=ipc-rpc`: run the RPC server, granted a `RECV` cap.
pub extern "C" fn rpc_server_main_entry() -> ! {
    let ep = *crate::ipc::DEMO_ENDPOINT.get().expect("ipc endpoint created before rpc server runs");
    run_ipc(RPC_SERVER_ELF, ep, kernel_core::cap::Rights::RECV)
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
/// over `endpoint` with `rights` (`SEND` or `RECV`) — the kernel-brokered IPC
/// cap — and delivers its handle as the third startup register (`a2`). Used by
/// the `workload=ipc` sender/receiver. Never returns.
///
/// [`Endpoint`]: kernel_core::cap::Object::Endpoint
fn run_ipc(image: &'static [u8], endpoint: kernel_core::ipc::EndpointId, rights: kernel_core::cap::Rights) -> ! {
    use kernel_core::cap::{Capability, Object};

    let root_pa = mmu::new_user_root().expect("ipc: no frame for user root page table");
    let counter = user_metric_id().expect("userspace telemetry counter registered before entry");
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
