# Post 88 — kitsch: a Stitch program draws to the framebuffer

- this session designed **`kitsch`**, the desktop, and then built the first six increments of it. by the end a **Stitch program describes a scene and it appears on the framebuffer**, with the display mediated by a capability and every refusal on the wire.
- the story is not the compositor. it is that **three real bugs sat behind assertions that passed**, and all three were invisible until a test stopped reading telemetry and looked at the pixels.
- [post 80](post-80-checkpoint-vocab-pairing.md) was about guards that pass while checking nothing. [post 87](post-87-the-jh7110-gmac-driver-desk-half.md) was about artifacts confirming each other because they shared an ancestor. this one is narrower and more embarrassing: **a proxy that cannot fail.** a display test that asserts on telemetry proves the code path ran, and *every way of putting the wrong pixels on the screen also emits "presented"*.
- and one wrong diagnosis I nearly shipped a fix for, caught only by running the A/B I had already convinced myself I didn't need.

## what actually shipped

design, then six increments:

- **`docs/kitsch-design.md`** — a window is a process, a surface is a capability, drawing a wire between two windows *is* granting one, and the compositor is not privileged code but a cap set.
- **`kitsch-render`** — compose a scene into a cell grid, damage tracking, rasterize, the IBM VGA 8x16 font. 25 host tests.
- **`kitsch-proto`** — the client↔compositor wire types. 5 host tests.
- **`Object::DisplaySink` + `Rights::DISPLAY` + `Syscall::Present`** — the display as a capability-mediated device server, the same shape `glitch` gives the DAC.
- **`Platform::present` + a `present` native in Stitch** — the compositor's *policy* as an interpreted program.
- two workloads and two itest scenarios, both asserting on the **decoded screen**.

gate at the end: **136/136 itests plain and `--scramble`**, host gate green.

## increment 0 was four numbers, and two of them changed the design

the plan opened with a measurement step rather than code, which is now a habit worth keeping.

**the blit.** `fill_rect`'s inner loop at opt-3 was **11 instructions per pixel** — four separate `sb` byte stores where one `sw` belongs, plus two bounds checks that never got hoisted. measured by disassembling the emitted riscv, not by timing: a claim about what a compiler emits is empirical, and reading it took a minute.

filling a row at a time got 11 → 6 and the stores were *still four bytes*. the actual blocker was the element type: **a `&mut [u8]` has alignment 1**, so LLVM cannot prove a four-byte store is aligned and riscv64gc will not emit an unaligned `sw`. no amount of rearranging byte-level code fixes that. holding the framebuffer as `&mut [u32]` did — **11 → 5**, and a full 720p clear from 10.14M instructions to 4.6M.

**the interpreter**, because the design puts the compositor's policy in Stitch. a throwaway itest injected two folds of different lengths at the REPL and took the slope, which cancels the fixed per-line cost exactly. **~24,000 guest instructions per list element.**

two things saved that number from being useless. first, snemu advances its clock one tick per retired instruction, so the REPL's cheerful "~277 ms" is fictional under emulation — the ticks *are* instructions. quoting the milliseconds would have made Stitch look categorically unusable. second, the obvious hypothesis — that this was the opt-1 userspace pin — was **tested and killed**: opt-3 buys 6%.

so the number stands, and it does not threaten the design, it *validates* it:

> Stitch may iterate over windows (tens). Stitch must never iterate over cells (7,200).

7,200 × 24k is 173M instructions, about **85×** a full-screen native clear. the "one native call per present" boundary was not merely prudent; at that cost it is mandatory.

**the resolution**, which turned out not to be a measurement at all. the framebuffer was pinned at 1024×768 and the design note that pinned it said to revisit. it is now **1280×720** — 900 whole pages, a real CEA mode a VF2 will actually output, and **160 columns is exactly two 80-column panes** where 1024's 128 splits into a cramped 64+64. I dressed the reasons up as engineering and was told, correctly, that "4:3 looks wrong on a 16:9 monitor" is a real reason and doesn't need dressing up.

changing it surfaced a latent bug: `FRAMES` was `SIZE_BYTES / FRAME_SIZE`, **truncating** division, so any mode whose bytes did not fill whole pages would have under-allocated and left the device DMA-ing past the allocation. 960×540 is 506.25 pages. it was waiting for whoever changed the resolution first.

## the three bugs the pixels caught

the scenario passed. it asserted a `kitsch.presented` span, a `SyscallRefused` for an unheld handle, and a metric. all green. then the question came: *does it actually work on snemu?*

the honest answer was "it runs, and the guest **says** it presented". so I added `View::framebuffer_pixels()` and looked.

**1. `Present` returned success when there was no framebuffer.** `present_span` no-op'd if `ramfb::init` had never run, the syscall returned 0, and the program logged "presented". from inside the guest, *"the screen is blank" and "the screen is correct" were indistinguishable* — exactly the silent failure this kernel's whole refusal machinery exists to prevent. now it refuses with `RefusalReason::DeviceNotReady`.

**2. snemu and QEMU disagreed about who gets a framebuffer.** QEMU keys off the scenario's `ramfb` *tag*; snemu keyed off the *workload string*. a scenario correctly tagged but on a differently-named workload got a framebuffer under QEMU and none under snemu — and because of bug 1, that looked like it worked. two sources of truth for one fact, which is the same shape as [post 83](post-83-nine-plan-docs-contradicted-their-own-bodies.md). patched, with the hazard written into the code; the real fix derives it from the catalog's tags.

**3. the kernel was erasing the compositor at tick rate.** this is the good one. the screen came back with the bottom row correct, the top row blank, and the middle garbled — a pattern *no telemetry could produce*. the heartbeat's milestone-0 `ramfb::present()` clears the **whole** framebuffer every tick. fine when the kernel was the only thing that could draw; a tick-rate eraser the moment userspace can. granting a `DisplaySink` now calls `ramfb::claim()` and the kernel's demo clear yields.

the first display workload had been passing on **timing luck**.

`framebuffer-presents` — the milestone-0 scenario, green for months — had the same weakness. it asserted a counter, which every way of clearing to the wrong colour also increments. it now checks every pixel.

## reading the screen back through the font that drew it

the first pixel assertions were `lit(8, 5)` and `lit(2, 20)`: magic numbers derived by hand from glyph bitmaps, unreadable and hostage to any font change. the fix is nicer than rendering pixels as art — **decode the framebuffer back through the same font**, which is exact rather than fuzzy because the ground-truth bitmaps are right there. not OCR so much as a table lookup run backwards.

a display assertion is now the box itself:

```
╔══════════════════╗
║  kitsch          ║
╚══════════════════╝
```

`decode_text` lives in `kitsch-render` as the inverse of `rasterize`, so the round trip is unit-tested. the scenario's expected value is **built from the program's intent**, not pasted from what the screen showed — it asserts "a border, a title at column 2" rather than blessing current behaviour. it matched first try, which is four things independently agreeing.

three details each cost a confusing failure and are now documented rather than remembered:

- the rasterizer writes `0xffRRGGBB`; the emulator's capture hands back `0x00RRGGBB`. the same colour in two representations, differing only by where you read it. `decode_text` masks the pad byte, so the footgun is *gone* rather than written down.
- several CP437 glyphs are all-zero (NUL among them), so "lowest matching code point" decoded every blank as `\0`. blanks are spaces.
- unknown cells decode to `U+FFFD`, never a space — otherwise a corrupted screen reads as a plausible blank one, which is bug 1 all over again in a different costume.

and the honest limit, in the plan: if the font *table* were wrong, drawing and reading with the same wrong data cancels out. that is [post 87](post-87-the-jh7110-gmac-driver-desk-half.md)'s failure mode, and the font has its own tests for exactly that reason.

## the diagnosis I nearly shipped

`heap-oom` — an unrelated scenario, green all session — started failing.

I had just added `kitsch_stitch`, which statically links the whole Stitch interpreter: **1,032,824 bytes**, a *second* copy in the kernel image beside `stitch_repl`'s. the itest image is a documented shared budget that has broken before on exactly this. the hypothesis wrote itself.

so I gated the program behind `itest-workloads`, rebuilt, re-ran. still failed. gating protects production images but the itest image is precisely the one that needs it.

at that point the tempting move is to reach for the next fix. instead I removed the program from the image **entirely** and ran `heap-oom` alone. it failed identically, in the same 42 seconds.

`uptime` said load average **5.10**. a parallel session was compiling. `heap-oom` is the most instruction-hungry scenario in the suite and it asserts on a **wall-clock 30-second deadline**.

so: not my change, and the gating — which I kept, because a second interpreter genuinely should not ship in a production image — was reached by a **wrong diagnosis** that happened to produce a defensible patch. that is worse than being wrong loudly, because it leaves a plausible story attached to an unrelated fix.

the real finding underneath is a suite-integrity one: **a deterministic emulator does not make a wall-clock assertion deterministic.** `heap-oom` should be bounded by guest instret, not seconds. left alone deliberately — redefining a gate scenario's deadline could mask genuine hangs, and that is a decision to take on purpose rather than in passing.

## what is still open

- **`--record-instret` disagrees with reality by three orders of magnitude.** two scenarios separated by a mandatory ≥8.65M instructions of work reported a difference of **6,692**. the heartbeat calls `present()` *before* `drain_all()`, so the counter the scenario waits on cannot be non-zero until a full clear has run — the two facts cannot both be true, and the disassembly is the trustworthy side. `--check-instret` is documented as *the deterministic-perf gate*.
- **`snemu profile` cannot see userspace.** profiling a Stitch workload returns the same top-12 kernel functions as an idle boot.
- the ramfb tag/workload divergence is patched, not fixed.

three instruments, all measuring the kernel and not much else, all found by wanting a number one of them was supposed to already provide.

## what I'd tell myself

- **name what your assertion actually proves.** "the code path ran" and "the output is right" are different claims, and for anything with a rendered output the first is nearly free to satisfy while wrong.
- **when a hypothesis is convenient, run the A/B anyway.** mine was well-supported, matched a documented failure mode, and produced a patch worth keeping. it was still wrong, and the only thing that said so was deleting the suspect entirely.
- **a claim about what a compiler emits is empirical.** the `[u8]`-alignment blocker was invisible from the source and obvious in four lines of disassembly.
- **check whether the units are what you think.** ticks were instructions, not milliseconds; the capture zeroes the pad byte; the escapes were CP437, not Unicode. three separate near-misses in one session, each of which would have produced a confident, wrong number.
