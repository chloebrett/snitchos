//! Checking a chapter against the machine it describes.
//!
//! Pure: frames in, verdict out. The booting lives in `xtask-itest`, which has
//! snemu; the *judgement* lives here, where it is host-tested and mutation-tested
//! and where the browser could run it too.
//!
//! Claims are checked over the stream **up to and including the anchor**. A
//! chapter describes a moment, so its claims are about that moment — and an
//! absence claim ("never handed to anyone with send and receive together") means
//! never *by then*, which is a statement a bounded run can actually settle.

use crate::{Chapter, Claim};
use protocol::stream::OwnedFrame;

/// A chapter that does not describe the machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The guest never reached the world-state the chapter is about.
    AnchorNeverReached { chapter: String },
    /// A claim the chapter makes is not true there.
    ClaimBroken { chapter: String, says: String, expected: bool },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AnchorNeverReached { chapter } => write!(
                f,
                "chapter {chapter:?}: the guest never reached its anchor, so the page \
                 would show a machine the prose does not describe"
            ),
            Self::ClaimBroken { chapter, says, expected } => {
                let what = if *expected { "did not happen" } else { "happened anyway" };
                write!(f, "chapter {chapter:?} claims {says:?} — but it {what}")
            }
        }
    }
}

/// Check `chapter` against a boot's decoded frames.
///
/// # Errors
/// Every failure found, not just the first — a run that reports one broken claim
/// per boot turns a five-minute fix into five boots.
pub fn check(chapter: &Chapter, frames: &[OwnedFrame]) -> Result<(), Vec<Failure>> {
    let Some(anchor) = chapter.anchor.find(frames) else {
        return Err(vec![Failure::AnchorNeverReached { chapter: chapter.slug.clone() }]);
    };

    let upto = &frames[..=anchor];
    let failures: Vec<Failure> = chapter
        .claims
        .iter()
        .filter(|claim| !holds(claim, upto))
        .map(|claim| Failure::ClaimBroken {
            chapter: chapter.slug.clone(),
            says: claim.says.clone(),
            expected: claim.present,
        })
        .collect();

    if failures.is_empty() { Ok(()) } else { Err(failures) }
}

/// Whether the stream agrees with what the claim says about it.
fn holds(claim: &Claim, frames: &[OwnedFrame]) -> bool {
    let found = frames.iter().any(|frame| claim.frame.matches_frame(frame));
    found == claim.present
}

#[cfg(test)]
mod tests {
    use super::{Failure, check};
    use crate::Chapter;
    use protocol::stream::OwnedFrame;
    use protocol::{CapEventKind, CapObject};
    use snitchos_abi::rights;

    fn cap_event(kind: CapEventKind, name: &str, rights: u32) -> OwnedFrame {
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

    /// A chapter anchored on the server's handover, claiming the client only sends.
    fn chapter() -> Chapter {
        Chapter::parse(
            r#"
            slug = "capabilities"
            title = "Who is allowed to do what"
            workload = "init"
            body = "capabilities.mdx"

            [anchor]
            occurrence = 1

            [anchor.matches]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["RECV", "MINT"]

            [[claims]]
            says = "the client is handed send"

            [claims.frame]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["SEND"]

            [[claims]]
            says = "nobody is handed send and receive together"
            present = false

            [claims.frame]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["SEND", "RECV"]
            "#,
        )
        .expect("the fixture chapter parses")
    }

    /// A boot that matches the chapter passes.
    #[test]
    fn a_chapter_that_describes_the_machine_holds() {
        let frames = vec![
            cap_event(CapEventKind::Transferred, "fs", rights::SEND),
            cap_event(CapEventKind::Transferred, "fs", rights::RECV | rights::MINT),
        ];

        assert_eq!(check(&chapter(), &frames), Ok(()));
    }

    /// **A claim that stops being true names itself.**
    ///
    /// The message is the product here. "A chapter failed" would send someone
    /// reading three files; the sentence the prose actually makes is what tells
    /// them which paragraph the kernel just falsified.
    #[test]
    fn a_broken_claim_names_the_chapter_and_the_sentence() {
        let frames = vec![cap_event(CapEventKind::Transferred, "fs", rights::RECV | rights::MINT)];

        let failures = check(&chapter(), &frames).expect_err("the client never got its cap");

        assert_eq!(failures, vec![Failure::ClaimBroken {
            chapter: "capabilities".to_owned(),
            says: "the client is handed send".to_owned(),
            expected: true,
        }]);
        assert!(
            failures[0].to_string().contains("the client is handed send"),
            "the message carries the sentence: {}",
            failures[0]
        );
    }

    /// An absence claim fails when the thing happens.
    #[test]
    fn an_absence_claim_fails_when_the_forbidden_handover_happens() {
        let frames = vec![
            cap_event(CapEventKind::Transferred, "fs", rights::SEND),
            cap_event(CapEventKind::Transferred, "fs", rights::SEND | rights::RECV),
            cap_event(CapEventKind::Transferred, "fs", rights::RECV | rights::MINT),
        ];

        let failures = check(&chapter(), &frames).expect_err("send+receive was handed out");

        assert_eq!(failures, vec![Failure::ClaimBroken {
            chapter: "capabilities".to_owned(),
            says: "nobody is handed send and receive together".to_owned(),
            expected: false,
        }]);
    }

    /// **A guest that never reaches the anchor is a failure, not a pass.**
    ///
    /// This is the one that would otherwise rot silently. If the anchor is never
    /// reached the claim prefix is the whole (wrong) stream, and an absence claim
    /// over frames that were never emitted passes *vacuously* — a chapter
    /// describing a moment the machine no longer has would report itself healthy.
    #[test]
    fn a_guest_that_never_reaches_the_anchor_fails_rather_than_passing_vacuously() {
        let frames = vec![cap_event(CapEventKind::Granted, "fs", rights::RECV | rights::MINT)];

        let failures = check(&chapter(), &frames).expect_err("the anchor never arrived");

        assert_eq!(failures, vec![Failure::AnchorNeverReached {
            chapter: "capabilities".to_owned()
        }]);
    }
}
