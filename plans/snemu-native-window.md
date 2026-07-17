# Plan: `--window` — a live native window for snemu's framebuffer

**Branch**: main (project works directly on main; the user commits)
**Status**: Active

Follow-up to the shipped ramfb work (`plans/legacy/snemu-ramfb-model.md`). Right now
the only way to see the framebuffer is `--dump-framebuffer out.ppm` — one static image
after the run ends. This plan adds `--window`: a live `minifb` window updated
periodically while the guest runs. See `project_snemu_native_window` memory for the
minifb-vs-winit decision (minifb now; winit deferred until resizable/multi-window/
edge-triggered-input is a real need).

## Goal

`snemu --ramfb --window kernel.elf` pops a real window showing the framebuffer,
updating live as the guest presents, closable via the window's close button or Esc.

## Why `minifb`, and how it integrates

`minifb` has no event loop of its own — `window.update_with_buffer(&buf, w, h)` both
pumps OS events and redraws in one call, so it drops straight into the existing
`while steps < max_steps { machine.step()?; ... }` loop with no restructuring of
`main()`. No new threading, no channel, no takeover of `main()` (the `winit` cost this
plan is explicitly avoiding).

## Design

- **Dependency**: `minifb`, scoped `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`
  — same scoping `libc` already uses for the JIT. Windowing is meaningless in a wasm
  build; this keeps the browser path (Backend A) dependency-free, matching the crate's
  existing convention.
- **Shared pixel decode**: `render_ppm` (from the `--dump-framebuffer` work) and the new
  minifb buffer function both need "XRGB8888 LE bytes → RGB channels" — factor the
  per-pixel decode into one private helper both call, rather than duplicating it.
  `render_ppm` packs to 3-byte RGB triples; minifb wants one `u32` per pixel
  (`0x00RRGGBB`, native-endian) — different packing, same decode.
- **Update cadence**: calling `update_with_buffer` every single `step()` would pump OS
  events (and redraw) far more often than useful and tank throughput. Throttle to every
  N steps (a simple constant to start, e.g. every 200_000 steps — tune once it's
  running; not worth over-engineering before it's observable).
- **Exit**: if the window closes (`!window.is_open()`) or Esc is pressed
  (`window.is_key_down(Key::Escape)`), stop stepping and exit cleanly — same as hitting
  `max_steps`, not an error.

## Testing doctrine

The pixel-decode refactor is pure and host-tested (RED→GREEN, extending
`snemu/src/framebuffer.rs`'s existing test suite). The window loop itself is CLI glue
(owns a real OS window) — untestable, thin, mirrors `--dump-framebuffer`'s existing
untested wrapper in `main.rs`. No MUTATE gate (`snemu` isn't in `xtask mutants`'s
package list — consistent with the rest of this plan's precedent).

## Acceptance Criteria

- [ ] `render_ppm` and the new minifb buffer function share one pixel-decode helper —
      no duplicated XRGB8888-decode logic.
- [ ] The minifb buffer function is pure, host-tested, and produces the correct
      `0x00RRGGBB` value for a known input pixel (reuse `render_ppm`'s test fixtures:
      the `0x20_20_40` clear color, stride-padding case).
- [ ] `snemu --ramfb --window <kernel>` opens a real window and updates it live as the
      guest presents (manually verified — the human-in-the-loop check automation can't
      replace, same as the PPM's eyeball check).
- [ ] Closing the window or pressing Esc exits `snemu` cleanly (no panic, no hang).
- [ ] `--window` without `--ramfb` (or before the guest's first present) doesn't panic
      — shows an empty/black window until a config is captured, same graceful-absence
      spirit as `--dump-framebuffer`'s "no framebuffer captured" message.

## Steps

### Step 1: shared pixel-decode helper (refactor, host-tested)

**Acceptance criteria**: `render_ppm`'s existing test suite still passes unchanged
after extracting the per-pixel decode into a shared helper; no behavior change.
**RED**: none needed — this is a refactor under existing green tests. Run the existing
`framebuffer::tests` suite before touching anything to confirm the baseline.
**GREEN**: extract `fn decode_pixel(pixels: &[u8], offset: usize) -> (u8, u8, u8)`
(returns `(r, g, b)`, degrading missing bytes to `0` — same graceful-degradation
`render_ppm` already has) from `render_ppm`'s inner loop; `render_ppm` calls it.
**Done when**: all existing `framebuffer::tests` still green, no new test needed yet
(the helper has no new behavior — it's the same logic, relocated).

### Step 2: `to_minifb_buffer` — pure, host-tested

**Acceptance criteria**: given the same pixel fixtures `render_ppm`'s tests already
use, `to_minifb_buffer(pixels, width, height, stride) -> Vec<u32>` produces
`(r << 16) | (g << 8) | b` per pixel, row-major, respecting stride padding exactly
like `render_ppm`.
**RED**: tests mirroring `render_ppm`'s (`single_pixel_...`, `stride_wider_than...`,
`multi_row_multi_column_...`) but asserting `u32` values instead of PPM bytes — reuse
the same fixture inputs so the two functions are provably testing the same decode.
**GREEN**: `to_minifb_buffer` — same iteration shape as `render_ppm`, calls
`decode_pixel`, packs into `u32` instead of pushing 3 bytes.
**Done when**: criteria met, both pixel-format tests green.

### Step 3: `--window` CLI flag

**Acceptance criteria**: see plan-level acceptance criteria (window opens, updates
live, closes cleanly, no panic without a captured config).
**RED/GREEN**: no host test (real OS window) — covered by manual verification per the
testing doctrine above.
**GREEN**: add `minifb` dependency (wasm-scoped). `--window` flag on `Cli`. In `main`'s
step loop: every `WINDOW_UPDATE_INTERVAL` steps, if `window` is `Some`, call
`machine.dump_framebuffer()` → `to_minifb_buffer` (falling back to a black buffer of
the configured or a default size if no config captured yet) → `window.update_with_buffer`.
Break the loop early if `!window.is_open()`.
**Done when**: criteria met, manually verified (open a real kernel, watch it live,
close it, confirm clean exit).

## Pre-PR Quality Gate

1. `cargo test -p snemu` — full pass, including the new `to_minifb_buffer` tests.
2. `cargo xtask clippy` clean.
3. `cargo xtask snemu-itest` — 114/114 unregressed (the window flag is opt-in CLI-only,
   shouldn't touch itest paths at all, but confirm).
4. Manual run: `cargo run -p snemu --bin snemu --release -- --ramfb --window --native-ops --jit <kernel>` —
   watch the window update live, close it, confirm clean exit.

## Out of scope (follow-up)

- **`winit`/`softbuffer`** — deferred per `project_snemu_native_window` memory until
  resizable/multi-window/edge-triggered-input is a real requirement.
- **Feeding window input back into the guest** — this plan is display-only. Routing
  `minifb`'s polled key/mouse state into the guest's UART RX (or a future virtio-input
  model) is a separate milestone.
- **Configurable update cadence** — a fixed constant for now; a `--window-interval`
  flag is easy to add later if the default throttle proves wrong.

---
*On completion, move this file to `plans/legacy/` (`git mv`) rather than deleting it —
see the SnitchOS override of the planning skill's default in `.claude/CLAUDE.md`.*
