//! The boot banner, printed to the UART once the higher-half mapping is live.
//!
//! Art and version fitting live in `kernel_boot::banner` (host-tested); this is
//! only the printing. Note the ordering constraint that applies to everything
//! here: a formatted `println!` embeds absolute formatter fn-pointers, so this
//! must not run before `mmu::enable` — see CLAUDE.md's memory-layout gotchas.

use crate::{print, println};
use kernel_boot::banner::{
    ART_ABOVE, ART_BELOW, VERSION_PREFIX, VERSION_SUFFIX, VersionField, width,
};

/// Prints the banner on the human-readable UART channel.
///
/// Deliberately UART-only: this is decoration for whoever is watching a serial
/// console, not something the collector should have to decode. Boot facts that
/// want asserting on go out as telemetry frames instead.
pub fn print() {
    let version = VersionField::new(env!("CARGO_PKG_VERSION"));

    // Fenced top and bottom: unframed, the art reads as more boot log rather
    // than as a break in it — which is exactly how it looked on the board.
    println!();
    rule();
    for line in ART_ABOVE {
        println!("{line}");
    }
    println!("{VERSION_PREFIX}{}{VERSION_SUFFIX}", version.as_str());
    for line in ART_BELOW {
        println!("{line}");
    }
    println!("{REGIME_LINE}");
    rule();
    println!();
}

/// What this image was built as, for whoever is looking at a serial console.
///
/// On the VisionFive 2 this is the *only* channel that answers the question:
/// the matching telemetry frame needs a collector, and the collector has no
/// serial source on hardware (docs/debt-register.md #18). So the board reads its
/// build regime here, and the itest asserts on the frame.
///
/// Both halves are named because they move independently — a release kernel can
/// carry a userspace at opt-1, 2 or 3, and it is the *userspace* number that
/// decides how fast a drivel completion is.
///
/// `concat!` rather than `format!`: both values are compile-time literals, so
/// this is a `&'static str` in rodata and the boot path allocates nothing to
/// print it.
///
/// ASCII only, deliberately. The board's serial console is the channel that
/// most needs this line and the least able to render anything exotic — it
/// already interleaves the heartbeat pulse mid-word (debt #18), and terminal
/// handling of non-ASCII width is not something to bet a diagnostic on.
const REGIME_LINE: &str = concat!(
    "        build: kernel ",
    env!("SNITCHOS_KERNEL_PROFILE"),
    ", userspace opt-",
    env!("SNITCHOS_USERSPACE_OPT_LEVEL"),
);

/// A full-width horizontal rule, drawn a character at a time.
///
/// No `"===…"` constant to keep in step with [`width`], and no allocation to do
/// it — at ~60 UART bytes once per boot, the loop costs nothing worth saving.
fn rule() {
    for _ in 0..width() {
        print!("=");
    }
    println!();
}
