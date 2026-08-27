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

    /// A chapter names the workload its guest boots, and parsing yields it.
    ///
    /// `init` rather than the bootarg-less default: a chapter should say what it
    /// boots, so that reading the manifest answers the question.
    #[test]
    fn a_chapter_declares_the_workload_its_guest_boots() {
        let chapter = Chapter::parse(
            r#"
            slug = "capabilities"
            title = "Who is allowed to do what"
            workload = "init"
            "#,
        )
        .expect("a chapter naming a workload the kernel knows should parse");

        assert_eq!(chapter.slug, "capabilities");
        assert_eq!(chapter.title, "Who is allowed to do what");
        assert_eq!(chapter.workload, "init");
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
        let error = Chapter::parse(
            r#"
            slug = "capabilities"
            title = "Who is allowed to do what"
            workload = "no-such-workload"
            "#,
        )
        .expect_err("an unknown workload should be rejected at parse time");

        assert!(
            error.to_string().contains("no-such-workload"),
            "the error should name the offending workload; got: {error}"
        );
    }
}
