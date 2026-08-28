//! `kitsch-render` — the native half of the desktop.
//!
//! `kitsch`'s *policy* (layout, focus, damage bookkeeping, surface lifecycle,
//! cap grants, input routing) is a Stitch program. This crate is what that
//! program calls, **once per frame**: compose a scene into a cell grid, then
//! rasterize the damaged spans into pixels.
//!
//! That boundary is not a style choice. Measured on target, one Stitch list
//! element costs ~24k guest instructions, so a Stitch loop over the grid's
//! ~7,200 cells would be ~173M instructions — about 85x a full-screen native
//! clear. **Stitch may iterate over windows; it must never iterate over
//! cells.** See `plans/kitsch-v1.md` increment 0.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use kernel_devices::framebuffer::{Framebuffer, Rect as FbRect};

/// One character cell: what to draw and in what colours. Colours are packed
/// `0xRRGGBB`; the alpha/pad byte is the rasterizer's business, not the grid's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub glyph: char,
    pub fg: u32,
    pub bg: u32,
}

/// The desktop's default cell: a blank on the background colour.
impl Default for Cell {
    fn default() -> Self {
        Self { glyph: ' ', fg: 0x00c0_c0c0, bg: 0x0000_0000 }
    }
}

/// A rectangular region of the screen, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// A client's content, in **surface-local** coordinates. A surface is a
/// texture, not a piece of the screen: it does not know where it sits, and
/// moving a window touches none of it. Where it lands is kitsch's business —
/// the client holds `DRAW`, kitsch keeps `CONFIGURE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Cell>,
}

impl Surface {
    /// The cell at surface-local `(x, y)`, or `None` outside the surface.
    pub fn cell(&self, x: usize, y: usize) -> Option<Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells.get(y * self.width + x).copied()
    }
}

/// A surface placed on the screen: kitsch's decision about where it goes.
#[derive(Debug, Clone, Copy)]
pub struct Window<'a> {
    pub rect: Rect,
    pub content: &'a Surface,
}

/// Compose a scene into a grid. Later windows paint over earlier ones, so the
/// slice is in z-order — bottom first. Content is clipped to *both* the
/// window's rect and the screen, so a client cannot paint outside the geometry
/// kitsch gave it however large its surface is.
pub fn compose(width: usize, height: usize, windows: &[Window<'_>]) -> Grid {
    let mut grid = Grid::new(width, height);
    for window in windows {
        let x_end = (window.rect.x + window.rect.width).min(width);
        let y_end = (window.rect.y + window.rect.height).min(height);
        for y in window.rect.y..y_end {
            for x in window.rect.x..x_end {
                if let Some(cell) = window.content.cell(x - window.rect.x, y - window.rect.y) {
                    grid.cells[y * width + x] = cell;
                }
            }
        }
    }
    grid
}

/// A horizontal run of dirty cells on one row — what the rasterizer redraws.
/// Never crosses a row: the framebuffer is row-major, so a horizontal run is
/// contiguous memory and a vertical one is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub y: usize,
    pub x: usize,
    pub width: usize,
}

/// Which cells changed since the last present, as one dirty bit each.
///
/// A bitmap, not a tree. 7,200 cells is **900 bytes** — 113 `u64` words to mark
/// and scan — and there is no query to accelerate because every dirty cell gets
/// visited anyway. A quadtree is four orders of magnitude short of paying for
/// itself here. See `docs/kitsch-design.md` §4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    width: usize,
    height: usize,
    bits: Vec<u64>,
}

impl Damage {
    /// A map with nothing dirty.
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, bits: vec![0; (width * height).div_ceil(64)] }
    }

    /// Mark one cell dirty. Out of bounds is ignored, not a panic: callers
    /// compute cell coordinates from geometry that clips, and a redraw request
    /// for a cell that is not on screen is a no-op, not a bug.
    pub fn mark(&mut self, x: usize, y: usize) {
        if x >= self.width || y >= self.height {
            return;
        }
        let index = y * self.width + x;
        self.bits[index / 64] |= 1 << (index % 64);
    }

    /// Mark every cell in `rect` dirty, clipped to the map.
    pub fn mark_rect(&mut self, rect: Rect) {
        let x_end = (rect.x + rect.width).min(self.width);
        let y_end = (rect.y + rect.height).min(self.height);
        for y in rect.y..y_end {
            for x in rect.x..x_end {
                self.mark(x, y);
            }
        }
    }

    /// Whether `(x, y)` is dirty. Out of bounds is clean.
    pub fn is_dirty(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let index = y * self.width + x;
        self.bits[index / 64] & (1 << (index % 64)) != 0
    }

    /// Everything clean again — call after a present.
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }

    /// The dirty cells as coalesced per-row runs, in row order.
    ///
    /// Coalescing is the whole optimisation: the framebuffer is row-major, so a
    /// horizontal run is one contiguous blit while the same cells drawn
    /// individually are N. Runs stop at the row end — bits adjacent across a row
    /// boundary are not adjacent pixels.
    pub fn spans(&self) -> Vec<Span> {
        let mut spans = Vec::new();
        for y in 0..self.height {
            let mut x = 0;
            while x < self.width {
                if !self.is_dirty(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.width && self.is_dirty(x, y) {
                    x += 1;
                }
                spans.push(Span { y, x: start, width: x - start });
            }
        }
        spans
    }
}

/// A bitmap font: `height` bytes per glyph, one byte per row, **MSB is the
/// leftmost pixel**. Glyphs are stored contiguously from code point `first`,
/// so lookup is an index rather than a search.
///
/// `width` may be less than 8; only the top `width` bits of each row byte are
/// read. Widths above 8 are not representable and are not wanted — the desktop
/// is an 8x16 grid.
#[derive(Debug, Clone, Copy)]
pub struct Font<'a> {
    pub width: usize,
    pub height: usize,
    pub first: u32,
    pub bitmaps: &'a [u8],
}

impl Font<'_> {
    /// The `height` row-bytes for `glyph`, or `None` if the font has no such
    /// glyph. A missing glyph is the caller's decision to make, not a panic
    /// and not a silent blank.
    pub fn rows(&self, glyph: char) -> Option<&[u8]> {
        let index = usize::try_from(u32::from(glyph).checked_sub(self.first)?).ok()?;
        let start = index.checked_mul(self.height)?;
        self.bitmaps.get(start..start.checked_add(self.height)?)
    }
}

/// Read a rasterized region back as text — the inverse of [`rasterize`], and
/// what makes a display assertion a *snapshot* rather than a list of lit pixel
/// coordinates.
///
/// A pixel whose **RGB** matches `ink` is treated as foreground, anything else as
/// background; each cell's resulting bitmap is matched against `font`'s table.
/// This is exact rather than fuzzy because the ground-truth bitmaps are right
/// here — it is not OCR so much as a table lookup run backwards.
///
/// The comparison ignores the XRGB pad byte deliberately. The rasterizer writes
/// `0xffRRGGBB`, but a framebuffer captured through an emulator's minifb-shaped
/// accessor arrives as `0x00RRGGBB` — the same logical colour in two
/// representations, depending only on where the buffer was read. Masking here
/// means a caller passes the colour it drew with and is right either way.
///
/// Cells matching no glyph decode to `U+FFFD`, so a corrupted screen reads as
/// corrupted instead of as a plausible blank one. Glyphs sharing a bitmap (the
/// blanks, mostly) resolve to the lowest code point, which is why a space stays
/// a space.
///
/// **What this cannot catch**: if the font *table itself* were wrong, drawing and
/// reading with the same wrong data would cancel out. The font has its own tests
/// for that; this checks composition, rasterization and present.
pub fn decode_text(
    pixels: &[u32],
    stride: usize,
    cols: usize,
    rows: usize,
    font: &Font<'_>,
    ink: u32,
) -> String {
    let (cw, ch) = (font.width, font.height);
    let mut out = String::with_capacity(rows * (cols + 1));
    let mut cell = Vec::with_capacity(ch);

    for cy in 0..rows {
        if cy > 0 {
            out.push('\n');
        }
        for cx in 0..cols {
            cell.clear();
            for row in 0..ch {
                let mut bits = 0u8;
                for col in 0..cw {
                    let (x, y) = (cx * cw + col, cy * ch + row);
                    let px = pixels.get(y * stride + x).copied();
                    if px.map(|p| p & RGB) == Some(ink & RGB) {
                        bits |= 0x80 >> col;
                    }
                }
                cell.push(bits);
            }
            out.push(match_glyph(font, &cell).unwrap_or('\u{fffd}'));
        }
    }
    out
}

/// The colour bits of an XRGB8888 pixel — everything but the alpha/pad byte.
const RGB: u32 = 0x00ff_ffff;

/// The code point in `font` whose bitmap is exactly `cell`, lowest first — except
/// that an all-blank cell is a **space**. Several CP437 glyphs are all-zero (NUL
/// at 0x00 among them), and "lowest wins" would decode every blank as `\0`,
/// making a snapshot unreadable for no gain.
fn match_glyph(font: &Font<'_>, cell: &[u8]) -> Option<char> {
    if cell.iter().all(|b| *b == 0) {
        return Some(' ');
    }
    let count = font.bitmaps.len() / font.height;
    (0..count).find_map(|i| {
        let start = i * font.height;
        (&font.bitmaps[start..start + font.height] == cell)
            .then(|| char::from_u32(font.first + i as u32))
            .flatten()
    })
}

/// The IBM VGA 8x16 text-mode font: 256 glyphs, **exactly one 4 KiB page**,
/// carrying CP437's whole repertoire — ASCII, single- and double-line box
/// drawing, block elements, shading, arrows. Everything the window furniture
/// needs, and nothing to source at runtime.
///
/// Indexed by **code page 437 byte**, not by Unicode scalar: `rows('\u{b3}')`
/// is CP437 0xB3 (a vertical line), not U+00B3. Mapping Unicode box-drawing
/// characters onto the code page is a caller's job.
///
/// Provenance and the licence position: `kitsch-render/fonts/PROVENANCE.md`.
pub fn ibm_vga_8x16() -> Font<'static> {
    const BITMAPS: &[u8] = include_bytes!("../fonts/ibm-vga-8x16.bin");
    Font { width: 8, height: 16, first: 0, bitmaps: BITMAPS }
}

/// Draw `grid` into `pixels` (a `stride`-pixels-per-row XRGB8888 buffer).
///
/// Each cell becomes a `font.width` x `font.height` block: background first,
/// then a run of foreground per horizontal run of set bits. Runs rather than
/// pixels so the work goes through `fill_rect`, which increment 1 made emit one
/// `sw` per pixel — a per-pixel path would give back that win immediately.
pub fn rasterize(grid: &Grid, font: &Font<'_>, pixels: &mut [u32], stride: usize) {
    let all: Vec<Span> =
        (0..grid.height).map(|y| Span { y, x: 0, width: grid.width }).collect();
    rasterize_spans(grid, font, pixels, stride, &all);
}

/// Draw only the cells covered by `spans`. The damage-driven path, and the one
/// that matters: a keystroke should cost a few cells, not a screen.
pub fn rasterize_spans(
    grid: &Grid,
    font: &Font<'_>,
    pixels: &mut [u32],
    stride: usize,
    spans: &[Span],
) {
    let (cw, ch) = (font.width, font.height);
    let height = pixels.len() / stride.max(1);
    let mut fb = Framebuffer::new(pixels, stride, height, stride);

    for span in spans {
        if span.y >= grid.height {
            continue;
        }
        let x_end = (span.x + span.width).min(grid.width);
        for x in span.x..x_end {
            let cell = grid.cells[span.y * grid.width + x];
            let (ox, oy) = (x * cw, span.y * ch);
            fb.fill_rect(FbRect { x: ox, y: oy, width: cw, height: ch }, cell.bg);

            let Some(rows) = font.rows(cell.glyph) else {
                continue;
            };
            for (row, bits) in rows.iter().enumerate() {
                let mut col = 0;
                while col < cw {
                    if bits & (0x80 >> col) == 0 {
                        col += 1;
                        continue;
                    }
                    let start = col;
                    while col < cw && bits & (0x80 >> col) != 0 {
                        col += 1;
                    }
                    fb.fill_rect(
                        FbRect { x: ox + start, y: oy + row, width: col - start, height: 1 },
                        cell.fg,
                    );
                }
            }
        }
    }
}

/// The composed screen, in cells. This — not the pixel buffer — is what tests
/// assert on: it is text, so it snapshots readably and reviews like source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
}

impl Grid {
    /// A blank grid of `width` x `height` cells.
    pub fn new(width: usize, height: usize) -> Self {
        Self { width, height, cells: vec![Cell::default(); width * height] }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Overwrite the cell at `(x, y)`. Out-of-bounds writes are ignored — the
    /// grid is the screen, and there is nothing past its edge to scribble on.
    pub fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = cell;
        }
    }

    /// The grid's glyphs as text, one line per row. The snapshot form — colour
    /// and attributes are asserted separately so neither view becomes
    /// unreadable.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.height * (self.width + 1));
        for y in 0..self.height {
            if y > 0 {
                out.push('\n');
            }
            for x in 0..self.width {
                out.push(self.cells[y * self.width + x].glyph);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    #[test]
    fn a_new_grid_is_blank() {
        let grid = Grid::new(3, 2);
        assert_eq!(grid.to_text(), "   \n   ");
    }

    #[test]
    fn a_grid_reports_the_size_it_was_made_with() {
        let grid = Grid::new(160, 45);
        assert_eq!((grid.width(), grid.height()), (160, 45));
    }

    /// Build a surface from text, one line per row. Rows are padded to the
    /// longest, so a test can draw the content it means and not count columns.
    fn surface(lines: &[&str]) -> Surface {
        let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let mut cells = Vec::with_capacity(width * lines.len());
        for line in lines {
            let mut n = 0;
            for glyph in line.chars() {
                cells.push(Cell { glyph, ..Cell::default() });
                n += 1;
            }
            for _ in n..width {
                cells.push(Cell::default());
            }
        }
        Surface { width, height: lines.len(), cells }
    }

    #[test]
    fn a_window_lands_at_its_rect() {
        let content = surface(&["ab", "cd"]);
        let grid = compose(5, 4, &[Window {
            rect: Rect { x: 1, y: 1, width: 2, height: 2 },
            content: &content,
        }]);

        assert_eq!(grid.to_text(), "     \n ab  \n cd  \n     ");
    }

    #[test]
    fn a_surface_larger_than_its_rect_is_clipped_to_the_rect() {
        // The authority property, in the compositor: a client holds DRAW on its
        // own content and kitsch keeps CONFIGURE. However big the surface is, it
        // paints only the geometry kitsch gave it — a client cannot scribble
        // over its neighbours by lying about its size.
        let content = surface(&["XXXX", "XXXX", "XXXX"]);
        let grid = compose(4, 3, &[Window {
            rect: Rect { x: 0, y: 0, width: 2, height: 1 },
            content: &content,
        }]);

        assert_eq!(grid.to_text(), "XX  \n    \n    ");
    }

    #[test]
    fn a_window_hanging_off_the_screen_is_clipped_not_wrapped() {
        // Clipping to the screen must not wrap into the next row, which is the
        // classic off-by-one that looks like corruption rather than a bug.
        let content = surface(&["ab", "cd"]);
        let grid = compose(3, 3, &[Window {
            rect: Rect { x: 2, y: 2, width: 2, height: 2 },
            content: &content,
        }]);

        assert_eq!(grid.to_text(), "   \n   \n  a");
    }

    #[test]
    fn later_windows_paint_over_earlier_ones() {
        // Tiled windows are disjoint, so z-order rarely bites — but popups,
        // menus and drag overlays are exactly where it does, so the rule is
        // defined now rather than discovered then.
        let under = surface(&["...."]);
        let over = surface(&["##"]);
        let grid = compose(4, 1, &[
            Window { rect: Rect { x: 0, y: 0, width: 4, height: 1 }, content: &under },
            Window { rect: Rect { x: 1, y: 0, width: 2, height: 1 }, content: &over },
        ]);

        assert_eq!(grid.to_text(), ".##.");
    }

    /// A 2x2 font holding one glyph, `A`, as a diagonal:
    ///
    /// ```text
    /// #.
    /// .#
    /// ```
    ///
    /// Tiny on purpose — the rasterizer's *logic* (bit order, fg/bg, cell
    /// placement) is what these tests pin. The real 8x16 CP437 table is data,
    /// and data cannot be unit-tested into correctness.
    fn diagonal_font() -> Font<'static> {
        const BITMAPS: &[u8] = &[0b1000_0000, 0b0100_0000];
        Font { width: 2, height: 2, first: 'A' as u32, bitmaps: BITMAPS }
    }

    #[test]
    fn a_glyph_paints_foreground_where_its_bits_are_set() {
        let font = diagonal_font();
        let mut grid = Grid::new(1, 1);
        grid.set(0, 0, Cell { glyph: 'A', fg: 0x00ff_0000, bg: 0x0000_00ff });

        let mut pixels = vec![0u32; 2 * 2];
        rasterize(&grid, &font, &mut pixels, 2);

        let (fg, bg) = (0xffff_0000, 0xff00_00ff);
        assert_eq!(pixels, vec![fg, bg, bg, fg]);
    }

    #[test]
    fn the_ibm_font_covers_all_256_code_page_437_glyphs() {
        let font = ibm_vga_8x16();
        assert_eq!((font.width, font.height), (8, 16));
        assert!(font.rows('\u{0}').is_some(), "first glyph missing");
        assert!(font.rows('\u{ff}').is_some(), "last glyph missing");
        assert!(font.rows('\u{100}').is_none(), "font claims a glyph past the table");
    }

    #[test]
    fn the_ibm_font_has_a_recognisable_capital_a() {
        // Spot-check the two rows that make an `A` an `A`: the apex and the
        // crossbar. Asserting all 16 would pin the data rather than the wiring,
        // and it is the wiring — offset arithmetic and bit order — that can break.
        let font = ibm_vga_8x16();
        let rows = font.rows('A').expect("font has 'A'");
        assert_eq!(rows.len(), 16);
        assert_eq!(rows[2], 0b0001_0000, "apex");
        assert_eq!(rows[7], 0b1111_1110, "crossbar");
    }

    #[test]
    fn the_ibm_font_carries_the_box_drawing_the_window_furniture_needs() {
        // CP437's box-drawing lives at 0xB3..0xDA in the *code page*, which is
        // why the font is indexed by byte and a Unicode box-drawing char must be
        // mapped to it first. This pins that the glyphs are actually present.
        let font = ibm_vga_8x16();
        let vertical = font.rows('\u{b3}').expect("CP437 0xB3 vertical line");
        // A vertical bar: the same single column set on every row of the body.
        assert_eq!(vertical[4], 0b0001_1000);
        assert_eq!(vertical[10], 0b0001_1000);
    }

    /// Render a pixel buffer as art: `#` where the foreground was painted, `.`
    /// where the background was. Readable and byte-exact, so a golden diffs like
    /// source instead of like a checksum — the same reason `Grid::to_text` is
    /// the snapshot form for composition.
    fn pixels_as_art(pixels: &[u32], stride: usize, fg: u32) -> String {
        let mut out = String::new();
        for (i, px) in pixels.iter().enumerate() {
            if i > 0 && i % stride == 0 {
                out.push('\n');
            }
            out.push(if *px == fg { '#' } else { '.' });
        }
        out
    }

    #[test]
    fn a_composed_scene_rasterizes_through_the_real_font() {
        // The integration golden: composition, font wiring and the rasterizer
        // together. The expected art was derived independently from the font
        // binary (not captured from this code's own output), so it catches drift
        // in any of the three rather than blessing whatever they currently do.
        let font = ibm_vga_8x16();
        let content = surface(&["Hi"]);
        let grid = compose(2, 1, &[Window {
            rect: Rect { x: 0, y: 0, width: 2, height: 1 },
            content: &content,
        }]);

        let mut pixels = vec![0u32; 16 * 16];
        rasterize(&grid, &font, &mut pixels, 16);

        let expected = "\
................\n\
................\n\
##...##....##...\n\
##...##....##...\n\
##...##.........\n\
##...##...###...\n\
#######....##...\n\
##...##....##...\n\
##...##....##...\n\
##...##....##...\n\
##...##....##...\n\
##...##...####..\n\
................\n\
................\n\
................\n\
................";
        assert_eq!(pixels_as_art(&pixels, 16, 0xffc0_c0c0), expected);
    }

    #[test]
    fn decoding_a_rasterized_scene_recovers_the_text_that_drew_it() {
        // The inverse of `rasterize`, and the round trip is the test: draw text
        // through the font, read the pixels back through the same font, get the
        // text. That makes a display assertion readable — a snapshot of what is
        // on the screen — instead of a list of lit pixel coordinates.
        let font = ibm_vga_8x16();
        let content = surface(&["Hi!", "ok."]);
        let grid = compose(3, 2, &[Window {
            rect: Rect { x: 0, y: 0, width: 3, height: 2 },
            content: &content,
        }]);

        let (pw, ph) = (3 * font.width, 2 * font.height);
        let mut pixels = vec![0u32; pw * ph];
        rasterize(&grid, &font, &mut pixels, pw);

        assert_eq!(decode_text(&pixels, pw, 3, 2, &font, 0x00c0_c0c0), "Hi!\nok.");
    }

    #[test]
    fn a_blank_cell_decodes_to_a_space_not_a_nul() {
        // CP437 has several all-zero glyphs — NUL at 0x00 and space at 0x20 among
        // them — so "lowest matching code point" would decode every blank cell as
        // `\0` and make each snapshot unreadable.
        let font = ibm_vga_8x16();
        let pixels = vec![0u32; 8 * 16];
        assert_eq!(decode_text(&pixels, 8, 1, 1, &font, 0x00c0_c0c0), " ");
    }

    #[test]
    fn decoding_marks_a_cell_it_cannot_identify() {
        // A cell whose pixels match no glyph must be visible as unknown, not
        // silently rendered as a space — otherwise a corrupted screen decodes to
        // a plausible-looking blank one.
        let font = ibm_vga_8x16();
        let mut pixels = vec![0u32; 8 * 16];
        // A lone lit pixel matches no glyph in the table.
        pixels[8 * 3 + 4] = 0x00c0_c0c0;

        assert_eq!(decode_text(&pixels, 8, 1, 1, &font, 0x00c0_c0c0), "\u{fffd}");
    }

    #[test]
    fn a_fresh_damage_map_has_nothing_to_redraw() {
        let damage = Damage::new(160, 45);
        assert!(damage.spans().is_empty());
    }

    #[test]
    fn one_dirty_cell_is_one_span_of_width_one() {
        let mut damage = Damage::new(4, 2);
        damage.mark(2, 1);
        assert_eq!(damage.spans(), vec![Span { y: 1, x: 2, width: 1 }]);
    }

    #[test]
    fn adjacent_dirty_cells_coalesce_into_one_span() {
        // The whole point of coalescing: three separate blits become one, over
        // contiguous memory.
        let mut damage = Damage::new(5, 1);
        damage.mark(1, 0);
        damage.mark(2, 0);
        damage.mark(3, 0);
        assert_eq!(damage.spans(), vec![Span { y: 0, x: 1, width: 3 }]);
    }

    #[test]
    fn a_clean_cell_between_dirty_ones_splits_the_span() {
        let mut damage = Damage::new(5, 1);
        damage.mark(0, 0);
        damage.mark(1, 0);
        damage.mark(3, 0);
        assert_eq!(damage.spans(), vec![
            Span { y: 0, x: 0, width: 2 },
            Span { y: 0, x: 3, width: 1 },
        ]);
    }

    #[test]
    fn a_span_never_crosses_a_row() {
        // The end of one row and the start of the next are adjacent *bits* but
        // not adjacent *pixels* — the framebuffer is row-major. Merging them
        // would smear a blit across the screen.
        let mut damage = Damage::new(3, 2);
        damage.mark(2, 0);
        damage.mark(0, 1);
        assert_eq!(damage.spans(), vec![
            Span { y: 0, x: 2, width: 1 },
            Span { y: 1, x: 0, width: 1 },
        ]);
    }

    #[test]
    fn marking_a_rect_dirties_exactly_that_rect() {
        let mut damage = Damage::new(4, 3);
        damage.mark_rect(Rect { x: 1, y: 1, width: 2, height: 2 });
        assert_eq!(damage.spans(), vec![
            Span { y: 1, x: 1, width: 2 },
            Span { y: 2, x: 1, width: 2 },
        ]);
    }

    #[test]
    fn spans_partition_the_dirty_cells_for_every_possible_pattern() {
        // Exhaustive, not random: every one of the 2^10 dirty patterns of a 5x2
        // grid. Three invariants together say "the spans are exactly the dirty
        // set, drawn once" — which is what a rasterizer driven by them needs.
        const W: usize = 5;
        const H: usize = 2;
        for pattern in 0u32..(1 << (W * H)) {
            let mut damage = Damage::new(W, H);
            for i in 0..(W * H) {
                if pattern & (1 << i) != 0 {
                    damage.mark(i % W, i / W);
                }
            }

            let spans = damage.spans();
            let mut covered = [0usize; W * H];
            for span in &spans {
                assert!(span.width > 0, "empty span in {pattern:#b}");
                for x in span.x..span.x + span.width {
                    assert!(x < W, "span past the row end in {pattern:#b}");
                    covered[span.y * W + x] += 1;
                }
            }

            for (i, &times) in covered.iter().enumerate() {
                let dirty = pattern & (1 << i) != 0;
                assert_eq!(
                    times,
                    usize::from(dirty),
                    "cell {i} covered {times} times, dirty={dirty}, pattern {pattern:#b}"
                );
            }
        }
    }

    #[test]
    fn each_cell_rasterizes_at_its_own_offset() {
        // A one-cell grid cannot catch an offset bug: every wrong formula still
        // lands at (0,0). Two cells side by side is the smallest grid that can.
        let font = diagonal_font();
        let mut grid = Grid::new(2, 1);
        let ink = Cell { glyph: 'A', fg: 0x00ff_ffff, bg: 0x0000_0000 };
        grid.set(1, 0, ink);

        let mut pixels = vec![0u32; 4 * 2];
        rasterize(&grid, &font, &mut pixels, 4);

        let (fg, bg) = (0xffff_ffff, 0xff00_0000);
        // Left cell is blank (no glyph in this font), right cell has the diagonal.
        assert_eq!(pixels, vec![
            bg, bg, fg, bg, // row 0
            bg, bg, bg, fg, // row 1
        ]);
    }

    #[test]
    fn rasterizing_spans_leaves_undamaged_pixels_alone() {
        // The point of damage tracking: a keystroke must not cost a full-screen
        // redraw. If undamaged pixels get touched, the spans are decoration and
        // the frame budget is gone.
        const SENTINEL: u32 = 0xdead_beef;
        let font = diagonal_font();
        let mut grid = Grid::new(2, 1);
        let ink = Cell { glyph: 'A', fg: 0x00ff_ffff, bg: 0x0000_0000 };
        grid.set(0, 0, ink);
        grid.set(1, 0, ink);

        let mut pixels = vec![SENTINEL; 4 * 2];
        rasterize_spans(&grid, &font, &mut pixels, 4, &[Span { y: 0, x: 1, width: 1 }]);

        let (fg, bg) = (0xffff_ffff, 0xff00_0000);
        assert_eq!(pixels, vec![
            SENTINEL, SENTINEL, fg, bg, // row 0: cell 0 untouched
            SENTINEL, SENTINEL, bg, fg, // row 1: cell 0 untouched
        ]);
    }

    #[test]
    fn a_glyph_the_font_lacks_paints_background_only() {
        // Missing glyphs are ordinary — the desktop will meet code points no
        // 8x16 table covers. Painting the background and moving on is the
        // behaviour; panicking, or reading a neighbouring glyph's bytes, is not.
        let font = diagonal_font();
        let mut grid = Grid::new(1, 1);
        grid.set(0, 0, Cell { glyph: 'Z', fg: 0x00ff_0000, bg: 0x0000_ff00 });

        let mut pixels = vec![0u32; 2 * 2];
        rasterize(&grid, &font, &mut pixels, 2);

        assert_eq!(pixels, vec![0xff00_ff00; 4]);
    }

    #[test]
    fn a_window_placed_entirely_off_screen_draws_nothing() {
        let content = surface(&["zz"]);
        let grid = compose(2, 2, &[Window {
            rect: Rect { x: 5, y: 5, width: 2, height: 2 },
            content: &content,
        }]);

        assert_eq!(grid.to_text(), "  \n  ");
    }
}
