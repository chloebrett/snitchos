//! Kernel-internal synchronisation primitives. The single chokepoint
//! for locks in the kernel binary — every `static Mutex<...>` and
//! every `Once<...>` goes through here.
//!
//! Today these are thin wrappers around `spin::Mutex` / `spin::Once`
//! with no-op acquisition/release hooks. The wrapper exists so that
//! when preempt-disable (v0.5.x) and SMP IRQ-disable (v0.7+) land,
//! the change happens *in this file* rather than at every lock site.
//!
//! Discipline: nothing outside `kernel::sync` should reference
//! `spin::Mutex` / `spin::MutexGuard` / `spin::Once` directly. A
//! workspace `disallowed_types` clippy lint enforces this; the
//! `#[allow]` below is the only sanctioned escape hatch.
//!
//! Flavour: one. Every `Mutex::lock()` will eventually save+disable
//! interrupts and bump the preempt-disable counter. Two-flavour
//! splits (Linux's `lock_irqsave` / `lock` distinction) can come
//! later if a perf-critical hot path proves it needs them; the
//! wrapper internals change, callers stay the same.
//!
//! See `plans/v0.5-pre-smp-sync-prefactor.md`.

#![allow(
    clippy::disallowed_types,
    reason = "kernel::sync is the sanctioned home for the underlying spin types"
)]

use core::ops::{Deref, DerefMut};

/// A mutual-exclusion primitive for kernel-internal data. Wraps
/// `spin::Mutex` today; future versions disable preemption and IRQs
/// across the critical section.
pub struct Mutex<T: ?Sized> {
    inner: spin::Mutex<T>,
}

/// RAII guard returned by [`Mutex::lock`]. Releases the lock and
/// restores preempt/IRQ state on drop.
pub struct MutexGuard<'a, T: ?Sized + 'a> {
    inner: spin::MutexGuard<'a, T>,
    // Future fields (one-flavour evolution path):
    //   prior_sie: bool        — value of sstatus.SIE before lock()
    //   _preempt: PreemptToken — bump+restore on a per-task counter
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self { inner: spin::Mutex::new(value) }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquire the lock. Currently a thin wrap of `spin::Mutex::lock`;
    /// future versions disable preemption and save+clear `sstatus.SIE`
    /// before returning the guard.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        // Acquisition hook stub. v0.5.x adds preempt::disable(); SMP
        // adds save+clear of sstatus.SIE here.
        MutexGuard { inner: self.inner.lock() }
    }

    /// Try to acquire the lock without blocking: the guard if it was free,
    /// `None` if it's held (by any hart, or by this hart mid-critical-section).
    /// The non-blocking seam the panic path needs — it must never spin on a lock
    /// the panicking code might already hold. Runs the same acquisition hooks as
    /// [`lock`](Self::lock), but only on the success path.
    // The first caller is the panic-safe telemetry emit (increment 3 of
    // plans/panic-emits-telemetry.md); this `allow` goes away when it lands.
    #[allow(dead_code, reason = "seam for the panic-safe virtio send, wired up next")]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        // Acquisition hook stub (success path only). Mirrors `lock`: v0.5.x adds
        // preempt::disable(); SMP adds save+clear of sstatus.SIE here.
        self.inner.try_lock().map(|inner| MutexGuard { inner })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        // Release hook stub. v0.5.x restores preempt counter; SMP
        // restores sstatus.SIE from `prior_sie`. Order: restore in
        // the reverse of acquisition.
    }
}

/// One-time initialisation cell for late-bound statics. Wraps
/// `spin::Once`. Hooks for preempt/IRQ discipline land here when
/// they land on `Mutex`.
pub struct Once<T> {
    inner: spin::Once<T>,
}

impl<T> Once<T> {
    pub const fn new() -> Self {
        Self { inner: spin::Once::new() }
    }

    /// Initialise the cell on first call; subsequent calls observe
    /// the already-initialised value without re-running `f`.
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) -> &T {
        self.inner.call_once(f)
    }

    /// Get a reference to the initialised value, or `None` if
    /// `call_once` has not yet completed.
    pub fn get(&self) -> Option<&T> {
        self.inner.get()
    }
}
