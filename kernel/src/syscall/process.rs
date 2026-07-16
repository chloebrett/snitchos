//! Process-lifecycle syscalls: `Exit`, `Wait`, and `Spawn`.

use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Terminate the calling user process with exit status `a0` (v0.12). Snitches
/// `snitchos.user.exits_total`, clears this hart's current-process pointer,
/// records the exit status + wakes any parent blocked in `Wait` on this task
/// (the reaping bookkeeping), then hands the hart to its next ready task via
/// `sched::exit_now` — which never returns. v0.7b leaks the address space + caps;
/// reclamation is a later milestone.
pub(super) fn handle_exit(frame: &TrapFrame) -> ! {
    let status = frame.a0 as i32;
    let me = crate::sched::current_task_id();

    if let Some(id) = crate::user::user_exits_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(core::ptr::null_mut(), Ordering::Relaxed);

    // Record the zombie + wake any parent blocked in `Wait` on us. Must happen
    // before `exit_now` (which never returns). `wake` only re-enqueues a blocked
    // task, so a not-yet-blocked parent (cross-hart racing) is a no-op — fine, as
    // v0.12 `Wait` is same-hart and the parent is already blocked by here.
    if let Some(parent) = crate::sched::note_exit(me, status) {
        crate::sched::wake(parent);
    }

    // Owned by the REAP table: the zombie persists (holding `status`) until the
    // parent `Wait`s + `reap_task`s it — so exit via the *owned* path, which does
    // not mark the task ownerless for the heartbeat sweep.
    crate::sched::exit_now_owned()
}

/// Terminate a child named by an `Object::Process` cap (`a0` = its handle), the
/// v2a `Kill` syscall. Resolves + validates the `KILL` right over a `Process`
/// object (`invoke_kill`, host-tested in `kernel_core::cap`), then hands the target
/// to [`crate::sched::kill_task`]. On a successful kill the lifecycle cap is
/// **spent** — consumed from the caller's table and snitched as `CapEvent::Revoked`,
/// the destruction half of the mint at `Spawn` (the lifecycle is symmetric on the
/// wire). A missing/wrong cap refuses via the normal cap path; a target v2a can't yet
/// safely kill (self / cross-hart-running / blocked — see `plans/supervision-v2.md`
/// §3a) refuses cleanly and logs the precise deferred reason.
pub(super) fn handle_kill(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_kill, Handle, Rights};
    use snitchos_abi::Syscall;

    let sc = Syscall::Kill as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    // Resolve + validate the Process/KILL cap, capturing its global id for the
    // Revoked event before we (maybe) consume the holding.
    let (target, cap_id) = {
        let caps = proc.caps.lock();
        match invoke_kill(&caps, handle) {
            Ok(t) => (t, caps.cap_id_of(handle).unwrap_or(0)),
            Err(denied) => {
                super::refuse(frame, sc, super::refusal_for(denied));
                return;
            }
        }
    };

    match crate::sched::kill_task(target) {
        crate::sched::KillOutcome::Killed | crate::sched::KillOutcome::AlreadyDead => {
            // Spend the lifecycle cap: the object it named is gone. Consume the
            // holding, then snitch a `Revoked` — creation minted this cap at `Spawn`,
            // destruction spends it here.
            proc.caps.lock().consume(handle);
            crate::tracing::emit_cap_revoked(
                cap_id,
                0,
                crate::sched::current_task_id().0,
                protocol::CapObject::Process,
                Rights::KILL.bits(),
                0,
                [0; snitchos_abi::CAP_NAME_LEN],
            );
            frame.a0 = 0;
        }
        crate::sched::KillOutcome::RefusedSelf | crate::sched::KillOutcome::Deferred => {
            // The cap was valid, but v2a can't safely kill this target yet. Refuse
            // (reusing an existing wire reason to avoid a protocol-enum ripple) and
            // log the precise deferred reason — the itest happy path kills a `Ready`
            // spinner and never lands here.
            crate::tracing::emit_log(&alloc::format!(
                "kill deferred: target {} is self or running on another hart (v2b)",
                target.0
            ));
            super::refuse(frame, sc, protocol::RefusalReason::CapWrongObject);
        }
    }
}

/// Wait for a child to exit and return its status (v0.12). `a0` = the child's
/// task id; returns its exit status in `a0`. Blocks until the child `Exit`s
/// (re-checking on each wake), or returns immediately if it already exited
/// (reaping the zombie). Same-hart in v0.12.
pub(super) fn handle_wait(frame: &mut TrapFrame) {
    use kernel_core::reap::WaitStep;
    use kernel_core::sched::TaskId;

    let me = crate::sched::current_task_id();
    let child = TaskId(frame.a0 as u32);
    loop {
        match crate::sched::wait_for(me, child) {
            WaitStep::Ready(status) => {
                // The child is fully `Exited` and we run in our own address space,
                // so it's safe to reclaim its resources now (frees the child's user
                // AS, `Process`, and kernel stack — see `sched::reap_task`).
                crate::sched::reap_task(child);
                frame.a0 = status as u64;
                return;
            }
            // Recorded as the waiter; block until the child's `Exit` wakes us,
            // then loop to re-check (it'll find the zombie and reap it).
            WaitStep::Block => crate::sched::block_current(),
        }
    }
}

/// Wait for *any* child to exit and return its status + id (v0.13). No args;
/// returns the exited child's status in `a0` and its task id in `a1`. Blocks until
/// any child this caller spawned `Exit`s (re-checking on each wake), or returns
/// immediately if one already exited (reaping the zombie). The supervising-parent
/// variant of [`handle_wait`]; same-hart in v0.13.
pub(super) fn handle_wait_any(frame: &mut TrapFrame) {
    use kernel_core::reap::WaitAnyStep;

    let me = crate::sched::current_task_id();
    loop {
        match crate::sched::wait_for_any(me) {
            WaitAnyStep::Ready { child, status } => {
                // The child is fully `Exited` and we run in our own address space,
                // so it's safe to reclaim its resources now (see `reap_task`).
                crate::sched::reap_task(child);
                frame.a0 = status as u64;
                frame.a1 = u64::from(child.0);
                return;
            }
            // Recorded as an any-waiter; block until a child's `Exit` wakes us,
            // then loop to re-check (it'll find the zombie and reap it).
            WaitAnyStep::Block => crate::sched::block_current(),
        }
    }
}

/// Spawn a new userspace process, delegating a subset of the caller's caps to it
/// (v0.11). `a0` = program id, `a1` = pointer to a `[u32; N]` handle array in the
/// caller's space, `a2` = `N`. Resolves the program, delegates the named caps —
/// **all-or-nothing**, refusing the whole spawn if any handle is unheld — builds
/// the child with those caps plus bootstrap telemetry/span, and returns the
/// child's task id in `a0` (or `usize::MAX` on refusal). Ambient like `Yield`,
/// but a process can only delegate authority it already holds.
pub(super) fn handle_spawn(frame: &mut TrapFrame) {
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::Spawn as u8;

    // The caller's table is what we delegate *from*.
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Resolve the program image to launch.
    let Some((name, image)) = crate::user::spawnable_program(frame.a0 as usize) else {
        super::refuse(frame, sc, RefusalReason::UnknownProgram);
        return;
    };

    // Resolve + delegate the caller's `[u32; N]` handle array (a1 = ptr, a2 = N).
    let delegated = match delegate_from_user(proc, frame.a1 as usize, frame.a2 as usize) {
        Ok(d) => d,
        Err(reason) => {
            super::refuse(frame, sc, reason);
            return;
        }
    };

    // Build + queue the child on this hart; hand back its task id.
    let child = crate::user::spawn_process_with_caps(
        crate::percpu::current_hartid(),
        name,
        image,
        delegated,
        kernel_core::sched::Priority::Normal,
    );
    // Record parentage so the caller can later `WaitAny` and have this child's
    // exit matched to it.
    crate::sched::note_spawn(crate::sched::current_task_id(), child);
    frame.a0 = u64::from(child.0);
    // Mint the caller a lifecycle cap over the child (v2a) so it can later `Kill`
    // it. Additive: `a0` is the task id as before, `a1` the new Process-cap handle.
    frame.a1 = mint_process_cap(proc, child);
}

/// Mint an `Object::Process { id: child }` capability carrying `KILL` into the
/// caller's table and return its handle (v2a) — the "lifecycle cap" that authorizes
/// a later `Kill` of the just-spawned child. Kernel-installed at `Spawn`, so it's a
/// `CapEvent::Granted` (like the bootstrap sinks) and a derivation-tree root
/// (`parent_cap_id == 0`); it **composes** — the parent can delegate `KILL` over its
/// subtree to a sub-supervisor. A `Process` cap names a kernel-side task id, not a
/// user-visible object, so it carries no wire name.
fn mint_process_cap(proc: &crate::process::Process, child: kernel_core::sched::TaskId) -> u64 {
    use kernel_core::cap::{Capability, Object, Rights};

    let cap_id = crate::process::next_cap_id();
    let handle = proc.caps.lock().insert_with_id(
        Capability { object: Object::Process { id: child }, rights: Rights::KILL },
        cap_id,
        0,
    );
    crate::tracing::emit_cap_granted(
        cap_id,
        crate::sched::current_task_id().0,
        protocol::CapObject::Process,
        Rights::KILL.bits(),
        [0; snitchos_abi::CAP_NAME_LEN],
    );
    u64::from(handle.raw())
}

/// Copy the caller's `[u32; N]` delegate-handle array (`ptr`/`n`) and resolve it
/// against the caller's `CapTable` into `(cap, parent_cap_id)` pairs — the shared
/// front half of `Spawn`/`SpawnImage`. **All-or-nothing**: an unheld handle
/// refuses the whole set. Pairs each cap with its source holding's global cap id
/// so the child's `CapEvent::Transferred` can name the derivation edge. `Err`
/// carries the refusal reason (bad range, or an unheld handle).
fn delegate_from_user(
    proc: &crate::process::Process,
    ptr: usize,
    n: usize,
) -> Result<alloc::vec::Vec<(kernel_core::cap::Capability, u64)>, protocol::RefusalReason> {
    use kernel_core::cap::Handle;
    use protocol::RefusalReason;

    const MAX_DELEGATE: usize = 16;
    if n > MAX_DELEGATE {
        return Err(RefusalReason::BadUserRange);
    }
    let mut handles: alloc::vec::Vec<Handle> = alloc::vec::Vec::new();
    if n > 0 {
        let mut buf = [0u8; MAX_DELEGATE * core::mem::size_of::<u32>()];
        let byte_len = n * core::mem::size_of::<u32>();
        let bytes =
            crate::user::copy_from_user(ptr, byte_len, &mut buf).ok_or(RefusalReason::BadUserRange)?;
        handles = bytes
            .chunks_exact(core::mem::size_of::<u32>())
            .map(|c| Handle::from_raw(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect();
    }
    // Lock released with the returned `Vec` built, so the child build never
    // contends on the parent's table.
    let caps = proc.caps.lock();
    kernel_core::cap::delegate(&caps, &handles)
        .map(|caps_vec| {
            handles
                .iter()
                .zip(caps_vec)
                .map(|(handle, cap)| (cap, caps.cap_id_of(*handle).unwrap_or(0)))
                .collect()
        })
        .map_err(|e| match e {
            // A `Once`/affine cap in the delegate set refuses the whole spawn,
            // snitched with its own reason rather than the generic not-found.
            kernel_core::cap::CapError::NotDelegable => RefusalReason::CapNotDelegable,
            // A bad/stale handle: the delegated cap isn't held.
            _ => RefusalReason::CapNotFound,
        })
}

/// Spawn a userspace process from a **caller-supplied ELF image** (v0.13,
/// `SpawnImage`) — the path for running an executable read out of the filesystem,
/// vs [`handle_spawn`]'s kernel-embedded registry. `a0`/`a1` = the ELF bytes
/// (ptr/len) in the caller's space, `a2`/`a3` = the delegate handle array
/// (ptr/`N`). The kernel copies the image into an owned heap buffer (freed once
/// loaded), delegates the named caps all-or-nothing, and spawns. Returns the
/// child's task id in `a0` (or `usize::MAX` on refusal).
pub(super) fn handle_spawn_image(frame: &mut TrapFrame) {
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::SpawnImage as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Copy the ELF image out of user memory into an owned kernel buffer. Bounded
    // so a bad/huge length can't ask the kernel to allocate unboundedly.
    const MAX_IMAGE: usize = 4 * 1024 * 1024;
    let len = frame.a1 as usize;
    if len == 0 || len > MAX_IMAGE {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }
    // `copy_from_user` caps each copy at `MAX_USER_STR_LEN` (it was built for
    // name-sized copies), so pull the ELF across in chunks.
    let mut image = alloc::vec![0u8; len];
    let mut off = 0;
    while off < len {
        let take = core::cmp::min(kernel_core::mmu::MAX_USER_STR_LEN, len - off);
        if crate::user::copy_from_user(frame.a0 as usize + off, take, &mut image[off..off + take])
            .is_none()
        {
            super::refuse(frame, sc, RefusalReason::BadUserRange);
            return;
        }
        off += take;
    }

    // Validate the ELF *here*, synchronously, so a malformed user image refuses
    // cleanly — the child's later `load` panics on a bad image, and a userspace
    // program must not be able to panic the kernel. (`UnknownProgram` = no
    // runnable program in this image.)
    if kernel_core::elf::parse(&image).is_err() {
        super::refuse(frame, sc, RefusalReason::UnknownProgram);
        return;
    }

    let delegated = match delegate_from_user(proc, frame.a2 as usize, frame.a3 as usize) {
        Ok(d) => d,
        Err(reason) => {
            super::refuse(frame, sc, reason);
            return;
        }
    };

    let child = crate::user::spawn_image_with_caps(
        crate::percpu::current_hartid(),
        "spawned-image",
        image.into_boxed_slice(),
        delegated,
        kernel_core::sched::Priority::Normal,
    );
    crate::sched::note_spawn(crate::sched::current_task_id(), child);
    frame.a0 = u64::from(child.0);
    frame.a1 = mint_process_cap(proc, child);
}
