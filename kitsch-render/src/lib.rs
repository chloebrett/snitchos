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

/// Draw `grid` into `pixels` (a `stride`-pixels-per-row XRGB8888 buffer).
///
/// Each cell becomes a `font.width` x `font.height` block: background first,
/// then a run of foreground per horizontal run of set bits. Runs rather than
/// pixels so the work goes through `fill_rect`, which increment 1 made emit one
/// `sw` per pixel — a per-pixel path would give back that win immediately.
pub fn rasterize(grid: &Grid, font: &Font<'_>, pixels: &mut [u32], stride: usize) {
    let (cw, ch) = (font.width, font.height);
    let height = pixels.len() / stride.max(1);
    let mut fb = Framebuffer::new(pixels, stride, height, stride);

    for y in 0..grid.height {
        for x in 0..grid.width {
            let cell = grid.cells[y * grid.width + x];
            let (ox, oy) = (x * cw, y * ch);
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
