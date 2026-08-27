# `ibm-vga-8x16.bin` — provenance and licence position

**What it is**: the IBM VGA 8x16 text-mode font, as extracted from a VGA BIOS ROM.
256 glyphs × 16 rows × 1 byte = **exactly 4,096 bytes**, one byte per glyph row,
**MSB is the leftmost pixel**. Code page 437 repertoire, so it carries ASCII,
single- and double-line box drawing, block elements, shading and arrows — the whole
vocabulary a cell desktop's window furniture needs, in one page of memory.

**Where this copy came from**: [`spacerace/romfont`](https://github.com/spacerace/romfont)
`font-bin/IBM_VGA_8x16.bin`, a project that disassembles BIOS and VGA ROMs and
publishes the extracted font tables.

## Why not the obvious copy

The most convenient source is the Linux kernel's `lib/fonts/font_8x16.c`, which
carries the same bitmaps. **That file is `SPDX-License-Identifier: GPL-2.0`**, so
vendoring it would attach GPL-2.0 to this repository. A ROM extraction is the same
bytes without the GPL-licensed wrapper, which is why this copy is the one here.

## The licence position, stated plainly

The bitmaps originate in IBM's VGA BIOS ROM. The position this repository takes —
the same one taken by the many projects that ship these bytes — is that **bitmap
font data of this kind is not a copyrightable work**: in US law typeface *designs*
are not protected, and a bitmap table is data rather than a program or a drawing.

That is a legal position, not a licence grant, and it is recorded here rather than
assumed silently. `spacerace/romfont` states no explicit licence. If this repository
is ever open-sourced ([`plans/open-sourcing-extractables.md`](../../plans/open-sourcing-extractables.md)),
this file is one to look at again — and swapping it is cheap, because
`kitsch-render` treats a font as *data behind a `Font` value*, not as code. Any
8×16 table in the same layout drops in.

Alternatives with explicit permissive terms, if the position is ever unwanted:
Terminus (OFL), Spleen (BSD-2-Clause), or unscii (declared public domain by its
author, though it carries no licence file). All look different from the IBM font;
none is a drop-in for the CP437 *repertoire* without checking glyph coverage.
