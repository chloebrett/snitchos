//! What a tour chapter declares, and what makes one valid.
//!
//! A chapter is **data**: the workload its guest boots, the anchor it replays to,
//! and the claims its prose makes. This crate owns that schema and nothing else —
//! no snemu, no MMIO, no I/O beyond reading its own chapters. It is host-tested,
//! and is meant to be linked by *both* drivers — the gate and (from step 8) the
//! browser — so that they stop at the same frame. A second definition of "the
//! anchor" in TypeScript would be free to drift from this one.
//!
//! See [`../../plans/tour-v1.md`](../../plans/tour-v1.md) for the design and the
//! decisions behind it.

pub mod anchor;
pub mod search;

use std::fs;
use std::path::{Path, PathBuf};

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
    /// The prose, as a path relative to the chapters directory.
    pub body: String,
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
    /// Every chapter declared under `dir`, in slug order.
    ///
    /// Slug order rather than directory order: a filesystem's iteration order is
    /// not a promise, and the tour's chapter sequence must not depend on one.
    ///
    /// # Errors
    /// [`LoadError`] naming the offending file — the gate's failure has to say
    /// *which* chapter is wrong, or the message sends you looking through all of
    /// them.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, LoadError> {
        let entries = fs::read_dir(dir).map_err(|e| LoadError::at(dir, &e))?;

        let mut chapters = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| LoadError::at(dir, &e))?.path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|e| LoadError::at(&path, &e))?;
            chapters.push(Self::parse(&text).map_err(|e| LoadError::at(&path, &e))?);
        }

        chapters.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(chapters)
    }

    /// Parse a chapter declaration, rejecting one the guest could not honour.
    pub fn parse(declaration: &str) -> Result<Self, ParseError> {
        let chapter: Self = toml::from_str(declaration).map_err(ParseError::Malformed)?;

        if kernel_boot::bootargs::select(&format!("workload={}", chapter.workload)).is_none() {
            return Err(ParseError::UnknownWorkload(chapter.workload));
        }

        Ok(chapter)
    }
}

/// A chapter that could not be loaded, and where it lives.
#[derive(Debug)]
pub struct LoadError {
    /// The file at fault.
    pub path: PathBuf,
    /// What was wrong with it.
    pub cause: String,
}

impl LoadError {
    fn at(path: &Path, cause: &dyn std::fmt::Display) -> Self {
        Self { path: path.to_path_buf(), cause: cause.to_string() }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.cause)
    }
}

impl std::error::Error for LoadError {}

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
            body = "capabilities.mdx"

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
            body = "capabilities.mdx"

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

    /// **Every chapter in the repo parses, and its prose is where it says.**
    ///
    /// This is the tour's schema validation — the thing Astro's content collections
    /// would have given us and that an SPA has to own. A chapter that does not parse
    /// is one the site cannot render and the gate cannot check, and that failure
    /// belongs in the fast suite rather than in a browser.
    ///
    /// The body check is a link check by another name: a manifest naming prose that
    /// is not there renders an empty chapter, which looks like a styling bug and is
    /// not one. This repo has broken exactly this kind of unbuilt reference on every
    /// doc move it has ever done.
    #[test]
    fn every_chapter_in_the_repo_parses_and_its_prose_exists() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/chapters"));
        let chapters = Chapter::load_dir(dir).expect("every chapter on disk parses");

        assert!(!chapters.is_empty(), "the tour has at least one chapter");
        for chapter in &chapters {
            let body = dir.join(&chapter.body);
            assert!(body.is_file(), "{} names prose at {body:?}, which is not there", chapter.slug);
        }
    }

    /// **A chapter that will not load is named, and so is the reason.**
    ///
    /// The entire reason `LoadError` carries a path is that the gate's failure has
    /// to say *which* chapter is wrong — "a chapter failed to load" sends you
    /// reading all of them. A `Display` that rendered nothing would still compile,
    /// still fail the build, and still be useless, which is why this asserts on the
    /// message rather than merely on the error.
    #[test]
    fn a_chapter_that_will_not_load_is_named_in_the_error() {
        let missing = std::path::Path::new("/no/such/tour/chapters");
        let error = Chapter::load_dir(missing).expect_err("a missing directory cannot load");

        let message = error.to_string();
        assert!(message.contains("/no/such/tour/chapters"), "should name the path; got: {message}");
        assert!(
            message.len() > "/no/such/tour/chapters".len(),
            "and should say what was wrong with it; got: {message}"
        );
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
