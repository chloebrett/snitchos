//! Loom model-check of the virtio TX staging critical section.
//!
//! This is the deterministic regression for the cross-hart bug in
//! `kernel::virtio_console::send` (see plans/tx-staging-cross-hart-race.md):
//! the original `let base = *handle.lock();` copied `base` out and dropped
//! the `CONSOLE` guard at the `;`, leaving the shared `TX_STAGING` buffer
//! unprotected while two harts emitted concurrently. It reproduced only
//! ~2% of the time under real threads; loom finds it on every run by
//! exhaustively exploring the interleavings.
//!
//! The whole file is `#![cfg(loom)]`, so a normal `cargo test` compiles it
//! to nothing. Run it with:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test -p kernel-devices --test loom_tx
//! ```
//!
//! Two models, asserting opposite outcomes (a detector-liveness check —
//! if the harness silently stopped detecting the race, the buggy-twin
//! test would start passing and trip its `assert!`):
//!
//! - `correct_model` — buffer lives *inside* the mutex (today's
//!   `Mutex<TxStaging>`), driven through the real
//!   `kernel_core::virtio::stage_and_emit`. Race-free across all
//!   interleavings.
//! - `buggy_model` — mirrors the *pre-fix* architecture: a `Mutex` that
//!   guards only `base`, plus the staging buffer in a separate
//!   `UnsafeCell` reachable without the lock, with the guard dropped
//!   early. Loom finds the concurrent unsynchronised access.
//!
//! The two models differ in exactly one dimension — whether the lock
//! covers the buffer — so the green/red split is unambiguous.

#![cfg(loom)]

use loom::cell::UnsafeCell;
use loom::sync::{Arc, Mutex};
use loom::thread;

/// Staging buffer size + payload length. Kept tiny: loom's state space
/// is exponential in the number of modelled operations.
const BUF: usize = 2;

/// The two concurrent senders, each staging a buffer full of its own id.
const SENDERS: [u8; 2] = [0xAA, 0xBB];

/// Correct: the buffer is guarded by the mutex, and `stage_and_emit`
/// runs with the guard held — so the staged bytes the emit observes are
/// always this sender's own. Loom proves this holds for every interleaving.
fn correct_model() {
    loom::model(|| {
        let console: Arc<Mutex<[u8; BUF]>> = Arc::new(Mutex::new([0u8; BUF]));
        let handles: Vec<_> = SENDERS
            .iter()
            .map(|&id| {
                let console = Arc::clone(&console);
                thread::spawn(move || {
                    let mut buf = console.lock().unwrap();
                    let payload = [id; BUF];
                    kernel_devices::virtio::stage_and_emit(&mut buf[..], &payload, |staged| {
                        // The buffer is exclusively ours for the whole call.
                        assert!(staged.iter().all(|&b| b == id), "staged bytes were corrupted");
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

/// Buggy twin: the lock guards only `base`; the staging buffer lives
/// outside it in an `UnsafeCell` and is touched after the guard drops.
/// Two senders then race the buffer with no happens-before between them —
/// the exact shape of the original bug. Loom flags the concurrent access.
fn buggy_model() {
    loom::model(|| {
        let base: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let buf: Arc<UnsafeCell<[u8; BUF]>> = Arc::new(UnsafeCell::new([0u8; BUF]));
        let handles: Vec<_> = SENDERS
            .iter()
            .map(|&id| {
                let base = Arc::clone(&base);
                let buf = Arc::clone(&buf);
                thread::spawn(move || {
                    // The footgun: lock, copy `base` out, drop the guard...
                    let g = base.lock().unwrap();
                    let _base = *g;
                    drop(g);
                    // ...then stage + emit with NO lock protecting the buffer.
                    buf.with_mut(|p| {
                        let p = unsafe { &mut *p };
                        for slot in p.iter_mut() {
                            *slot = id;
                        }
                    });
                    buf.with(|p| {
                        let p = unsafe { &*p };
                        assert!(p.iter().all(|&b| b == id), "staged bytes were corrupted");
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

#[test]
fn correct_primitive_is_race_free_under_loom() {
    correct_model();
}

#[test]
fn buggy_twin_is_caught_by_loom() {
    // Loom panics when it finds the violation; assert it does. If this
    // test ever *passes the inner model* (no panic), the detector has
    // rotted and this assertion fails loudly.
    let outcome = std::panic::catch_unwind(buggy_model);
    assert!(outcome.is_err(), "loom failed to catch the buggy twin's data race");
}
