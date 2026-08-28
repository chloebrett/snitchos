//! `workload=kitsch-stitch` — the compositor's *policy* as a Stitch program.
//!
//! This is the increment where `kitsch` stops being Rust. A Stitch program
//! describes the scene — a bordered box with a title — and calls `present`; the
//! backend below does the cell-picking, the glyph rasterizing and the syscall.
//!
//! **The boundary is the whole design.** Measured on target, one Stitch list
//! element costs ~24k guest instructions, so a Stitch loop over a screen's ~7,200
//! cells would be ~173M — about 85x a full-screen native clear. So Stitch
//! iterates over *windows* (a handful) and never over cells, and one `present`
//! call carries a whole window. See `plans/kitsch-v1.md` increments 0 and 5.
//!
//! Also the first **long-running** Stitch process: every Stitch program so far
//! ran and exited. It presents repeatedly and emits its own heap footprint, so a
//! leak in the interpreter shows up as a rising number on the wire rather than
//! as a desktop that dies overnight.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use kitsch_render::{ibm_vga_8x16, Cell, Grid};
use snitchos_user::{delegated_handle, entry, present, register_counter, tracer, PRESENT_MAX};
use stitch::platform::{CapInfo, Handle, Platform, Rights, RuntimePlatform};
use stitch::runner::Repl;
use stitch::telemetry::RuntimeTelemetry;

/// How many times the Stitch program presents. Enough that a per-present leak
/// would be visible in the heap figure, few enough to stay inside the itest's
/// instruction budget.
const PRESENTS: usize = 8;

/// The screen colours. Foreground is what `decode_text` reads back.
const INK: u32 = 0x00c0_c0c0;
const PAPER: u32 = 0x0000_2040;

/// A `Platform` that can draw: everything else delegates to the on-target
/// runtime backend, and `present` rasterizes text through `kitsch-render` and
/// hands the pixels to the kernel.
///
/// It lives here rather than in `stitch` on purpose — the language crate should
/// not learn about the desktop, and a program supplying its own backend is
/// exactly the seam `Platform` exists to provide.
struct KitschPlatform {
    inner: RuntimePlatform,
    display: usize,
}

impl Platform for KitschPlatform {
    fn read_line(&self) -> Option<String> {
        self.inner.read_line()
    }

    fn read_byte(&self) -> Option<u8> {
        self.inner.read_byte()
    }

    fn write(&self, text: &str) {
        self.inner.write(text);
    }

    fn hold(&self) -> Vec<CapInfo> {
        self.inner.hold()
    }

    fn fs_read(&self, name: &str) -> Option<String> {
        self.inner.fs_read(name)
    }

    fn revoke(&self, handle: Handle) -> Option<usize> {
        self.inner.revoke(handle)
    }

    fn grant(&self, handle: Handle, badge: u64, rights: Rights) -> Option<Handle> {
        self.inner.grant(handle, badge, rights)
    }

    /// The native half of one frame: compose the rows into a cell grid,
    /// rasterize, and present run by run. Everything expensive happens here, in
    /// compiled code, exactly once per call.
    fn present(&self, x: u32, y: u32, rows: &[&str]) -> bool {
        let cols = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        if cols == 0 || rows.is_empty() {
            return true; // nothing to draw is not a failure
        }

        let mut grid = Grid::new(cols, rows.len());
        for (row, line) in rows.iter().enumerate() {
            for (col, glyph) in line.chars().enumerate() {
                grid.set(col, row, Cell { glyph, fg: INK, bg: PAPER });
            }
        }

        let font = ibm_vga_8x16();
        let (cw, ch) = (font.width, font.height);
        let (pw, ph) = (cols * cw, rows.len() * ch);
        let mut pixels = alloc::vec![0u32; pw * ph];
        kitsch_render::rasterize(&grid, &font, &mut pixels, pw);

        let (ox, oy) = (x as usize * cw, y as usize * ch);
        for line in 0..ph {
            let mut done = 0;
            while done < pw {
                let n = (pw - done).min(PRESENT_MAX);
                let start = line * pw + done;
                if present(self.display, ox + done, oy + line, &pixels[start..start + n]).is_err() {
                    return false;
                }
                done += n;
            }
        }
        true
    }
}

/// The compositor's policy, in Stitch: describe the scene, call `present`. No
/// pixel crosses this boundary — the rows are text and the backend does the
/// rest.
///
/// One expression, because the REPL evaluates expressions: Stitch's top-level
/// definition form is `name(args) -> T = body` and there is no `let`.
///
/// **A normal Rust string, not a raw one.** The escapes below must be processed
/// *by Rust*, so Stitch's lexer sees the actual glyph characters — a raw string
/// would hand it the literal seven characters `\u{cd}`. And they are CP437 code
/// points (U+00CD, not U+2550): the font is indexed by code-page byte, so the
/// Unicode box-drawing block would index past the table and render blank.
/// The border run is **computed, not typed**: an 18-long fold in Stitch rather
/// than eighteen hand-written escapes. The first attempt typed seventeen, which
/// is exactly the failure that spelling a repetition out invites — and it also
/// means the interpreter is doing real work per frame rather than holding
/// literals.
const POLICY: &str = "present(0, 0, [\
    \"\u{c9}\" + (1.. |> take(18) |> toList |> fold(\"\", (a, _) -> a + \"\u{cd}\")) + \"\u{bb}\", \
    \"\u{ba}  kitsch          \u{ba}\", \
    \"\u{c8}\" + (1.. |> take(18) |> toList |> fold(\"\", (a, _) -> a + \"\u{cd}\")) + \"\u{bc}\"\
])";

#[entry(needs = [("display", DISPLAY_SINK, DISPLAY)])]
fn main() {
    let _span = tracer().span("kitsch.stitch.start");

    let platform = Rc::new(KitschPlatform {
        inner: RuntimePlatform::new(),
        display: delegated_handle(0),
    });
    // One env, built once and reused — the prelude registration is the expensive
    // part of an eval, and a compositor re-registering it per frame would be
    // paying the REPL's per-line cost forever.
    let mut repl = Repl::with_backends(Rc::new(RuntimeTelemetry::default()), platform.clone());

    let evals = register_counter("snitchos.kitsch.policy_evals");
    for i in 0..PRESENTS {
        let out = repl.eval_line(POLICY);
        if out.contains("error") {
            platform.write(&out);
            let _ = tracer().span("kitsch.stitch.eval_failed");
            return;
        }
        if i == 0 {
            let _ = tracer().span("kitsch.stitch.first_present");
        }
        evals.emit(i as i64 + 1);
    }

    let _ = tracer().span("kitsch.stitch.presented");
}
