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
    rule();
    println!();
}

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
