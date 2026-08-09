//! What regime this kernel and its embedded userspace were actually built at.
//!
//! Two facts, and they are independent: the kernel's cargo profile and the
//! opt-level of the userspace programs `kernel/build.rs` embeds. Keeping them
//! separate is the whole point — the Low/Mid/Hi/Max ladder exists because a
//! release kernel can carry a userspace at opt-1, 2 or 3, and collapsing them
//! into one number is how "what is on this board?" stopped having an answer.
//!
//! This lives in `kernel-boot` rather than inside the build script so the rule
//! has a test. `kernel/build.rs` calls it both to decide what to *pass* to the
//! nested userspace build and to report what it passed, so the reported value
//! cannot drift from the built one.

/// The level a release kernel gives its userspace when nothing overrides it.
///
/// opt-1 rather than opt-3 for historical reasons that no longer hold: it was
/// pinned to dodge a "userspace opt>=2 UB class" later shown not to exist
/// (docs/debt-register.md #16). It survives as a deliberate regime —
/// `OptLevel::Mid` — not as a belief.
pub const DEFAULT_RELEASE_USERSPACE_OPT: &str = "1";

/// The opt-level the embedded userspace was **actually** built at.
///
/// `kernel/build.rs` adds `--release` — and forwards an opt-level at all — only
/// when the *kernel* is a release build. So in a debug kernel the userspace is
/// a debug build at opt-0, and any `SNITCHOS_USERSPACE_OPT` in the environment
/// is simply never read.
///
/// That last sentence is the entire reason this function exists. Board images
/// come from `cargo xtask image`, which was a debug build until 2026-08-06 — so
/// the VF2 ran an **opt-0** userspace, drivel's transformer forward pass
/// included, and nothing said so.
#[must_use]
pub fn userspace_opt_level<'a>(kernel_profile: &str, opt_override: Option<&'a str>) -> &'a str {
    if kernel_profile != "release" {
        return "0";
    }
    opt_override.unwrap_or(DEFAULT_RELEASE_USERSPACE_OPT)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_RELEASE_USERSPACE_OPT, userspace_opt_level};

    /// **The fact that cost the board its performance.** `build.rs` consults
    /// `SNITCHOS_USERSPACE_OPT` only inside its release branch, so in a debug
    /// kernel the variable is inert — setting it looks like it should work and
    /// changes nothing. Reporting the *override* rather than the resolved level
    /// would print `3` for a build that is opt-0, which is worse than printing
    /// nothing at all.
    #[test]
    fn a_debug_kernel_carries_an_opt_0_userspace_whatever_the_environment_asks_for() {
        assert_eq!(userspace_opt_level("debug", None), "0");
        assert_eq!(userspace_opt_level("debug", Some("3")), "0");
    }

    #[test]
    fn a_release_kernel_without_an_override_takes_the_pinned_default() {
        assert_eq!(userspace_opt_level("release", None), DEFAULT_RELEASE_USERSPACE_OPT);
    }

    #[test]
    fn a_release_kernel_honours_an_override() {
        assert_eq!(userspace_opt_level("release", Some("0")), "0");
        assert_eq!(userspace_opt_level("release", Some("2")), "2");
        assert_eq!(userspace_opt_level("release", Some("3")), "3");
    }

    /// An unrecognised profile is treated as "not release", i.e. opt-0. Cargo
    /// only sets `PROFILE` to `debug` or `release`, so this is about refusing to
    /// invent an answer for an input that would mean we had misunderstood the
    /// build: guessing `1` there reports an optimized userspace for a build we
    /// cannot account for.
    #[test]
    fn an_unrecognised_profile_is_not_assumed_to_be_optimized() {
        assert_eq!(userspace_opt_level("bench", Some("3")), "0");
    }
}
