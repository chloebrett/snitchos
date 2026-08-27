//! What a tour chapter declares, and what makes one valid.
//!
//! A chapter is **data**: the workload its guest boots, the anchor it replays to,
//! and the claims its prose makes. This crate owns that schema and nothing else —
//! no snemu, no MMIO, no I/O. It is host-tested and also linked into `snemu-wasm`,
//! so the browser stops at the same frame the gate asserts at. A second definition
//! of "the anchor" in TypeScript would be free to drift from this one.
//!
//! See [`../../plans/tour-v1.md`](../../plans/tour-v1.md) for the design and the
//! decisions behind it.

pub mod anchor;
pub mod search;

use anchor::{Anchor, FrameMatch};
use serde::Deserialize;

/// One chapter: prose beside a guest booted to the world-state it describes.
#[derive(Debug, Deserialize)]
pub struct Chapter {
    /// The chapter's URL path segment.
    pub slug: String,
    /// The heading a reader sees.
    pub title: String,
    /// The `workload=` bootarg its guest boots.
    pub workload: String,
    /// Where the guest stops — the world-state the prose describes.
    pub anchor: Anchor,
    /// What the prose asserts is true there. Empty is allowed: a chapter may
    /// describe without asserting, and an empty list is honest about checking
    /// nothing rather than a check that passes vacuously.
    #[serde(default)]
    pub claims: Vec<Claim>,
}

/// One assertion a chapter makes about the world at its anchor.
#[derive(Debug, Deserialize)]
pub struct Claim {
    /// The sentence a reader sees. Prose and predicate can disagree and no tool
    /// can tell — that part stays the author's responsibility. What the gate
    /// guarantees is that `frame` still holds.
    pub says: String,
    /// The frame the claim is about.
    pub frame: FrameMatch,
    /// Whether that frame must appear in the stream, or must not.
    ///
    /// Absence is the capability-shaped assertion: "the client is never handed the
    /// right to receive" is the interesting thing to say about a delegation, and
    /// there is no positive frame that says it.
    #[serde(default = "yes")]
    pub present: bool,
}

const fn yes() -> bool {
    true
}

impl Chapter {
    /// Parse a chapter declaration, rejecting one the guest could not honour.
    pub fn parse(declaration: &str) -> Result<Self, ParseError> {
        let chapter: Self = toml::from_str(declaration).map_err(ParseError::Malformed)?;

        if kernel_boot::bootargs::select(&format!("workload={}", chapter.workload)).is_none() {
            return Err(ParseError::UnknownWorkload(chapter.workload));
        }

        Ok(chapter)
    }
}

/// Why a chapter declaration was rejected.
#[derive(Debug)]
pub enum ParseError {
    /// The declaration is not well-formed TOML, or is missing a field.
    Malformed(toml::de::Error),
    /// The chapter names a workload the kernel's `workload=` parser does not know.
    UnknownWorkload(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(error) => write!(f, "malformed chapter declaration: {error}"),
            Self::UnknownWorkload(name) => write!(
                f,
                "chapter names workload {name:?}, which the kernel does not recognise — \
                 booting it would silently fall back to the default, and the chapter \
                 would describe a machine the reader is not looking at"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::Chapter;
    use crate::anchor::FrameMatch;
    use protocol::CapEventKind;

    /// A valid chapter booting `workload`, with the anchor every chapter must have.
    fn chapter_booting(workload: &str) -> String {
        format!(
            r#"
            slug = "capabilities"
            title = "Who is allowed to do what"
            workload = "{workload}"

            [anchor]
            occurrence = 1

            [anchor.matches]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["RECV", "MINT"]
            "#
        )
    }

    /// A chapter names the workload its guest boots, and parsing yields it.
    ///
    /// `init` rather than the bootarg-less default: a chapter should say what it
    /// boots, so that reading the manifest answers the question.
    #[test]
    fn a_chapter_declares_the_workload_its_guest_boots() {
        let chapter = Chapter::parse(&chapter_booting("init"))
            .expect("a chapter naming a workload the kernel knows should parse");

        assert_eq!(chapter.slug, "capabilities");
        assert_eq!(chapter.title, "Who is allowed to do what");
        assert_eq!(chapter.workload, "init");
    }

    /// **A chapter says where it stops and what is true there.**
    ///
    /// Both halves are data rather than prose, because both are checked. A claim
    /// carries the sentence a reader sees *and* the frame that must (or must not)
    /// be in the stream — the gate asserts the second, and a kernel change that
    /// falsifies it turns the build red instead of leaving the prose lying.
    ///
    /// `present = false` is the capability-shaped half: "the client is never handed
    /// the right to receive" is the interesting claim about a delegation, and it is
    /// only expressible as an absence.
    #[test]
    fn a_chapter_declares_where_it_stops_and_what_holds_there() {
        let chapter = Chapter::parse(
            r#"
            slug = "capabilities"
            title = "Who is allowed to do what"
            workload = "init"

            [anchor]
            occurrence = 1

            [anchor.matches]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["RECV", "MINT"]

            [[claims]]
            says = "the client is never handed the right to receive"
            present = false

            [claims.frame]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            rights = ["RECV"]
            "#,
        )
        .expect("a chapter with an anchor and a claim parses");

        assert_eq!(chapter.anchor.occurrence, 1);
        assert!(matches!(
            chapter.anchor.matches,
            FrameMatch::CapEvent { kind: CapEventKind::Transferred, ref name, rights: Some(12) }
                if name == "fs"
        ));

        let [claim] = &chapter.claims[..] else { panic!("one claim") };
        assert_eq!(claim.says, "the client is never handed the right to receive");
        assert!(!claim.present, "the claim is that this never happens");
    }

    /// **A claim left unqualified asserts the frame is there.**
    ///
    /// The default has to be `present`, and it has to be tested, because getting it
    /// backwards is silent: a chapter that meant "the server is handed RECV|MINT"
    /// would instead assert that it never is, and pass — against a guest where the
    /// handover genuinely does not happen. The gate would be green and the prose
    /// would be describing the opposite of the machine.
    #[test]
    fn a_claim_that_does_not_say_otherwise_asserts_the_frame_is_present() {
        let chapter = Chapter::parse(&format!(
            "{}\n{}",
            chapter_booting("init"),
            r#"
            [[claims]]
            says = "the file server is handed the endpoint"

            [claims.frame]
            frame = "cap_event"
            kind = "Transferred"
            name = "fs"
            "#
        ))
        .expect("a claim without `present` parses");

        let [claim] = &chapter.claims[..] else { panic!("one claim") };
        assert!(claim.present, "an unqualified claim asserts presence, not absence");
    }

    /// **A chapter cannot name a workload the kernel does not know.**
    ///
    /// The failure this prevents is a quiet one. An unrecognised `workload=` boots
    /// the kernel's *default* rather than refusing, so the guest would come up, the
    /// panels would fill, and the chapter would confidently describe a machine the
    /// reader is not looking at. Checked through `kernel_boot::bootargs::select` —
    /// the guest's own parser, not a copy of it.
    #[test]
    fn a_chapter_cannot_name_a_workload_the_kernel_does_not_know() {
        let error = Chapter::parse(&chapter_booting("no-such-workload"))
            .expect_err("an unknown workload should be rejected at parse time");

        assert!(
            error.to_string().contains("no-such-workload"),
            "the error should name the offending workload; got: {error}"
        );
    }
}
