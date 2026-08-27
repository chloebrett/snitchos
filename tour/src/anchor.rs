//! Where a chapter's guest stops.
//!
//! An anchor is a **predicate over the decoded frame stream**, never an instret
//! count. A count is invalidated by every kernel rebuild; a predicate re-finds
//! itself, which is what makes a chapter survive a change to the kernel it
//! describes. Instret is a cache on top of this — run to the remembered number,
//! check the predicate holds there, scan forward if it does not.
//!
//! This is the same contract the itest scenarios use: assert on the decoded wire
//! frames, not on where they landed.

use protocol::CapEventKind;
use protocol::stream::OwnedFrame;

/// Where a chapter's guest stops replaying.
#[derive(Debug, Clone)]
pub struct Anchor {
    /// Which frame ends the replay.
    pub matches: FrameMatch,
    /// Which occurrence of that frame, counting from one.
    pub occurrence: usize,
}

/// What makes a frame the one a chapter is waiting for.
///
/// Deliberately minimal — one variant, enough for the chapter that exists. The
/// vocabulary grows the way `WorkloadKind` does: additively, when a chapter needs
/// to say something it currently cannot.
#[derive(Debug, Clone)]
pub enum FrameMatch {
    /// A capability event of `kind` whose object is named `name`.
    CapEvent { kind: CapEventKind, name: String },
}

impl FrameMatch {
    fn matches(&self, frame: &OwnedFrame) -> bool {
        match (self, frame) {
            (
                Self::CapEvent { kind, name },
                OwnedFrame::CapEvent { kind: got_kind, name: got_name, .. },
            ) => got_kind == kind && snitchos_abi::name_str(got_name) == name.as_str(),
            _ => false,
        }
    }
}

impl Anchor {
    /// The index of the frame this anchor stops at, or `None` if the stream never
    /// reaches it.
    pub fn find(&self, frames: &[OwnedFrame]) -> Option<usize> {
        frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| self.matches.matches(frame))
            .nth(self.occurrence.checked_sub(1)?)
            .map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, FrameMatch};
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject};

    /// A cap event naming `name`, with everything a match should ignore left flat.
    fn cap_event(kind: CapEventKind, name: &str) -> OwnedFrame {
        OwnedFrame::CapEvent {
            kind,
            cap_id: 0,
            parent_cap_id: 0,
            holder: 0,
            object: CapObject::Endpoint,
            rights: 0,
            badge: 0,
            t: 0,
            hart_id: 0,
            name: snitchos_abi::pack_name(name),
        }
    }

    /// A frame that is not a cap event, to prove the search skips rather than counts.
    fn other() -> OwnedFrame {
        OwnedFrame::Dropped { count: 0 }
    }

    /// The chapter's anchor: the moment a named capability changes hands.
    #[test]
    fn an_anchor_finds_the_frame_that_hands_over_the_named_capability() {
        let frames = vec![
            other(),
            cap_event(CapEventKind::Granted, "fs.endpoint"),
            cap_event(CapEventKind::Transferred, "fs.endpoint"),
        ];

        let anchor = Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs.endpoint".to_owned(),
            },
            occurrence: 1,
        };

        assert_eq!(anchor.find(&frames), Some(2));
    }

    /// **`occurrence` counts matches, not frames.**
    ///
    /// A chapter that wants the *second* time a capability changes hands must not
    /// be handed the first. The two are different world-states, and the prose
    /// describes exactly one of them — stopping early would leave the reader
    /// looking at a machine the chapter does not describe.
    #[test]
    fn an_anchor_waiting_for_a_later_occurrence_does_not_stop_at_an_earlier_one() {
        let frames = vec![
            cap_event(CapEventKind::Transferred, "fs.endpoint"),
            other(),
            cap_event(CapEventKind::Transferred, "fs.endpoint"),
        ];

        let anchor = Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs.endpoint".to_owned(),
            },
            occurrence: 2,
        };

        assert_eq!(anchor.find(&frames), Some(2));
    }

    /// A stream that never reaches the anchor yields nothing — not its last frame.
    ///
    /// The distinction is load-bearing at the call site. `None` means "keep
    /// running"; a nearest-frame fallback would anchor the chapter to whatever
    /// happened to arrive last, stopping the guest somewhere plausible and wrong.
    #[test]
    fn a_stream_that_never_reaches_the_anchor_yields_nothing() {
        let frames = vec![other(), cap_event(CapEventKind::Granted, "fs.endpoint")];

        let anchor = Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs.endpoint".to_owned(),
            },
            occurrence: 1,
        };

        assert_eq!(anchor.find(&frames), None);
    }

    /// **The anchor discriminates on the name, not just the kind.**
    ///
    /// Written by hand rather than by cargo-mutants, which cannot see this gap: it
    /// mutates `==` into `!=`, never into `true`, so a predicate that ignored the
    /// name would survive every other test here. Two chapters anchoring on
    /// different capabilities of the same kind would then both stop at whichever
    /// changed hands first.
    #[test]
    fn a_cap_event_of_the_right_kind_but_the_wrong_name_is_not_the_anchor() {
        let frames = vec![
            cap_event(CapEventKind::Transferred, "telemetry"),
            cap_event(CapEventKind::Transferred, "fs.endpoint"),
        ];

        let anchor = Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs.endpoint".to_owned(),
            },
            occurrence: 1,
        };

        assert_eq!(anchor.find(&frames), Some(1));
    }
}
