//! Keeping the frames the folds need, and only those.
//!
//! The panels reconstruct their views by folding a *slice* of frames
//! (`diagram::caps::derivation_tree` and friends), so something has to retain them.
//! A live tab cannot retain a whole boot, so something has to drop them too — and
//! **the obvious policy is wrong in a way that does not announce itself.**
//!
//! `derivation_tree` walks its slice three times, and each pass depends on frames
//! that arrived during boot:
//!
//! - `thread_names` scans for `ThreadRegister` to label holders. Drop those and every
//!   holder renders as `h3` instead of `stitch_repl` — degraded, still plausible.
//! - A `revoked` set is built by scanning for `CapEvent::Revoked`. Drop an old
//!   revocation and a **revoked capability renders as live**. Not degraded: wrong,
//!   in the direction that matters most for a capability view.
//! - Roots are `parent_cap_id == 0`. Drop a parent while a child survives and the
//!   child points at a node that no longer exists.
//!
//! None of those raise an error. So retention is split by what a frame *means*:
//! registrations and lifecycle events are cumulative facts and are kept; the rest is
//! a stream, and a recent window is what you actually want to look at.

use protocol::stream::OwnedFrame;
use protocol::CapEventKind;
use std::collections::VecDeque;

/// How long a frame is worth keeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// A cumulative fact. Dropping it changes what the *remaining* frames mean.
    Durable,
    /// Part of a stream. The views built from it are about the recent past.
    Windowed,
}

/// Classify a frame for retention.
///
/// Both matches are **exhaustive on purpose**, with no catch-all arm. The wire
/// reserves `CapEventKind::Invoked` / `Denied` for audit events, which would be
/// unbounded in volume and structurally meaningless — the opposite classification
/// from every kind that exists today. A catch-all would file them as `Durable` on
/// arrival and leak memory in a way nothing would report; a missing arm is a build
/// failure and a decision someone has to make.
#[must_use]
pub fn retention_of(frame: &OwnedFrame) -> Retention {
    match frame {
        // Registrations: emitted once each, and every later view reads them.
        OwnedFrame::Hello { .. }
        | OwnedFrame::StringRegister { .. }
        | OwnedFrame::MetricRegister { .. }
        | OwnedFrame::ThreadRegister { .. }
        | OwnedFrame::HartRegister { .. }
        | OwnedFrame::BuildInfo { .. } => Retention::Durable,

        // Capability lifecycle: the derivation tree is cumulative state.
        OwnedFrame::CapEvent { kind, .. } => match kind {
            CapEventKind::Granted
            | CapEventKind::Transferred
            | CapEventKind::Revoked
            | CapEventKind::Minted => Retention::Durable,
        },

        // The stream.
        OwnedFrame::SpanStart { .. }
        | OwnedFrame::SpanEnd { .. }
        | OwnedFrame::Event { .. }
        | OwnedFrame::Metric { .. }
        | OwnedFrame::Dropped { .. }
        | OwnedFrame::ContextSwitch { .. }
        | OwnedFrame::SyscallRefused { .. }
        | OwnedFrame::Log { .. }
        | OwnedFrame::Message { .. }
        | OwnedFrame::NotifySignal { .. }
        | OwnedFrame::NotifyWait { .. }
        | OwnedFrame::AudioXRun { .. } => Retention::Windowed,
    }
}

/// Frames retained for the folds, under the policy in [`retention_of`].
///
/// Both buckets record an arrival sequence and [`frames`](Self::frames) merges on it,
/// rather than concatenating durable-then-windowed. That is not fastidiousness: the
/// span and switch folds read *sequences*, and a registration emitted mid-run would
/// otherwise appear to have happened before everything that preceded it.
#[derive(Debug, Default)]
pub struct FrameStore {
    durable: Vec<(u64, OwnedFrame)>,
    window: VecDeque<(u64, OwnedFrame)>,
    window_cap: usize,
    next_seq: u64,
}

impl FrameStore {
    /// A store keeping at most `window_cap` stream frames, and every durable one.
    #[must_use]
    pub fn new(window_cap: usize) -> Self {
        Self { durable: Vec::new(), window: VecDeque::new(), window_cap, next_seq: 0 }
    }

    /// Retain `frame` under the policy.
    pub fn push(&mut self, frame: OwnedFrame) {
        let seq = self.next_seq;
        self.next_seq += 1;

        match retention_of(&frame) {
            Retention::Durable => self.durable.push((seq, frame)),
            Retention::Windowed => {
                self.window.push_back((seq, frame));
                // `if`, not `while`: this call adds exactly one frame and the cap is
                // fixed at construction, so at most one can ever need evicting. A
                // loop here would be termination that depends on the comparison being
                // right — and mutating `>` to `<` or `>=` spins forever, which in a
                // browser is a dead tab rather than a failed assertion. Third time
                // this shape has come up in this work; state the bound structurally.
                if self.window.len() > self.window_cap {
                    self.window.pop_front();
                }
            }
        }
    }

    /// Everything retained, in arrival order, ready to fold.
    #[must_use]
    pub fn frames(&self) -> Vec<OwnedFrame> {
        let mut merged: Vec<&(u64, OwnedFrame)> =
            self.durable.iter().chain(self.window.iter()).collect();
        merged.sort_by_key(|(seq, _)| *seq);
        merged.into_iter().map(|(_, frame)| frame.clone()).collect()
    }

    /// How many cumulative facts are held. Unbounded by design, so worth being able
    /// to watch: "bounded in practice" is an assumption about guest behaviour.
    #[must_use]
    pub fn durable_len(&self) -> usize {
        self.durable.len()
    }

    /// How many stream frames are held.
    #[must_use]
    pub fn windowed_len(&self) -> usize {
        self.window.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameStore, Retention, retention_of};
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject, SpanId, StringId};

    /// A capability event. `parent_cap_id` matters more than it looks: the fold
    /// **drops any node that ends up in no edge** (an isolated bootstrap grant), so a
    /// fixture where everything has parent 0 folds to an empty graph.
    fn cap_event(kind: CapEventKind, cap_id: u64, parent_cap_id: u64) -> OwnedFrame {
        OwnedFrame::CapEvent {
            kind,
            cap_id,
            parent_cap_id,
            holder: 1,
            object: CapObject::Endpoint,
            rights: 0,
            badge: 0,
            t: 0,
            hart_id: 0,
            name: [0; snitchos_abi::CAP_NAME_LEN],
        }
    }

    fn span(id: u64) -> OwnedFrame {
        OwnedFrame::SpanStart {
            id: SpanId(id),
            parent: SpanId(0),
            name_id: StringId(1),
            t: id,
            task_id: 0,
            hart_id: 0,
        }
    }

    fn thread(id: u32, name: &str) -> OwnedFrame {
        OwnedFrame::ThreadRegister { id, name: name.to_string(), priority: 0 }
    }

    // --- the policy ------------------------------------------------------------

    /// Registrations are cumulative facts: a name that stops resolving makes every
    /// later view unreadable.
    #[test]
    fn registrations_are_durable() {
        for frame in [
            OwnedFrame::StringRegister { id: StringId(1), value: "n".into() },
            thread(1, "init"),
            OwnedFrame::HartRegister { id: 0, mhartid: 0, role: protocol::HartRole::Boot },
            OwnedFrame::BuildInfo { kernel_profile: "r".into(), userspace_opt: "3".into() },
        ] {
            assert_eq!(retention_of(&frame), Retention::Durable, "{frame:?}");
        }
    }

    /// Every capability event is derivation lifecycle, so all of them are kept.
    ///
    /// The wire reserves `Invoked`/`Denied` slots for *audit* events, which would be
    /// unbounded and structurally meaningless — those would be windowed. They do not
    /// exist yet, and `retention_of` matches `CapEventKind` exhaustively precisely so
    /// that adding one is a compile error rather than a silent default into the
    /// unbounded bucket.
    #[test]
    fn every_capability_lifecycle_event_is_durable() {
        for kind in [
            CapEventKind::Granted,
            CapEventKind::Transferred,
            CapEventKind::Revoked,
            CapEventKind::Minted,
        ] {
            assert_eq!(retention_of(&cap_event(kind, 1, 0)), Retention::Durable, "{kind:?}");
        }
    }

    /// Spans, switches and logs are a stream — high-volume, and the views built from
    /// them are *about* the recent past.
    #[test]
    fn the_event_stream_is_windowed() {
        for frame in [
            span(1),
            OwnedFrame::SpanEnd { id: SpanId(1), t: 1 },
            OwnedFrame::Log { msg: "x".into(), task_id: 0, t: 0, hart_id: 0 },
            OwnedFrame::Metric { name_id: StringId(1), value: 1, t: 0, hart_id: 0 },
        ] {
            assert_eq!(retention_of(&frame), Retention::Windowed, "{frame:?}");
        }
    }

    // --- the store -------------------------------------------------------------

    #[test]
    fn frames_come_back_in_arrival_order() {
        let mut store = FrameStore::new(10);
        // Distinct ids across the two frame kinds: `discriminant_key` reads whichever
        // id a frame carries, so reusing 1 for both would compare a collision.
        store.push(thread(0, "init"));
        store.push(span(1));
        store.push(span(2));

        let ids: Vec<u64> = store.frames().iter().map(discriminant_key).collect();
        assert_eq!(ids, vec![0, 1, 2]);
    }

    /// Interleaved buckets still come back interleaved. Storing durable and windowed
    /// separately and concatenating would reorder the stream, and the switch and span
    /// folds read sequences.
    #[test]
    fn a_durable_frame_arriving_late_stays_late() {
        let mut store = FrameStore::new(10);
        store.push(span(1));
        store.push(thread(9, "late"));
        store.push(span(2));

        let ids: Vec<u64> = store.frames().iter().map(discriminant_key).collect();
        assert_eq!(ids, vec![1, 9, 2], "the registration must not jump to the front");
    }

    #[test]
    fn the_window_drops_the_oldest_first() {
        let mut store = FrameStore::new(2);
        store.push(span(1));
        store.push(span(2));
        store.push(span(3));

        let ids: Vec<u64> = store.frames().iter().map(discriminant_key).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    /// **The failure this module exists to prevent.** A registration from before an
    /// overflow must still be there, or every holder in the cap tree loses its name.
    #[test]
    fn a_registration_survives_an_overflow_that_discards_the_stream() {
        let mut store = FrameStore::new(4);
        store.push(thread(7, "stitch_repl"));
        for i in 0..100 {
            store.push(span(i));
        }

        let named = store.frames().iter().any(|f| {
            matches!(f, OwnedFrame::ThreadRegister { name, .. } if name == "stitch_repl")
        });
        assert!(named, "the holder's name was dropped, so the tree would render `h7`");
    }

    /// **And the worse one.** A revocation from before an overflow must survive, or a
    /// revoked capability renders as live.
    #[test]
    fn a_revocation_survives_an_overflow() {
        let mut store = FrameStore::new(4);
        store.push(cap_event(CapEventKind::Granted, 1, 0));
        store.push(cap_event(CapEventKind::Revoked, 1, 0));
        for i in 0..100 {
            store.push(span(i));
        }

        let revoked = store.frames().iter().any(
            |f| matches!(f, OwnedFrame::CapEvent { kind: CapEventKind::Revoked, .. }),
        );
        assert!(revoked, "a revoked capability would render as live");
    }

    /// The durable bucket is unbounded by design, so its size is worth being able to
    /// see — "bounded in practice" is an assumption about guest behaviour, and this
    /// project has been wrong about guest behaviour before.
    ///
    /// Counts deliberately differ from each other and from one: an earlier version
    /// asserted `durable_len() == 1`, which a function returning the constant `1`
    /// satisfies perfectly. Mutation testing found that, and it is the same
    /// cannot-discriminate shape that keeps recurring here.
    #[test]
    fn the_store_reports_how_much_it_is_keeping() {
        let mut store = FrameStore::new(4);
        store.push(thread(1, "a"));
        store.push(thread(2, "b"));
        store.push(thread(3, "c"));
        store.push(span(1));
        store.push(span(2));

        assert_eq!(store.durable_len(), 3);
        assert_eq!(store.windowed_len(), 2);

        // And both move as more arrives, so neither can be a constant.
        store.push(thread(4, "d"));
        store.push(span(3));
        assert_eq!(store.durable_len(), 4);
        assert_eq!(store.windowed_len(), 3);
    }

    #[test]
    fn an_empty_store_folds_to_nothing() {
        assert!(FrameStore::new(4).frames().is_empty());
    }

    /// A zero-length window keeps registrations and nothing else — the degenerate
    /// case a caller might reach for to mean "structure only".
    #[test]
    fn a_zero_window_keeps_only_the_durable_frames() {
        let mut store = FrameStore::new(0);
        store.push(thread(1, "a"));
        store.push(span(1));

        assert_eq!(store.frames().len(), 1);
        assert_eq!(store.durable_len(), 1);
    }

    /// **The claim, checked against the real fold rather than a proxy.**
    ///
    /// Every test above asserts a frame is *present*. That is one inference short of
    /// the thing that matters, which is whether `derivation_tree` still produces a
    /// true tree after the window has overflowed. Here the fold itself is the oracle:
    /// a store that has discarded thousands of stream frames must produce the same
    /// graph as one that discarded nothing.
    #[test]
    fn the_folded_tree_survives_an_overflow_that_discards_the_stream() {
        let mut lossless = FrameStore::new(100_000);
        let mut lossy = FrameStore::new(4);

        let history = [
            // id 1 to match `cap_event`'s `holder`, or no name resolves and the
            // comparison below is between two equally-nameless trees.
            thread(1, "stitch_repl"),
            // A root and a capability derived from it, so the pair forms an edge and
            // survives the fold's isolated-node filter.
            cap_event(CapEventKind::Granted, 1, 0),
            cap_event(CapEventKind::Transferred, 2, 1),
            cap_event(CapEventKind::Revoked, 2, 1),
        ];
        for frame in &history {
            lossless.push(frame.clone());
            lossy.push(frame.clone());
        }
        // Enough stream traffic to evict the window many times over.
        for i in 0..500 {
            lossless.push(span(i));
            lossy.push(span(i));
        }

        let expected = diagram::caps::derivation_tree(&lossless.frames()).to_json();
        let got = diagram::caps::derivation_tree(&lossy.frames()).to_json();

        assert_eq!(got, expected, "the cap tree changed when the stream was evicted");
        // And the tree is not vacuously equal because both are empty.
        assert!(expected.contains("stitch_repl"), "holder named from the registration");
        assert!(expected.contains("revoked"), "revocation still marks its cap");
    }

    /// Identify a frame in these tests by the number it was built with.
    fn discriminant_key(frame: &OwnedFrame) -> u64 {
        match frame {
            OwnedFrame::SpanStart { id, .. } => id.0,
            OwnedFrame::ThreadRegister { id, .. } => u64::from(*id),
            _ => u64::MAX,
        }
    }
}
