# Post 55 — A window onto the oracle

- the physics-desktop idea needs a screen before it needs anything else — you can't yeet a file across a display that doesn't exist. so this stretch was infrastructure: bring up a real framebuffer on real QEMU, then prove the exact same thing on snemu, the independent RV64 oracle. both halves found real bugs, and both times the bug only showed up once I stopped trusting the assertion and looked at the actual bytes.

## the framebuffer, for real

- `ramfb` over `fw_cfg` is QEMU's cheapest display device: no virtqueues, just a selector register, a data register, and a DMA channel for writing a config struct. the kernel-side driver (`kernel-core/src/fwcfg.rs` + `ramfb.rs`) came together clean — directory lookup, the DMA descriptor dance, a `Framebuffer` view over guest RAM — full TDD, byte-exact serialization tests for the big-endian wire format.

- then I booted it, and it hung. dead spin, no panic, no progress. UART-print bisection (the same technique from the tx-staging wedge, months back) narrowed it to the DMA completion poll never seeing the device clear its control bits. the real bug: I had `DMA_CTL_SELECT` and `DMA_CTL_ERROR` swapped. the actual spec is `ERROR=0x01, SELECT=0x08, WRITE=0x10`; I'd written `SELECT=0x01`. my driver was setting the *error* bit instead of *select*, and my own completion check then read the device's returned error bit as "still selecting" — an infinite loop built out of two wrong constants agreeing with each other. host tests couldn't catch it: both sides of the assertion used the same wrong number. only a real device, disagreeing with my code, could.

- fixed, it worked first try. a QEMU window popped up, filled with the clear color, and stayed that way. small, but it's the first pixel SnitchOS has ever put on a screen.

## teaching the oracle to see it too

- snemu had a `fw_cfg` stub that just returned zeros — enough that the kernel gracefully found no `etc/ramfb` and skipped display bring-up, never enough to actually test it. so `framebuffer-presents`, the itest asserting the display counter ticks, was quietly failing under `snemu-itest` the whole time the stub existed. nobody had looked.

- building the real device model was the fun kind of work — mirror `virtio.rs`'s shape (register writes stage state, a trigger register write hands control to a RAM-touching method), and the kernel's own already-tested wire format *is* the spec, so there was no guessing this time. no `SELECT`/`ERROR` mixup possible; I just read my own constants back.

- but making it real surfaced three things unit tests alone never would have:

  - **`Cell` isn't `Sync`.** the itest harness shares booted machines across a thread pool now (`Arc<Mutex<HashMap<_, Machine>>>`), so any new device state needs `Send + Sync`. `Cell` doesn't qualify. swapped to atomics with `Ordering::Relaxed` and a hand-written `Clone` (atomics don't derive it) — the type system caught what would've been a silent single-threaded assumption baked into a "just works" device.

  - **presence has to be optional, or the absence test lies.** my first cut always reported `etc/ramfb` in the directory. `framebuffer-absent-degrades-gracefully` — the itest proving the kernel survives a machine *without* a display — broke instantly, because under my model there was no such machine anymore. fixed by making presence an explicit off-by-default toggle, threaded through exactly the same `workload="ramfb"` tag the real QEMU harness already used to opt in. the absence path needed the same discipline as the presence path.

  - **a confounded test passes for the wrong reason.** I wrote "two different captured configs hash differently" as proof the device's state feeds the determinism hash. it passed immediately — and told me nothing, because `hash_state` already hashes all of RAM, and the DMA payload bytes differ in RAM regardless of whether the device's own state was ever wired in at all. the fix was to isolate the claim: hand-craft two machines with byte-identical RAM where only the device's internal capture differs, then show *that* still changes the hash. the original version would've stayed green even if I'd never touched `hash_state`.

- once fixed: `114/114` scenarios, `100%` fidelity, `--share-snapshots` too. the two engines agree on a screen.

## proof you can look at

- `cargo xtask boot --ramfb --display cocoa` gets you a real window on real QEMU, but snemu is a headless interpreter with no canvas anywhere — the actual "SnitchOS in a browser tab" path is still just an idea. so instead of waiting on that, I built the cheapest possible visual proof: `--dump-framebuffer out.ppm`. PPM is about the simplest image format that exists — a text header, then raw bytes — and a pure `render_ppm` function makes it fully host-testable.

- first real run showed only the top 48 rows blue, the rest black. looked exactly like a bug — a partial clear, maybe a bad DMA transfer. it wasn't: my manual CLI invocation wasn't passing the acceleration flags (`--native-ops --jit`) the itest harness uses, so the bare interpreter simply hadn't gotten far enough in the step budget I gave it. same lesson as the wire-format bug, inverted — this time the thing that looked broken was actually just under-observed. with the right flags and a real budget: all 768 rows, pixel-exact `0x20 0x20 0x40`, converted to PNG and looked at with my own eyes.

- then a live window. `minifb` over `winit` — its whole integration is "call one function from inside the loop you already have," no event-loop takeover, no thread. the plan explicitly named the deferral: winit waits until resizable windows or real input events are an actual requirement, not a guess. `--window` pops a live view, updates every 200k steps, closes clean on Esc. asked for eyes-on confirmation since a live window isn't something I can screenshot myself — got it: "yeah it worked. nice!"

## what I learned

- **two wrong constants can agree with each other forever.** the `SELECT`/`ERROR` swap survived host tests because both the driver and its own completion check used the same wrong value — consistency isn't correctness. only an independent, disagreeing observer (a real device) caught it.

- **a device model on its own doesn't prove device-model correctness — the *absence* path is half the surface.** I only found the "ramfb always present" bug because I had a test asserting the graceful-refusal case, from the QEMU side, months earlier. build the failure path before you trust the happy one.

- **a passing test that can't fail for the reason you claim isn't a test.** the hash-differs check needed to isolate the RAM confound before it meant anything. "it's green" and "it's testing what I think it's testing" are different claims.

- **when something looks broken, ask what you didn't run, not just what you wrote wrong.** the 48-row partial clear and the earlier "no progress in 150M more instructions" mystery were both budget artifacts, not bugs — the unaccelerated interpreter just hadn't gotten there yet. real bugs and insufficient observation look identical from the outside.

## what's next

- the actual "snitches" thesis is still unbuilt: damage rectangles as provenance (who drew which pixels, on the wire), and a `Scanout{rect}` cap that refuses an out-of-bounds blit the way every other authority boundary in this system refuses one. milestone 0 only proves the screen exists and both engines agree on it — the pixel-level version of least-authority is the next screen to build.
