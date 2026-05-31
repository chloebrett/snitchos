# Post 2 — Driving the UART ourselves

- post 1 cheated: every char went through OpenSBI via `ecall`. Time to drive the hardware directly.
- wrapped `sbi_putchar` in a `core::fmt::Write` impl on a unit struct. unlocks all of `write!` / `writeln!` / format args.
- `Write::write_str` takes `&mut self` even though my writer has no state — trait has to be conservative; some writers really do mutate.
- `core::fmt::Result` has no useful payload by design (no portable writer could fill it in). `let _ = write!(...)` is fine.
- wrote `print!` / `println!` as `macro_rules!`. two arms in `println!`: empty pattern for `println!()`, `$($arg:tt)*` for everything else. dispatch happens at compile time — can't `if args.is_empty()` at runtime, the format string is source code.
- `$crate::` inside a macro is "this crate, wherever I'm invoked from."
- improved panic handler: `println!("Kernel panic: {}", info)` before `wfi`. `info: &PanicInfo` is `Display`.
- exactly one `#[panic_handler]` per no_std binary.

## xtask

- tired of typing `qemu-system-riscv64 -machine virt -cpu rv64 ...` by hand.
- xtask is **not** a cargo feature. it's:
  - a regular bin crate in the workspace
  - a `.cargo/config.toml` alias: `xtask = "run --package xtask --"`
  - now `cargo xtask up` builds the kernel and runs QEMU
- ~50 lines of `std::process::Command`. no clap yet, two subcommands isn't worth it.
- writing build scripts in rust > bash because: same language, cross-platform, future clap.

## device tree

- the kernel can't know hardware layout from the architecture alone. RAM base, UART base, timer freq — board-level facts.
- DTB = binary, hierarchical hardware description. OpenSBI puts the physical address in `a1`.
- dumped QEMU's with `-machine dumpdtb=virt.dtb`, decompiled with `dtc`.
- three load-bearing values: memory base+length, UART base+length, `timebase-frequency` from `cpus` node.
- naming gotcha: the UART is named `serial@10000000`. linux DT convention names by function, not vendor. `compatible = "ns16550a"` is the actual matcher.
- used the `fdt` crate. manual decode for `timebase-frequency` because the crate didn't expose it as a typed accessor.

## ns16550a + MMIO

- 1987 national semiconductor PC UART. "16550-compatible" = generic UART. QEMU has a software model, not a physical chip.
- 8 registers, 1 byte each. for polled tx: THR (offset 0, write byte) + LSR bit 5 (offset 5, "tx ready").
- MMIO ≠ memory. reading can have side effects. compiler must not optimize.
- `read_volatile` / `write_volatile` on raw pointers — in `core::ptr`, no_std friendly. always `unsafe`.
- store base as `usize` not `*mut u8` — sidesteps the `!Sync` default. struct is just an integer wearing a type.
- `putchar`: spin on `LSR & 0x20 == 0`, then write to THR. three lines of unsafe.

## the bit-number bug

- wrote `0b00010000` for "bit 5" (THR empty). actually wrote bit 4.
- the confusion: assumed 0b00010000 = 2^5, it's actually 2^4.
- bit numbering is 0-indexed — "bit N" is the one with value 2^N. so:
  - bit 0 = value 1
  - bit 4 = value 16 = `0b00010000`
  - bit 5 = value 32 = `0b00100000`
- "bit 5" in human counting (5th from the right, 1-indexed) is bit 4 in machine counting. easy to slip.
- bit 4 in LSR is "break interrupt" — almost never set. result: every byte after the first dropped silently.
- moral: when masking against "bit N," the mask is `1 << N`, not "the Nth bit I'm typing." double-check 2^N.

## why volatile in the spin loop is load-bearing

- without volatile, compiler observes "you read lsr once, value didn't change in your code, why bother again" → infinite loop on the initial value → deadlock.
- volatile says "this access has side effects you can't see."

## spin::Once + spin::Mutex

- `Write::write_str` is `&mut self`, `putchar` is `&self`. need to make a global writer work.
- two options:
  - construct fresh `Uart16550` per call (cheap! just a usize). no serialization.
  - wrap in mutex, `.lock()` for `&mut`.
- went with `spin::Once<spin::Mutex<Uart16550>>`.
- `Once` = no_std OnceCell, one-shot init via `call_once`.
- `Mutex` = spin lock, will actually serialize once we have SMP or interrupts.
- pre-init fallback in the macros: if `UART.get()` is None, construct from hardcoded `0x10000000`. lets the panic handler print during early-boot panics.

## module split

- main.rs grew to ~140 lines. split into:
  - main.rs — entry, kmain, panic
  - console.rs — UART static + init + macros
  - uart.rs — Uart16550 driver
  - dtb.rs — DTB helpers
- macros use `#[macro_export]` → hoisted to crate root. `$crate::console::UART` works from anywhere.

## panic handler hardening

- two real fragilities when panic goes through the same `println!` as happy path:
  - **lock deadlock** — panic mid-`writeln!` would re-enter, try to relock the held mutex.
  - **infinite recursion** — buggy `Display` impl panics inside format args.
- fixes:
  - panic handler bypasses the mutex. fresh `Uart16550`, write directly. interleaving is OK — we're dying.
  - `static PANICKING: AtomicBool`. `swap(true, Relaxed)` on entry; if it was already true, skip the print.

## marking `new` unsafe

- `Uart16550::new(addr)` was secretly unsafe — wrong addr → write into random memory.
- marked `pub const unsafe fn new` with a `# Safety` block. doesn't change runtime, but the call sites now have `unsafe { }` blocks that show up in `grep -rn unsafe`.
- "shows up in audits" — i scoffed, then got it.

## what i learned

- linker scripts and entry assembly were _the_ learning, not boring detail. write them by hand once.
- bit-numbering off-by-ones are silent and devastating.
- volatile in Rust is a property of the access, not the type. more honest than C.
- `unsafe` is documentation, not just permission.
- defer the right things — interrupt-driven tx, FIFO config, RX, baud-rate setup all wait for their milestone. `# Known weaknesses` blocks in doc comments are how i tracked them without doing them.

## next

- protocol crate. wire format design (postcard `Frame` enum). TDD on hosted unit tests. then the second virtio-console for telemetry, then host-reader. killer feature in line of sight.
