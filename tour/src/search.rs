//! Running a guest forward until its chapter's anchor arrives.
//!
//! The search is bounded **structurally** — a `for` over a fixed round count, not
//! a `while` waiting for a flag. A loop whose termination depends on a value is a
//! loop that can hang a browser tab, and post 82 found three of them in this
//! codebase by mutation testing. Here the bound is visible by reading it.
//!
//! [`Guest`] is the seam. The gate drives a snemu `Machine`; the browser drives the
//! same machine through wasm; a test drives a script. All three run this one search,
//! so "where the chapter stops" cannot differ between them.

use crate::anchor::Anchor;
use protocol::stream::OwnedFrame;

/// What the search needs from a guest: advance it, and read what it has said.
pub trait Guest {
    /// Advance by at most `instret` instructions.
    fn advance(&mut self, instret: u64);
    /// The guest's cumulative retired-instruction count.
    fn instret(&self) -> u64;
    /// Every telemetry frame decoded so far, oldest first.
    fn frames(&self) -> &[OwnedFrame];
}

/// How hard to look before giving up.
#[derive(Debug, Clone, Copy)]
pub struct Search {
    /// Instructions to advance between checks.
    pub chunk: u64,
    /// How many chunks to try before reporting the anchor unreachable.
    pub rounds: u32,
}

/// Why a search ended without reaching the anchor.
#[derive(Debug, PartialEq, Eq)]
pub struct NotReached {
    /// How far the guest got.
    pub instret: u64,
    /// How many frames it had emitted by then.
    pub frames: usize,
}

/// Run `guest` forward until `anchor` arrives.
///
/// Returns the guest's instret at the end of the round in which the anchor was
/// first seen. That is an *observation* point, not the exact instruction the frame
/// was emitted on — the contract a chapter asserts is the **frame**, and two
/// drivers with different chunk sizes agree about that even when their reported
/// instrets differ.
///
/// # Errors
/// [`NotReached`] when the anchor has not arrived within `search.rounds` chunks.
pub fn run_to_anchor(
    anchor: &Anchor,
    guest: &mut impl Guest,
    search: Search,
) -> Result<u64, NotReached> {
    for _ in 0..search.rounds {
        guest.advance(search.chunk);

        if anchor.find(guest.frames()).is_some() {
            return Ok(guest.instret());
        }
    }

    Err(NotReached { instret: guest.instret(), frames: guest.frames().len() })
}

#[cfg(test)]
mod tests {
    use super::{Guest, NotReached, Search, run_to_anchor};
    use crate::anchor::{Anchor, FrameMatch};
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject};

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

    /// A guest that reveals one scripted batch of frames per advance.
    struct ScriptedGuest {
        script: Vec<Vec<OwnedFrame>>,
        said: Vec<OwnedFrame>,
        instret: u64,
        advances: u32,
    }

    impl ScriptedGuest {
        fn new(script: Vec<Vec<OwnedFrame>>) -> Self {
            Self { script, said: Vec::new(), instret: 0, advances: 0 }
        }
    }

    impl Guest for ScriptedGuest {
        fn advance(&mut self, instret: u64) {
            if !self.script.is_empty() {
                self.said.append(&mut self.script.remove(0));
            }
            self.instret += instret;
            self.advances += 1;
        }

        fn instret(&self) -> u64 {
            self.instret
        }

        fn frames(&self) -> &[OwnedFrame] {
            &self.said
        }
    }

    fn endpoint_handover() -> Anchor {
        Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs.endpoint".to_owned(),
            },
            occurrence: 1,
        }
    }

    /// The search reports where the guest was when the anchor arrived.
    #[test]
    fn running_to_an_anchor_reports_the_instret_it_was_reached_at() {
        let mut guest = ScriptedGuest::new(vec![
            vec![],
            vec![cap_event(CapEventKind::Granted, "fs.endpoint")],
            vec![cap_event(CapEventKind::Transferred, "fs.endpoint")],
        ]);

        let reached = run_to_anchor(&endpoint_handover(), &mut guest, Search {
            chunk: 1_000,
            rounds: 10,
        });

        assert_eq!(reached, Ok(3_000), "found on the third chunk");
    }

    /// **An anchor that never arrives ends the search, and ends it on time.**
    ///
    /// The round count is asserted, not just the error: a bound that quietly ran
    /// one round longer — or ten — would still return `NotReached` and still look
    /// correct here. In a browser the difference between a bounded search and an
    /// unbounded one is a tab that stops and a tab that hangs.
    #[test]
    fn an_anchor_that_never_arrives_gives_up_after_exactly_the_rounds_allowed() {
        let mut guest = ScriptedGuest::new(vec![vec![cap_event(
            CapEventKind::Granted,
            "fs.endpoint",
        )]]);

        let reached =
            run_to_anchor(&endpoint_handover(), &mut guest, Search { chunk: 100, rounds: 4 });

        assert_eq!(reached, Err(NotReached { instret: 400, frames: 1 }));
        assert_eq!(guest.advances, 4, "the guest was advanced exactly four times");
    }
}
