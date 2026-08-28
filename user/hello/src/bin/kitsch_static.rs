//! `workload=kitsch-static` — `kitsch` first light.
//!
//! A userspace program holding a `DisplaySink` cap composes a fixed scene
//! through `kitsch-render`, rasterizes it with the IBM VGA 8x16 font, and
//! presents it one damage span at a time through the `Present` syscall.
//!
//! Static on purpose: no clients, no input, no layout. Its whole job is proving
//! the chain end to end — font → compose → rasterize → cap-gated present →
//! glass — so that when a real compositor sits on top, a blank screen has only
//! one place left to hide. See `plans/kitsch-v1.md` increment 4.
//!
//! It also carries increment 3's acceptance check: it presents **twice**, once
//! with its real handle and once with a handle it does not hold, so a single
//! boot shows both that the cap works and that the refusal snitches.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;

use kitsch_render::{compose, ibm_vga_8x16, Cell, Rect, Surface, Window};
use snitchos_user::{delegated_handle, entry, present, tracer, PRESENT_MAX};

/// The scene's cell size. Deliberately not the full 160x45 screen: this proves
/// the path, and a smaller grid keeps the present-call count (and so the itest's
/// instruction budget) modest while exercising exactly the same code.
const COLS: usize = 20;
const ROWS: usize = 3;

/// Build the fixed scene: a bordered box with a title, drawn from CP437's
/// box-drawing glyphs — the vocabulary real window furniture will use.
fn scene() -> Surface {
    let mut cells = vec![Cell::default(); COLS * ROWS];
    let fg = 0x00c0_c0c0;
    let bg = 0x0000_2040;

    let mut put = |x: usize, y: usize, glyph: char| {
        cells[y * COLS + x] = Cell { glyph, fg, bg };
    };

    // CP437 box drawing: 0xC9/0xBB/0xC8/0xBC corners, 0xCD horizontal,
    // 0xBA vertical — the double-line set.
    for x in 0..COLS {
        put(x, 0, '\u{cd}');
        put(x, ROWS - 1, '\u{cd}');
    }
    for y in 0..ROWS {
        put(0, y, '\u{ba}');
        put(COLS - 1, y, '\u{ba}');
    }
    put(0, 0, '\u{c9}');
    put(COLS - 1, 0, '\u{bb}');
    put(0, ROWS - 1, '\u{c8}');
    put(COLS - 1, ROWS - 1, '\u{bc}');

    for (i, glyph) in " kitsch ".chars().enumerate() {
        put(2 + i, 1, glyph);
    }

    Surface { width: COLS, height: ROWS, cells }
}

#[entry(needs = [("display", DISPLAY_SINK, DISPLAY)])]
fn main() {
    let _span = tracer().span("kitsch.first_light");

    // The display cap is the first delegated grant, right after the two
    // bootstrap authorities.
    let display = delegated_handle(0);

    let font = ibm_vga_8x16();
    let content = scene();
    let grid = compose(COLS, ROWS, &[Window {
        rect: Rect { x: 0, y: 0, width: COLS, height: ROWS },
        content: &content,
    }]);

    // Rasterize the whole scene once. The cold paint is the expensive case and
    // the one worth proving before damage narrows it — and damage's own
    // correctness is settled by `kitsch-render`'s exhaustive span tests, not by
    // re-deriving it here.
    let (cw, ch) = (font.width, font.height);
    let (pw, ph) = (COLS * cw, ROWS * ch);
    let mut pixels = vec![0u32; pw * ph];
    kitsch_render::rasterize(&grid, &font, &mut pixels, pw);

    // Hand the pixels over a row at a time, in `PRESENT_MAX`-pixel runs — the
    // syscall's copy cap, and why `kitsch` presents runs rather than frames.
    for y in 0..ph {
        let mut done = 0;
        while done < pw {
            let n = (pw - done).min(PRESENT_MAX);
            let start = y * pw + done;
            if present(display, done, y, &pixels[start..start + n]).is_err() {
                // Refused holding the real cap: the grant is wrong, not this
                // program. Say so rather than painting half a screen in silence.
                let _ = tracer().span("kitsch.present_refused");
                return;
            }
            done += n;
        }
    }

    let _ = tracer().span("kitsch.presented");

    // Increment 3's acceptance half: the same call with a handle this process
    // does not hold must be refused, and the refusal must snitch. A silent
    // failure here would look exactly like success.
    if present(display.wrapping_add(64), 0, 0, &[0xffff_ffff]).is_ok() {
        let _ = tracer().span("kitsch.unheld_handle_was_accepted");
    } else {
        let _ = tracer().span("kitsch.unheld_handle_refused");
    }
}
