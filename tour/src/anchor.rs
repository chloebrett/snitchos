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
use serde::{Deserialize, Deserializer};

/// Where a chapter's guest stops replaying.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum FrameMatch {
    /// A capability event of `kind` whose object is named `name`.
    ///
    /// `rights` narrows further when the same object reaches more than one holder
    /// with different authority — on an `init` boot the endpoint named `fs` is
    /// delegated as `RECV|MINT` to its server and as a minted `SEND` to its client.
    /// `None` matches any rights.
    CapEvent {
        kind: CapEventKind,
        name: String,
        #[serde(default, deserialize_with = "rights_by_name")]
        rights: Option<u32>,
    },
}

/// Read a manifest's `rights = ["RECV", "MINT"]` into the mask the wire carries.
fn rights_by_name<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Option::<Vec<String>>::deserialize(d)?
        .map(|names| parse_rights(&names).map_err(serde::de::Error::custom))
        .transpose()
}

/// The rights bit for each ABI name a manifest may use.
///
/// Spelled out rather than derived: `snitchos_abi::rights` is a module of
/// constants, not an enum, so there is nothing to iterate. The
/// [`every_right_in_the_abi_can_be_named`] test is what keeps this honest when a
/// new right is added.
const NAMED_RIGHTS: &[(&str, u32)] = &[
    ("EMIT", snitchos_abi::rights::EMIT),
    ("SEND", snitchos_abi::rights::SEND),
    ("RECV", snitchos_abi::rights::RECV),
    ("MINT", snitchos_abi::rights::MINT),
    ("SIGNAL", snitchos_abi::rights::SIGNAL),
    ("WAIT", snitchos_abi::rights::WAIT),
    ("KILL", snitchos_abi::rights::KILL),
    ("AUDIO", snitchos_abi::rights::AUDIO),
];

/// The rights mask named by `names`, e.g. `["RECV", "MINT"]`.
///
/// # Errors
/// If any name is not a right this ABI has. Refused rather than ignored: a
/// silently-dropped name would *widen* the match, and an anchor that matches more
/// than it says is how a chapter ends up describing the wrong moment.
pub fn parse_rights(names: &[String]) -> Result<u32, String> {
    names.iter().try_fold(0, |mask, name| {
        NAMED_RIGHTS
            .iter()
            .find(|(known, _)| *known == name.as_str())
            .map(|(_, bit)| mask | bit)
            .ok_or_else(|| format!("{name:?} is not a capability right"))
    })
}

impl FrameMatch {
    /// Whether `frame` is the kind of frame this predicate describes.
    #[must_use]
    pub fn matches_frame(&self, frame: &OwnedFrame) -> bool {
        match (self, frame) {
            (
                Self::CapEvent { kind, name, rights },
                OwnedFrame::CapEvent {
                    kind: got_kind, name: got_name, rights: got_rights, ..
                },
            ) => {
                got_kind == kind
                    && snitchos_abi::name_str(got_name) == name.as_str()
                    && rights.is_none_or(|wanted| wanted == *got_rights)
            }
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
            .filter(|(_, frame)| self.matches.matches_frame(frame))
            .nth(self.occurrence.checked_sub(1)?)
            .map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, FrameMatch, parse_rights};
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject};
    use snitchos_abi::rights;

    /// A cap event naming `name`, with everything a match should ignore left flat.
    fn cap_event(kind: CapEventKind, name: &str) -> OwnedFrame {
        cap_event_with_rights(kind, name, 0)
    }

    fn cap_event_with_rights(kind: CapEventKind, name: &str, rights: u32) -> OwnedFrame {
        OwnedFrame::CapEvent {
            kind,
            cap_id: 0,
            parent_cap_id: 0,
            holder: 0,
            object: CapObject::Endpoint,
            rights,
            badge: 0,
            t: 0,
            hart_id: 0,
            name: snitchos_abi::pack_name(name),
        }
    }

    /// **A chapter names rights, it does not spell them in binary.**
    ///
    /// `rights = ["RECV", "MINT"]` is the difference between a manifest a reader of
    /// the chapter can check and one only its author can. The names are the ABI's
    /// own (`snitchos_abi::rights`), so a renamed right breaks the manifest rather
    /// than silently meaning something else.
    #[test]
    fn rights_are_named_in_a_manifest_not_numbered() {
        assert_eq!(parse_rights(&["RECV".to_owned(), "MINT".to_owned()]), Ok(rights::RECV | rights::MINT));
    }

    /// **Naming a right twice is the same as naming it once.**
    ///
    /// Rights accumulate by union, and the difference between union and
    /// exclusive-or is invisible until a name repeats — at which point `^` would
    /// *cancel* the right and produce an empty mask, matching a capability with no
    /// authority at all. Every other test here passes under either operator, which
    /// is exactly why this one exists.
    #[test]
    fn naming_a_right_twice_is_the_same_as_naming_it_once() {
        assert_eq!(
            parse_rights(&["RECV".to_owned(), "RECV".to_owned()]),
            Ok(rights::RECV),
            "rights accumulate, they do not cancel"
        );
    }

    /// An unknown right is refused, and the error says which — a silently-ignored
    /// name would widen the match instead of narrowing it.
    #[test]
    fn an_unknown_right_is_refused_by_name() {
        let error = parse_rights(&["WRITE".to_owned()]).expect_err("no such right");
        assert!(error.contains("WRITE"), "the error should name it; got: {error}");
    }

    /// **Every right the ABI defines can be named in a manifest.**
    ///
    /// `snitchos_abi::rights` is a module of constants, so there is nothing to
    /// iterate and nothing to make the table exhaustive by construction. Read the
    /// ABI's own source instead — the same trick `kernel_boot::bootargs` uses to
    /// keep its variants sorted. Without this, adding a right leaves it unnameable
    /// and the failure only shows up as a manifest that mysteriously will not parse.
    #[test]
    fn every_right_in_the_abi_can_be_named() {
        let abi = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../abi/src/lib.rs"))
            .expect("the abi source is a sibling of this crate");
        let rights_module =
            abi.split("pub mod rights {").nth(1).expect("`mod rights` exists in the abi");

        let declared: Vec<&str> = rights_module
            .lines()
            .take_while(|line| !line.starts_with('}'))
            .filter_map(|line| line.trim().strip_prefix("pub const "))
            .filter_map(|rest| rest.split(':').next())
            .collect();

        assert!(!declared.is_empty(), "the scrape found nothing — has the abi moved?");
        for name in declared {
            assert!(
                super::parse_rights(&[name.to_owned()]).is_ok(),
                "the abi defines {name}, but a manifest cannot name it"
            );
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
                rights: None,
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
                rights: None,
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
                rights: None,
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
                rights: None,
            },
            occurrence: 1,
        };

        assert_eq!(anchor.find(&frames), Some(1));
    }

    /// **The same capability reaches two holders with different authority.**
    ///
    /// On an `init` boot the endpoint named `fs` is delegated twice: `RECV|MINT`
    /// to the server, and a minted `SEND` to the client. Both are `Transferred`
    /// and both are named `fs`, so kind and name alone cannot say which moment a
    /// chapter means — and picking by position would break the first time `init`
    /// spawns its children in the other order. Rights are what actually
    /// distinguishes them, and they are what the chapter is about.
    #[test]
    fn an_anchor_can_distinguish_two_handovers_of_the_same_capability_by_rights() {
        let frames = vec![
            cap_event_with_rights(CapEventKind::Transferred, "fs", rights::SEND),
            cap_event_with_rights(CapEventKind::Transferred, "fs", rights::RECV | rights::MINT),
        ];

        let anchor = Anchor {
            matches: FrameMatch::CapEvent {
                kind: CapEventKind::Transferred,
                name: "fs".to_owned(),
                rights: Some(rights::RECV | rights::MINT),
            },
            occurrence: 1,
        };

        assert_eq!(anchor.find(&frames), Some(1), "the server's handover, not the client's");
    }
}
