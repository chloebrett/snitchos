//! The boot banner's art and its one piece of logic: fitting a version string
//! into the fixed-width field on the drawn screen.
//!
//! The art is split around that field rather than stored as one block, because
//! the version is the only part that isn't known until compile time. Printing
//! it lives in `kernel::banner` — this crate just decides what the characters
//! are.

/// Width, in bytes, of the version field on the drawn screen.
///
/// The screen's interior is 23 columns; ` snitchos v` eats 11 of them. Anything
/// longer than the remainder would push the monitor's right border out of line,
/// so the field truncates rather than overflows — a ragged frame reads as a
/// bug, a clipped version reads as a version.
pub const FIELD_WIDTH: usize = 12;

/// A version string fitted to [`FIELD_WIDTH`]: padded if short, clipped if long.
pub struct VersionField([u8; FIELD_WIDTH]);

impl VersionField {
    #[must_use]
    pub fn new(version: &str) -> Self {
        let fits = version
            .char_indices()
            .map(|(i, c)| i + c.len_utf8())
            .take_while(|end| *end <= FIELD_WIDTH)
            .last()
            .unwrap_or(0);

        let mut field = [b' '; FIELD_WIDTH];
        field[..fits].copy_from_slice(&version.as_bytes()[..fits]);
        Self(field)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY-equivalent argument, no `unsafe` needed: `new` only ever
        // copies whole chars plus ASCII spaces, so the buffer is valid UTF-8
        // by construction and `from_utf8` cannot fail.
        core::str::from_utf8(&self.0).unwrap_or("")
    }
}

/// Banner lines above the version field.
pub const ART_ABOVE: &[&str] = &[
    r#"          .----------------------------."#,
    r#"         (      say "i am alive"        )"#,
    r#"          '------.---------------------'"#,
    r#"                /"#,
    r#"               /             .-----------------------------."#,
    r#"              /              |  .-----------------------.  |"#,
];

/// The version line, up to the field itself.
pub const VERSION_PREFIX: &str = r#"        .--. '               |  | snitchos v"#;

/// The version line, after the field.
pub const VERSION_SUFFIX: &str = r#"|  |"#;

/// Banner lines below the version field.
pub const ART_BELOW: &[&str] = &[
    r#"       /    \                |  | > I AM ALIVE          |  |"#,
    r#"      |  o   |               |  | _                     |  |"#,
    r#"       \    /                |  '-----------------------'  |"#,
    r#"        '--'                 '-----------------------------'"#,
    r#"        /  \ .---------------.        |            |"#,
    r#"       /    (  oh my god.     )        '------------'"#,
    r#"      |     |'---------------'"#,
    r#"      |      |"#,
    r#"      |      |"#,
];

/// Width of the widest banner line — the length of the rule that fences the
/// banner off from the surrounding boot log.
///
/// Derived from the art rather than written down beside it: a hand-maintained
/// number would be one more thing an art edit could silently desynchronise.
/// Not a `const`, because `Iterator::max` isn't available in const context and
/// nothing needs this before runtime — a once-per-boot fold over ~16 lengths is
/// cheaper than the hand-rolled loop const-ness would cost.
#[must_use]
pub fn width() -> usize {
    ART_ABOVE
        .iter()
        .chain(ART_BELOW.iter())
        .map(|line| line.len())
        .chain(core::iter::once(
            VERSION_PREFIX.len() + FIELD_WIDTH + VERSION_SUFFIX.len(),
        ))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn a_short_version_is_padded_out_to_the_field_width() {
        assert_eq!(VersionField::new("0.13.0").as_str(), "0.13.0      ");
    }

    #[test]
    fn a_version_that_exactly_fills_the_field_is_left_alone() {
        assert_eq!(VersionField::new("0.13.0-rc123").as_str(), "0.13.0-rc123");
    }

    #[test]
    fn an_over_long_version_is_truncated_to_the_field_width() {
        assert_eq!(
            VersionField::new("0.13.0-nightly.20260724").as_str(),
            "0.13.0-night"
        );
    }

    #[test]
    fn truncation_drops_a_straddling_char_whole_and_pads_the_slack() {
        // Versions are ASCII by convention, but the field is a fixed *byte*
        // budget: "0.13.0-" (7) + α (2) + β (2) = 11, and γ would overflow it.
        // Slicing γ in half would produce invalid UTF-8, so it goes whole and
        // the leftover byte is padded.
        assert_eq!(VersionField::new("0.13.0-αβγδ").as_str(), "0.13.0-αβ ");
    }

    #[test]
    fn the_version_line_is_as_wide_as_the_screens_other_lines() {
        let version_line = VERSION_PREFIX.len() + FIELD_WIDTH + VERSION_SUFFIX.len();
        let reference = ART_BELOW
            .iter()
            .find(|line| line.contains("I AM ALIVE"))
            .expect("the screen prints I AM ALIVE");
        assert_eq!(version_line, reference.len());
    }

    #[test]
    fn no_banner_line_overhangs_the_width() {
        for line in ART_ABOVE.iter().chain(ART_BELOW.iter()) {
            assert!(
                line.len() <= width(),
                "{line:?} ({}) overhangs the rule ({})",
                line.len(),
                width()
            );
        }
        assert!(VERSION_PREFIX.len() + FIELD_WIDTH + VERSION_SUFFIX.len() <= width());
    }

    #[test]
    fn the_width_is_not_padded_past_the_art() {
        // The rule fences the banner; a rule wider than everything it fences
        // would look like a mistake. Some line must actually reach the width.
        assert!(
            ART_ABOVE
                .iter()
                .chain(ART_BELOW.iter())
                .any(|line| line.len() == width()),
            "no line reaches the rule width of {}",
            width()
        );
    }

    #[test]
    fn every_line_that_crosses_the_screen_is_the_same_width() {
        let widths: Vec<usize> = ART_ABOVE
            .iter()
            .chain(ART_BELOW.iter())
            .filter(|line| line.ends_with("|  |") || line.ends_with("'  |"))
            .map(|line| line.len())
            .collect();

        assert!(widths.len() >= 3, "expected several screen lines: {widths:?}");
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "ragged monitor frame: {widths:?}"
        );
    }
}
