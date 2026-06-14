//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs, `sepc`,
//! and `sstatus` into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.

core::arch::global_asm!(include_str!("trap.S"));

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel_core::clock::Clock;
use kernel_core::trap::{TrapCause, decode_scause};

use crate::percpu::PerCpu;

// ## Memory ordering note for the timer-IRQ statics below
//
// `TICK_PENDING` (set by ISR, read by main) and `LAST_IRQ_DURATION`
// (written by ISR, read by main after observing TICK_PENDING) form a
// classic publication pattern. Across harts that pattern needs
// `Release` on the store side and `Acquire` on the load side.
//
// Both are now `PerCpu<T>`: each hart's ISR touches only its own
// cell, and that hart's main/idle loop reads the same cell. Both
// ends are guaranteed same-hart by construction (the ISR runs on
// whichever hart's `stimecmp` expired; `this_cpu()` reads `tp`).
// Trap return synchronises the handler's memory ops with the
// resumed thread by hardware, so `Relaxed` is correct.
//
// Pre-PerCpu these were globals shared by both harts. Hart 0's ISR
// could clobber a tick that hart 1 had not yet polled (correctness
// for the heartbeat cadence on the secondary) and hart 0's
// heartbeat could observe hart 1's `LAST_IRQ_DURATION` (telemetry
// corruption). See `plans/deflake-bisection.md` follow-up (c).

/// How many ticks between timer interrupts. Set by `init_timer` from
/// the DTB timebase; both harts' IRQ handlers read it to arm the
/// next deadline. Init-once global shared config — the cadence is
/// the same on every hart, so there's no per-CPU state to track.
/// `Relaxed`: init-once, then read forever — no payload to publish.
pub static TIMER_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Set by the timer IRQ handler; the main/idle loop polls + clears.
/// One cell per hart — see block comment above.
/// `Relaxed`: same-CPU IRQ handoff — trap return sequences memory.
pub static TICK_PENDING: PerCpu<AtomicBool> =
    PerCpu::new([AtomicBool::new(false), AtomicBool::new(false)]);

/// Duration of the most recent timer IRQ in ticks. The IRQ handler
/// measures `rdtime` at entry and exit; the main thread reads this
/// after wake and emits a histogram observation. One cell per hart
/// so each hart's heartbeat reports its own IRQ cost. (We can't
/// emit telemetry from the IRQ itself — would deadlock on the
/// intern / virtio_console mutexes.)
/// `Relaxed`: same-CPU IRQ handoff — see block comment above.
pub static LAST_IRQ_DURATION: PerCpu<AtomicU64> =
    PerCpu::new([AtomicU64::new(0), AtomicU64::new(0)]);

/// SSTC-based clock: reads `time` CSR directly, writes `stimecmp`
/// (CSR 0x14d) to arm. No SBI round-trip. Implements
/// `kernel_core::clock::Clock`.
pub struct SstcClock;

impl Clock for SstcClock {
    fn now(&self) -> u64 {
        let t: u64;
        unsafe {
            asm!("rdtime {}", out(reg) t);
        }
        t
    }
    fn arm(&self, deadline: u64) {
        unsafe {
            asm!("csrw 0x14d, {}", in(reg) deadline);
        }
    }
}

/// The clock used by the IRQ handler and boot-time timer setup. A
/// single concrete instance lives here so the handler doesn't need to
/// take a `&dyn Clock` (no allocator, and the cost of dynamic dispatch
/// in an IRQ is silly when we only ever have one impl).
pub const CLOCK: SstcClock = SstcClock;

/// Saved register state at trap entry. The assembly stores into these
/// fields in this order; the Rust dispatcher reads them by name.
///
/// `#[repr(C)]` guarantees byte-for-byte agreement with the
/// hand-written offsets in `trap.S`. Reorder fields here and the asm
/// will be wrong — keep them in sync.
#[repr(C)]
pub struct TrapFrame {
    pub ra: u64, // x1   (offset 0)
    pub sp: u64, // x2   (offset 8)
    pub gp: u64, // x3
    pub tp: u64, // x4
    pub t0: u64, // x5
    pub t1: u64,
    pub t2: u64,
    pub s0: u64, // x8
    pub s1: u64,
    pub a0: u64, // x10
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
    pub a7: u64,
    pub s2: u64, // x18
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub s6: u64,
    pub s7: u64,
    pub s8: u64,
    pub s9: u64,
    pub s10: u64,
    pub s11: u64,
    pub t3: u64, // x28
    pub t4: u64,
    pub t5: u64,
    pub t6: u64,
    pub sepc: u64,    // offset 248
    pub sstatus: u64, // offset 256
}

#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(frame: *mut TrapFrame) {
    let scause: u64;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause);
    }
    match decode_scause(scause) {
        // SAFETY: `frame` points at the `TrapFrame` `trap_entry` built on this
        // hart's kernel stack; reading `sstatus` from it for the SPP gate is
        // sound and we are its sole accessor for the duration of the handler.
        TrapCause::SupervisorTimerInterrupt => handle_timer(unsafe { &*frame }),
        TrapCause::SupervisorSoftwareInterrupt => crate::ipi::handle_pending(),
        TrapCause::EnvCallFromUMode => {
            // SAFETY: `frame` points at the `TrapFrame` `trap_entry` just
            // built on this hart's kernel stack; we are its sole accessor
            // for the duration of the handler.
            handle_user_ecall(unsafe { &mut *frame });
        }
        // Instruction/load/store page fault (codes 12/13/15) from U-mode is
        // the isolation firewall catching userspace touching memory it has no
        // `U`-bit access to. Count it and park (v0.7a has no process teardown).
        // The same fault from S-mode is a real kernel bug — fall through to panic.
        TrapCause::UnknownException(12 | 13 | 15)
            if unsafe { &*frame }.sstatus & SSTATUS_SPP == 0 =>
        {
            handle_user_fault();
        }
        other => panic!("unhandled trap: {other:?} (scause={scause:#x})"),
    }
}

/// `sstatus.SPP` (bit 8): the privilege the trap came from. 0 = User.
const SSTATUS_SPP: u64 = 1 << 8;

/// A U-mode access faulted — the page-table `U`-bit firewall did its job
/// (v0.7a has no capability layer yet; that's v0.7b). Count it and park this
/// hart: with no process teardown we can't reschedule, and returning would
/// re-run the faulting instruction forever. Hart 0 carries on. Never returns.
fn handle_user_fault() -> ! {
    if let Some(id) = crate::user::user_fault_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    loop {
        // SAFETY: park until the next interrupt; nothing to do on this hart.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

/// Handle an `ecall` from U-mode. The v0.7b kernel surface is **invoke a
/// capability**: `a7` selects the syscall (`Invoke`), `a0` is the handle
/// into the *calling process's* `CapTable`, `a1` the argument. We resolve
/// and rights-check against that table (no ambient authority), then advance
/// `sepc` past the `ecall`.
fn handle_user_ecall(frame: &mut TrapFrame) {
    use snitchos_abi::Syscall;
    match Syscall::from_usize(frame.a7 as usize) {
        Some(Syscall::Invoke) => handle_invoke(frame),
        Some(Syscall::Exit) => handle_exit(), // does not return
        Some(Syscall::Yield) => crate::sched::yield_now(),
        Some(Syscall::SpanOpen) => handle_span_open(frame),
        Some(Syscall::SpanClose) => handle_span_close(frame),
        Some(Syscall::MapAnon) => handle_map_anon(frame),
        Some(Syscall::DebugWrite) => handle_debug_write(frame),
        Some(Syscall::Send) => handle_send(frame),
        Some(Syscall::Receive) => handle_receive(frame),
        Some(Syscall::Call) => handle_call(frame),
        Some(Syscall::Reply) => handle_reply(frame),
        None => {
            let n = frame.a7 as u8;
            refuse(frame, n, protocol::RefusalReason::UnknownSyscall);
        }
    }
    // `ecall` is a 4-byte instruction; without advancing past it, `sret`
    // would re-execute it and we'd trap on it forever. (Not reached for
    // `Exit` — `handle_exit` never returns.)
    frame.sepc = frame.sepc.wrapping_add(4);
}

/// Terminate the calling user process. Snitches `snitchos.user.exits_total`,
/// clears this hart's current-process pointer (the process is gone), then
/// hands the hart to its next ready task via `sched::exit_now` — which never
/// returns. On the userspace workload that next task is `hart_1_main`, whose
/// idle loop `wfi`s, so the hart goes truly idle rather than busy-spinning.
/// v0.7b leaks the address space + caps; reclamation is a later milestone.
fn handle_exit() -> ! {
    if let Some(id) = crate::user::user_exits_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(core::ptr::null_mut(), Ordering::Relaxed);
    crate::sched::exit_now()
}

/// Capability invocation. Resolve `a0` against the running process's
/// `CapTable`; on success perform the authorized operation (emit `a1` to
/// the `TelemetrySink`'s bound counter), else refuse with a nonzero `a0`.
/// The authority decision itself is the pure, host-tested
/// [`kernel_core::cap::invoke_telemetry`]; here we only act on its result.
fn handle_invoke(frame: &mut TrapFrame) {
    use kernel_core::cap::{Handle, invoke_telemetry};
    use snitchos_abi::Syscall;

    let sc = Syscall::Invoke as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    // Resolve under the lock, copy out the counter, drop the lock before
    // emitting — never hold a Mutex across telemetry emission.
    let outcome = invoke_telemetry(&proc.caps.lock(), handle);
    match outcome {
        Ok(counter) => {
            crate::tracing::emit_metric(counter, frame.a1 as i64);
            frame.a0 = 0; // success
        }
        Err(denied) => {
            // Snitch the refused authority decision two ways: the pre-
            // registered `cap_denied_total` counter (a rate) and a
            // `SyscallRefused` event carrying the *reason* (self-describing,
            // so a denial is never a silent missing-result). Counter is pre-
            // registered (`user::init_metric`) to avoid interning in trap
            // context.
            if let Some(id) = crate::user::cap_denied_metric_id() {
                crate::tracing::emit_metric(id, 1);
            }
            refuse(frame, sc, refusal_for(denied)); // emits SyscallRefused + sets a0
        }
    }
}

/// Send an inline message over an IPC endpoint. `a0` = `Endpoint` handle
/// (needs `SEND`), `a1..=a4` = the message words. Resolve the cap against the
/// running process's table; on success drive the rendezvous: either deliver
/// to a waiting receiver and wake it, or block until one arrives. The endpoint
/// lock is dropped inside `ipc::send_begin` before we block/wake (never hold a
/// lock across the switch). Returns `0` on success, the error sentinel if the
/// capability is refused.
fn handle_send(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_send, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Send as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_send(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let ep = match ep {
        Ok(ep) => ep,
        Err(denied) => {
            refuse(frame, sc, refusal_for(denied));
            return;
        }
    };

    let me = crate::sched::current_task_id();
    let msg = [frame.a1, frame.a2, frame.a3, frame.a4];
    // Carry the sender's trace context: its innermost open span becomes the
    // parent of the receiver's handling span (kernel-populated — userspace can
    // neither forge nor forget it).
    let parent = crate::tracing::current_span_id();
    match crate::ipc::send_begin(ep, me, msg, parent) {
        crate::ipc::SendStep::Deliver { wake } => crate::sched::wake(wake),
        crate::ipc::SendStep::Block => {
            crate::ipc::BLOCKS_TOTAL.fetch_add(1, Ordering::Relaxed);
            crate::sched::block_current();
        }
    }
    // Either path completes the rendezvous: the message was (or will be) taken
    // by the receiver. Report success.
    frame.a0 = 0;
}

/// Receive an inline message from an IPC endpoint. `a0` = `Endpoint` handle
/// (needs `RECV`). Drive the rendezvous: take a waiting sender's message (and
/// wake it), or block until one arrives and take the message delivered to us.
/// Writes the words into `a1..=a4` and `0` into `a0`; refuses with the error
/// sentinel if the capability is refused.
fn handle_receive(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_recv, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Receive as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_recv(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let ep = match ep {
        Ok(ep) => ep,
        Err(denied) => {
            refuse(frame, sc, refusal_for(denied));
            return;
        }
    };

    let me = crate::sched::current_task_id();
    let delivered = match crate::ipc::receive_begin(ep, me) {
        crate::ipc::RecvStep::Deliver { delivered, wake } => {
            // Wake the sender only for a one-way `send`. For a `call` the caller
            // is parked awaiting its reply — we mint it a reply cap instead.
            if delivered.reply_to.is_none() {
                crate::sched::wake(wake);
            }
            delivered
        }
        crate::ipc::RecvStep::Block => {
            crate::ipc::BLOCKS_TOTAL.fetch_add(1, Ordering::Relaxed);
            crate::sched::block_current();
            // Resumed: a sender stashed our message under our id at rendezvous.
            crate::ipc::take_delivered(ep, me)
        }
    };

    // If this was a `call`, mint a one-shot reply cap into *this* (server)
    // process naming the caller, and hand its handle back in `a5`; a one-way
    // `send` yields `a5 = 0`.
    frame.a5 = reply_handle_for(proc, me, delivered.reply_to);

    // The message crossed. Count it, and record the topology + trace link on
    // the wire — both at delivery, outside the endpoint critical section.
    crate::ipc::MESSAGES_TOTAL.fetch_add(1, Ordering::Relaxed);
    crate::tracing::emit_message(ep.0, delivered.from.0, me.0, delivered.parent);
    // Seed the sender's span as our incoming parent — the next span this task
    // opens (its handling span) becomes a child, so the trace continues across
    // the process boundary.
    crate::tracing::set_current_parent(delivered.parent);
    frame.a1 = delivered.msg[0];
    frame.a2 = delivered.msg[1];
    frame.a3 = delivered.msg[2];
    frame.a4 = delivered.msg[3];
    frame.a0 = 0;
}

/// Mint a one-shot reply cap into `proc` (the receiving server) naming the
/// blocked `caller`, snitch it as `CapEvent::Transferred`, and return its raw
/// handle for `receive`'s `a5`. Returns `0` when `reply_to` is `None` (a
/// one-way `send` — no reply expected). The first cross-process cap insertion:
/// the kernel grants the server authority to answer exactly this caller once.
fn reply_handle_for(
    proc: &crate::process::Process,
    holder: kernel_core::sched::TaskId,
    reply_to: Option<kernel_core::sched::TaskId>,
) -> u64 {
    use kernel_core::cap::{Capability, Object, Rights};

    let Some(caller) = reply_to else {
        return 0;
    };
    let handle = proc
        .caps
        .lock()
        .insert_once(Capability { object: Object::Reply { caller }, rights: Rights::NONE });
    crate::tracing::emit_cap_transferred(
        crate::process::next_cap_id(),
        holder.0,
        protocol::CapObject::Reply,
        Rights::NONE.bits(),
    );
    u64::from(handle.raw())
}

/// RPC `call`: `a0` = `Endpoint` handle (needs `SEND`), `a1..=a4` = request
/// words. Delivers the request (marked as a call so the receiver mints a reply
/// cap), then parks the caller until `reply` wakes it — at which point the
/// response words are written into `a1..=a4`. The caller's span stays open
/// across the round-trip, so the server's handling span nests under it.
fn handle_call(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_send, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Call as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_send(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let ep = match ep {
        Ok(ep) => ep,
        Err(denied) => {
            refuse(frame, sc, refusal_for(denied));
            return;
        }
    };

    crate::ipc::CALLS_TOTAL.fetch_add(1, Ordering::Relaxed);
    let me = crate::sched::current_task_id();
    let req = [frame.a1, frame.a2, frame.a3, frame.a4];
    let parent = crate::tracing::current_span_id();
    match crate::ipc::call_begin(ep, me, req, parent) {
        crate::ipc::SendStep::Deliver { wake } => crate::sched::wake(wake),
        crate::ipc::SendStep::Block => {}
    }
    // The caller always parks awaiting the reply — woken by `reply`, never by
    // the request rendezvous (a receiver taking a call mints a reply cap
    // instead of waking us).
    crate::ipc::BLOCKS_TOTAL.fetch_add(1, Ordering::Relaxed);
    crate::sched::block_current();

    // Resumed by `reply`: deliver the response words.
    let resp = crate::ipc::take_reply(me);
    frame.a1 = resp[0];
    frame.a2 = resp[1];
    frame.a3 = resp[2];
    frame.a4 = resp[3];
    frame.a0 = 0;
}

/// RPC `reply`: `a0` = reply-cap handle (from `receive`'s `a5`), `a1..=a4` =
/// response words. Resolves + **consumes** the one-shot reply cap (a second
/// `reply` is refused), stashes the response for the blocked caller, and wakes
/// it. The reply is point-to-point (server→caller), not endpoint-mediated.
fn handle_reply(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_reply, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Reply as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    // Resolve then consume the reply cap under the server's caps lock; drop the
    // lock before waking (never hold a Mutex across the path that may switch).
    let resolved = {
        let mut caps = proc.caps.lock();
        match invoke_reply(&caps, handle) {
            Ok(caller) => {
                caps.consume(handle);
                Ok(caller)
            }
            Err(denied) => Err(denied),
        }
    };
    let caller = match resolved {
        Ok(caller) => caller,
        Err(denied) => {
            refuse(frame, sc, refusal_for(denied));
            return;
        }
    };

    let resp = [frame.a1, frame.a2, frame.a3, frame.a4];
    crate::ipc::stash_reply(caller, resp);
    crate::sched::wake(caller);
    crate::ipc::REPLIES_TOTAL.fetch_add(1, Ordering::Relaxed);
    frame.a0 = 0;
}

/// Open a span on behalf of U-mode. `a0` = `SpanSink` handle, `a1` = name
/// pointer, `a2` = name length. Validates the capability, copies + interns the
/// name, opens a span on the calling task's cursor, and returns the span id in
/// `a0` (or `u64::MAX` on refusal).
fn handle_span_open(frame: &mut TrapFrame) {
    use kernel_core::cap::{Handle, invoke_span};
    use kernel_core::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::SpanOpen as u8;

    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    // Authority: the caller must hold a `SpanSink` cap at handle `a0`. Resolve
    // under the lock, then drop it before the intern/emit path.
    let denied = {
        let caps = proc.caps.lock();
        invoke_span(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        refuse(frame, sc, refusal_for(d));
        return;
    }

    // Copy the span name out of user memory (range-validated, SUM-guarded).
    let mut buf = [0u8; MAX_USER_STR_LEN];
    let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, frame.a2 as usize, &mut buf)
    else {
        refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let Ok(name) = core::str::from_utf8(bytes) else {
        refuse(frame, sc, RefusalReason::BadUtf8);
        return;
    };

    // Intern on demand (quota-gated) + open a span on this task's cursor; hand
    // back the `{id, parent}` close token (id in a0, parent in a1). A new name
    // past the per-process quota is refused without registering.
    let Some(opened) = crate::tracing::span_open_bounded(
        name,
        &proc.span_names_registered,
        crate::process::Process::MAX_SPAN_NAMES,
    ) else {
        refuse(frame, sc, RefusalReason::Quota);
        return;
    };
    frame.a0 = opened.id.0;
    frame.a1 = opened.parent.0;
}

/// Close a span on behalf of U-mode. `a0` = span id, `a1` = parent id (the
/// token `SpanOpen` returned). The kernel validates the id is the caller's
/// innermost open span (cursor top), refusing an out-of-order/forged close.
fn handle_span_close(frame: &mut TrapFrame) {
    use protocol::{RefusalReason, SpanId};
    use snitchos_abi::Syscall;

    let id = SpanId(frame.a0);
    let parent = SpanId(frame.a1);
    if crate::tracing::span_close_checked(id, parent) {
        frame.a0 = 0; // success
    } else {
        refuse(frame, Syscall::SpanClose as u8, RefusalReason::BadSpanId);
    }
}

/// Refuse a syscall: snitch a `SyscallRefused` event (so the denial is never
/// silent) and return the error sentinel in `a0`.
fn refuse(frame: &mut TrapFrame, syscall: u8, reason: protocol::RefusalReason) {
    crate::tracing::emit_syscall_refused(syscall, reason);
    frame.a0 = u64::MAX;
}

/// Map a capability-invocation denial to its wire refusal reason.
fn refusal_for(denied: kernel_core::cap::Denied) -> protocol::RefusalReason {
    use kernel_core::cap::Denied;
    use protocol::RefusalReason;
    match denied {
        Denied::NoSuchCapability => RefusalReason::CapNotFound,
        Denied::MissingRight => RefusalReason::CapWrongRights,
        Denied::WrongObject => RefusalReason::CapWrongObject,
    }
}

/// Resolve the user process running on this hart, or — if none — snitch a
/// `NoProcess` refusal (setting the error sentinel in `a0`) and return `None`
/// for the caller to early-return. Every capability syscall opens with this:
/// it needs the calling process's `CapTable`.
fn current_process_or_refuse(
    frame: &mut TrapFrame,
    syscall: u8,
) -> Option<&'static crate::process::Process> {
    let proc = crate::process::CURRENT_PROCESS.this_cpu().load(Ordering::Relaxed);
    // SAFETY: set by `user::run` on this hart before `sret`; the `Process` lives
    // in that never-returning frame, so a non-null pointer is valid for the
    // life of the kernel. Null only if no user process runs here — which then
    // could not have issued this U-mode `ecall`.
    match unsafe { proc.as_ref() } {
        Some(proc) => Some(proc),
        None => {
            refuse(frame, syscall, protocol::RefusalReason::NoProcess);
            None
        }
    }
}

/// Map a fresh anonymous memory region for U-mode. `a0` = bytes requested (the
/// runtime page-aligns). Maps that many zeroed frames into the process's heap
/// region and returns the region's **base** VA in `a0` (or `u64::MAX` if
/// refused — out of frames, or past the per-process memory cap). mmap-shaped:
/// the runtime allocator `claim`s the returned region. Placement is a simple
/// bump pointer (`heap_top`) for now; the allocator doesn't assume regions
/// abut, so disjoint placement + unmap can land later without an ABI change.
fn handle_map_anon(frame: &mut TrapFrame) {
    use kernel_core::mmu::PtePerms;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    use crate::frame::FRAME_SIZE;
    use crate::process::Process;

    let sc = Syscall::MapAnon as u8;
    let Some(proc) = current_process_or_refuse(frame, sc) else {
        return;
    };

    let bytes = (frame.a0 as usize).next_multiple_of(FRAME_SIZE);
    let base = proc.heap_top.load(Ordering::Relaxed);
    let end = base.saturating_add(bytes);
    if bytes == 0 || end > Process::HEAP_BASE + Process::HEAP_MAX {
        refuse(frame, sc, RefusalReason::OutOfMemory);
        return;
    }

    let perms = PtePerms::U.union(PtePerms::R).union(PtePerms::W);
    let mut va = base;
    while va < end {
        let Some(f) = crate::frame::alloc_zeroed() else {
            // Out of frames mid-map: the already-mapped pages leak until process
            // teardown (none in v0.7), and `heap_top` isn't advanced, so the
            // runtime never `claim`s a partial region.
            refuse(frame, sc, RefusalReason::OutOfMemory);
            return;
        };
        if crate::mmu::map_in(proc.root_pa, va, f.addr(), perms).is_err() {
            refuse(frame, sc, RefusalReason::OutOfMemory);
            return;
        }
        va += FRAME_SIZE;
    }
    // Make the new pages visible on this hart. SAFETY: flush stale (negative)
    // TLB entries for the freshly-mapped VAs; new mappings, so a local sfence
    // suffices — nothing on another hart cached them.
    unsafe { asm!("sfence.vma", options(nostack, nomem)) };

    proc.heap_top.store(end, Ordering::Relaxed);
    frame.a0 = base as u64;
}

/// Write bytes to the debug/stdout channel for U-mode. `a0` = pointer, `a1` =
/// length. Copies the bytes out (range-validated, SUM-guarded) and emits a
/// snitched `Log` frame attributed to the caller. Returns bytes written in
/// `a0` (or `u64::MAX` on a bad pointer). Ungated — writing to the debug log is
/// not an authority, like `Yield`. The runtime chunks writes to fit
/// `MAX_USER_STR_LEN`; a longer write becomes several `Log` frames.
fn handle_debug_write(frame: &mut TrapFrame) {
    use kernel_core::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::DebugWrite as u8;
    let mut buf = [0u8; MAX_USER_STR_LEN];
    let Some(bytes) = crate::user::copy_from_user(frame.a0 as usize, frame.a1 as usize, &mut buf)
    else {
        refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let Ok(msg) = core::str::from_utf8(bytes) else {
        refuse(frame, sc, RefusalReason::BadUtf8);
        return;
    };
    crate::tracing::emit_log(msg);
    frame.a0 = bytes.len() as u64;
}

/// Timer IRQ handler. Kept tiny: measure duration, arm the next
/// deadline (which acks the current pending bit), then set a flag so
/// the main thread knows to do the real work. **No locks taken here**
/// — the main thread owns all telemetry emission.
fn handle_timer(frame: &TrapFrame) {
    let start = CLOCK.now();
    let interval = TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);
    CLOCK.arm(start + interval);
    TICK_PENDING.this_cpu().store(true, Ordering::Relaxed);
    let end = CLOCK.now();
    LAST_IRQ_DURATION
        .this_cpu()
        .store(end.wrapping_sub(start), Ordering::Relaxed);

    // v0.8 preemption: if this timer interrupted a *userspace* task that has
    // overrun its quantum, deschedule it now. `SPP == 0` means the trap came
    // from U-mode; kernel code (`SPP == 1`) is never preempted, keeping the
    // cooperative "exclusive until I yield" invariant. When the descheduled
    // task is next picked, it resumes here, returns, and `trap_entry` restores
    // its full `TrapFrame` and `sret`s to the exact user PC it was running.
    crate::sched::maybe_preempt(frame.sstatus & SSTATUS_SPP == 0);
}

/// One-time timer setup: set the interval, arm the first deadline,
/// enable interrupts. Call once from kmain after the trap vector is
/// installed.
///
/// # Safety
///
/// Trap vector must be installed (`set_trap_vector`) before this —
/// otherwise the first timer interrupt jumps to garbage.
pub unsafe fn init_timer(interval_ticks: u64) {
    TIMER_INTERVAL_TICKS.store(interval_ticks, Ordering::Relaxed);
    CLOCK.arm(CLOCK.now() + interval_ticks);
    unsafe { enable_timer_interrupts() };
}

/// Enable S-mode timer interrupts. Sets the per-source enable bit
/// (`sie.STIE`) and the global S-mode interrupt enable (`sstatus.SIE`).
///
/// Order matters: set the per-source mask before the global enable,
/// so a stale pending interrupt from another source can't fire on us
/// the instant we flip SIE.
///
/// # Safety
///
/// After this returns, timer interrupts will be delivered to our
/// trap handler whenever `time >= stimecmp`. Caller must ensure the
/// trap vector is installed and the handler is ready to deal with
/// them.
pub unsafe fn enable_timer_interrupts() {
    unsafe {
        // sie.STIE = bit 5 (Supervisor Timer Interrupt Enable).
        asm!("csrs sie, {}", in(reg) 1u64 << 5);
        // sstatus.SIE = bit 1 (Supervisor Interrupt Enable, global).
        asm!("csrs sstatus, {}", in(reg) 1u64 << 1);
    }
}

/// Enable S-mode software interrupts (IPIs). `sie.SSIE` = bit 1.
/// `sstatus.SIE` is set globally by `enable_timer_interrupts`;
/// call this either before or after — the per-source bit is what
/// gates SSIP-driven trap entry.
///
/// # Safety
///
/// Trap vector must be installed and `ipi::handle_pending` must be
/// ready to run. Any pending `SSIP` from before this call fires
/// immediately on return.
pub unsafe fn enable_software_interrupts() {
    unsafe {
        // sie.SSIE = bit 1.
        asm!("csrs sie, {}", in(reg) 1u64 << 1);
    }
}

/// Install our `trap_entry` (from `trap.S`) as the S-mode trap vector,
/// and establish the in-kernel `sscratch` convention.
/// After this returns, every trap (exception or interrupt) routes to
/// our handler. Call once per hart, at boot, before anything that might
/// trap.
///
/// `sscratch` is zeroed here: `trap_entry`'s stack-switch swap uses
/// `sscratch == 0` as the "we were already in the kernel, this is a
/// trusted stack" sentinel. While running user code the scheduler parks
/// the thread's kernel stack top in `sscratch` instead; the trap exit
/// re-arms it. At boot we are in the kernel, so the sentinel is 0.
///
/// # Safety
///
/// No other code should be relying on the previous `stvec` value.
/// At first boot stvec is undefined; we're writing it for the first time.
pub unsafe fn set_trap_vector() {
    unsafe extern "C" {
        fn trap_entry();
    }
    let addr = trap_entry as *const () as usize;
    unsafe {
        asm!(
          "csrw stvec, {}",
          "csrw sscratch, zero",
          in(reg) addr,
        );
    }
}
