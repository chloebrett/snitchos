//! Kernel console: the global UART instance plus the `print!`/`println!`
//! macros that write to it.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use kernel_boot::bootargs::ConsoleMode;

use crate::uart::Uart16550;
use kernel_devices::console::{ConsoleRing, REBOOT_TOKEN, RebootDetector};

/// The kernel's console UART, initialized lazily from the DTB at boot.
///
/// Wrapping in `kernel::sync::Mutex` lets multiple call sites serialize
/// their writes once we have interrupts or SMP — today (single hart, no
/// interrupts) it never actually contends.
///
/// Known weaknesses:
/// - Accessed via the print!/println! macros, which silently fall back to
///   `pre_init_uart()` (via `emergency_uart_base()`) if this hasn't been
///   initialized yet. The base is hardcoded for QEMU `virt`; any other board
///   would lose pre-init output.
/// - No re-entrancy guard. A panic inside a print would try to lock again and
///   deadlock. Real kernels use a recursion-guarded console here.
pub static UART: crate::sync::Once<crate::sync::Mutex<Uart16550>> = crate::sync::Once::new();

/// Initialize the kernel console with the given UART MMIO base
/// physical address (typically pulled from the DTB).
///
/// Translates to the higher-half VA before storing — the kernel runs
/// at higher-half PC after the trampoline, and `mmu::unmap_identity`
/// later tears down the identity MMIO mapping. The MMIO region is
/// dual-mapped by `mmu::enable`, so this works from the moment
/// `enable` returns.
///
/// Safe to call exactly once; subsequent calls are no-ops thanks to
/// `Once`.
///
/// # Safety
///
/// `uart_base` must be the physical MMIO base of a real 8250-compatible UART with
/// register layout `reg_shift` / `io_width` (from the DTB), and `mmu::enable` must
/// have run (so the higher-half MMIO mapping is live).
pub unsafe fn init(uart_base: usize, reg_shift: u8, io_width: u8) {
  let va = uart_base + crate::mmu::KERNEL_OFFSET;
  // SAFETY: per the fn contract — DTB-derived base + layout, higher-half mapped.
  UART.call_once(|| {
    crate::sync::Mutex::new(unsafe { Uart16550::with_layout(va, reg_shift, io_width) })
  });
}

/// Hardcoded NS16550A physical MMIO base for QEMU `virt`. Used via
/// `emergency_uart_base()` by both the macro fallback (`pre_init_uart`)
/// and the panic handler. Wrong on any other board — see `console.rs`
/// known weaknesses.
pub const QEMU_VIRT_UART_BASE: usize = 0x10000000;

/// Returns a UART driver pointing at the QEMU `virt` UART. Used by
/// the `print!`/`println!` macros when `UART` isn't initialized yet.
/// (The panic handler builds its own UART directly via
/// `emergency_uart_base()` so it doesn't depend on this function.)
///
/// Picks the address space via `emergency_uart_base`: physical when
/// the MMU is off, higher-half when it's on.
///
/// # Safety
///
/// Only safe to call before `console::init` has run — no other writer
/// is using the device yet. Not exported for general use; it's `pub`
/// so the macros can reach it, not because callers should — which is what
/// `#[doc(hidden)]` says. It used to say it with a `_` prefix, but that means
/// "unused" and the macros below use it on every pre-init `print!`.
#[doc(hidden)]
pub unsafe fn pre_init_uart() -> Uart16550 {
  // SAFETY: see function-level doc; precondition is that no other code
  // currently holds the UART.
  unsafe { emergency_uart_at(emergency_uart_base()) }
}

/// Pick the UART MMIO base address that's valid for the current
/// `satp` state. Used by `pre_init_uart` and the panic handler — both
/// can fire at any boot stage, including pre-MMU and post-identity-unmap.
pub fn emergency_uart_base() -> usize {
  let satp: u64;
  // SAFETY: `csrr satp` is a non-trapping read in S-mode.
  unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp) };
  if satp != 0 {
    QEMU_VIRT_UART_BASE + crate::mmu::KERNEL_OFFSET
  } else {
    QEMU_VIRT_UART_BASE
  }
}

/// Register layout for the pre-init / emergency / panic UART. These paths fire
/// before `console::init` reads the DTB, so the layout has to be a compile-time
/// constant. UART0 sits at `QEMU_VIRT_UART_BASE` (`0x1000_0000`) on both QEMU
/// `virt` and the VisionFive 2, but the VF2's DesignWare UART spaces its registers
/// 4 bytes apart (`reg-shift = 2`) and takes 32-bit accesses (`reg-io-width = 4`);
/// QEMU's ns16550a is byte-spaced. Without matching this, an early fault on the
/// board polls the wrong `LSR` offset and prints nothing — see B4 in
/// `plans/visionfive2-port.md`.
#[cfg(not(feature = "vf2"))]
const EMERGENCY_UART_REG_SHIFT: u8 = 0;
#[cfg(not(feature = "vf2"))]
const EMERGENCY_UART_IO_WIDTH: u8 = 1;
#[cfg(feature = "vf2")]
const EMERGENCY_UART_REG_SHIFT: u8 = 2;
#[cfg(feature = "vf2")]
const EMERGENCY_UART_IO_WIDTH: u8 = 4;

/// Construct a UART for the pre-init / emergency / panic paths at `base`, with the
/// board's compile-time register layout ([`EMERGENCY_UART_REG_SHIFT`] /
/// [`EMERGENCY_UART_IO_WIDTH`]). The one constructor these paths share so the board
/// layout lives in exactly one place.
///
/// # Safety
///
/// Same as [`Uart16550::with_layout`]: `base` must be a real 8250-compatible UART
/// base, and no coordinated writer may conflict — sound here because these paths
/// run when nothing else holds the device (pre-init, or a panic that has already
/// stopped the world).
pub unsafe fn emergency_uart_at(base: usize) -> Uart16550 {
  // SAFETY: forwarded to the caller's contract above.
  unsafe { Uart16550::with_layout(base, EMERGENCY_UART_REG_SHIFT, EMERGENCY_UART_IO_WIDTH) }
}

/// Capacity of the console RX ring — bytes buffered between the timer-driven
/// drain and `ConsoleRead`. 256 comfortably absorbs a typed line between
/// sub-second drains; overflow drops the newest bytes (see [`ConsoleRing`]).
const RX_RING_CAP: usize = 256;

/// Buffered console input. The timer drain (producer, hart 0) pushes raw bytes
/// from the UART RX FIFO; the `ConsoleRead` syscall (consumer) pops them.
///
/// Its `Mutex` is safe to take in the timer handler — unlike the println
/// [`UART`] mutex — because it's held only by [`drain_rx`] and `ConsoleRead`,
/// both of which run with `sstatus.SIE == 0` (so they can't nest on one hart),
/// and neither allocates nor emits telemetry. See `kernel_devices::console`.
static CONSOLE_RX: crate::sync::Mutex<ConsoleRing<RX_RING_CAP>> =
  crate::sync::Mutex::new(ConsoleRing::new());

/// Drain the UART receive FIFO into [`CONSOLE_RX`]. Called from the timer handler
/// (hart 0, ~every tick) **and** from the `ConsoleRead` syscall (any hart) so an
/// actively-reading program empties the FIFO between ticks (burst hardening). The
/// `RBR` reads happen **under the [`CONSOLE_RX`] lock**, so concurrent drainers
/// serialize on it — multi-producer is safe even though each drain uses its own
/// unsynchronized RX handle.
///
/// Deliberately does **not** lock the println [`UART`] mutex. A kernel task can
/// hold that mutex for `print!`/`println!` with interrupts enabled; locking it
/// here would deadlock the instant the timer fires mid-print. RX register access
/// (poll `LSR`, pop `RBR`) touches device state disjoint from the TX path's
/// `THR` writes, so a separate RX handle is sound.
pub fn drain_rx() {
  // SAFETY: RX-only access (LSR/RBR), disjoint from the `UART`-mutex-guarded TX
  // path's THR writes; see the fn doc for why this needs no coordination.
  let uart = unsafe { emergency_uart_at(emergency_uart_base()) };
  let mut ring = CONSOLE_RX.lock();
  while let Some(byte) = uart.read_byte() {
    // Watch for the reboot line as the bytes go past. Deliberately *observed*
    // here and *acted on* elsewhere: this runs in the external-interrupt handler,
    // and the reset path emits a telemetry frame, which can intern a string and
    // allocate. Doing that from an ISR is the re-entry deadlock this kernel has
    // already paid for once (see the frame-allocator and IRQ notes in
    // `.claude/CLAUDE.md`). So the ISR raises a flag; the heartbeat acts.
    if REBOOT_DETECTOR.lock().feed(&[byte]) {
      REBOOT_REQUESTED.store(true, Ordering::Release);
    }
    ring.push(byte); // drop-on-full is handled inside the ring
  }
}

/// Watches console input for [`REBOOT_TOKEN`]. Locked rather than per-call so the
/// match survives across reads — the token arrives a keystroke at a time.
static REBOOT_DETECTOR: crate::sync::Mutex<RebootDetector> =
  crate::sync::Mutex::new(RebootDetector::new());

/// Set when [`drain_rx`] has seen the reboot line. Read by the heartbeat, which
/// owns the actual reset (see [`drain_rx`] for why the ISR does not).
static REBOOT_REQUESTED: core::sync::atomic::AtomicBool =
  core::sync::atomic::AtomicBool::new(false);

/// Whether someone has asked the board to reboot over the console.
pub fn reboot_requested() -> bool {
  REBOOT_REQUESTED.load(Ordering::Acquire)
}

/// Withdraw a reboot request. For the one case that survives the reset call:
/// firmware without SRST returns instead of rebooting, and a request left standing
/// would re-attempt on every tick, burning a flush and a log line each time.
pub fn clear_reboot_request() {
  REBOOT_REQUESTED.store(false, Ordering::Release);
}

/// Push every queued TX byte out of the ring and wait for the UART to finish
/// shifting them, polling rather than waiting on the THRE interrupt.
///
/// **For the moment before a reset.** The ring normally drains from the THRE
/// interrupt, but a reset stops that machinery, and bytes still in the ring or the
/// FIFO when the platform resets are simply lost — leaving a truncated frame and
/// an unexplained silence, exactly the diagnostic vacuum a reason frame exists to
/// prevent.
///
/// Bounded on purpose: a wedged UART must not turn "reboot" into "hang forever",
/// which would be a strictly worse failure than the one being escaped. The budget
/// is generous next to a 512-byte ring at 115200 baud (~44 ms) and still finite.
pub fn flush_tx_blocking() {
  /// Polling iterations before giving up. Each iteration writes at most one byte,
  /// so this covers many ring-fulls; a UART that has not made progress by then is
  /// not going to.
  const MAX_SPINS: usize = 1_000_000;

  // A fresh handle, like `drain_tx` — no println `UART` mutex to deadlock on.
  let uart = unsafe { emergency_uart_at(emergency_uart_base()) };
  let mut spins = 0usize;
  loop {
    if spins >= MAX_SPINS {
      return;
    }
    spins += 1;
    if !uart.thre() {
      continue;
    }
    let popped = crate::trap::without_interrupts(|| TX_RING.lock().pop());
    match popped {
      Some(byte) => uart.write_thr(byte),
      // Ring empty and the holding register is free: everything we queued has at
      // least reached the shift register.
      None => return,
    }
  }
}

/// Capacity of the interrupt-driven TX ring — bytes queued between an emitter's
/// [`tx_push`] and the THRE-interrupt [`drain_tx`]. Overflow drops the newest
/// bytes (drop-and-count discipline; the drop is the caller's to count).
const TX_RING_CAP: usize = 512;

/// Bytes queued for interrupt-driven transmit. The producer ([`tx_push`], normal
/// context) pushes and enables the UART's THRE interrupt; the consumer
/// ([`drain_tx`], the external-interrupt handler) drains into the FIFO. Both run
/// on hart 0.
static TX_RING: crate::sync::Mutex<ConsoleRing<TX_RING_CAP>> =
  crate::sync::Mutex::new(ConsoleRing::new());

/// Queue `byte` for interrupt-driven transmit and arm the THRE interrupt, so the
/// UART raises its PLIC line and [`drain_tx`] empties the ring. Non-blocking:
/// drops the byte if the ring is full.
///
/// The push and the interrupt-enable run with S-mode interrupts masked
/// ([`without_interrupts`](crate::trap::without_interrupts)): otherwise a THRE
/// interrupt already pending from an earlier push could fire while we hold
/// `TX_RING`, and `drain_tx` would deadlock re-taking it. `drain_tx` itself runs
/// interrupts-masked (in the trap handler), so it never races this.
pub fn tx_push(byte: u8) {
  crate::trap::without_interrupts(|| {
    TX_RING.lock().push(byte); // drop-on-full inside the ring
    // A fresh RX-layout handle (not the println `UART` mutex — see `drain_rx`);
    // only IER is touched here.
    let uart = unsafe { emergency_uart_at(emergency_uart_base()) };
    uart.set_tx_interrupt(true);
  });
}

/// Queue a whole encoded telemetry frame for transmit, or none of it. Returns
/// `false` if the ring hadn't room, in which case nothing was queued and the
/// caller counts one dropped frame.
///
/// The all-or-nothing contract is [`ConsoleRing::push_all`]'s, and it is why the
/// telemetry path cannot reuse [`tx_push`] in a loop: the wire is a COBS stream,
/// so a partially-queued frame corrupts its successor rather than merely losing
/// itself. Same masking discipline as [`tx_push`].
pub fn tx_push_all(bytes: &[u8]) -> bool {
  crate::trap::without_interrupts(|| {
    let accepted = TX_RING.lock().push_all(bytes);
    if accepted {
      // A fresh RX-layout handle (not the println `UART` mutex — see `drain_rx`);
      // only IER is touched here.
      let uart = unsafe { emergency_uart_at(emergency_uart_base()) };
      uart.set_tx_interrupt(true);
    }
    accepted
  })
}

/// Drain the TX ring into the UART FIFO — the THRE-interrupt handler's body. Push
/// bytes while the transmit register has room; when the ring empties, disable the
/// THRE interrupt (else it would fire continuously on the empty FIFO). Runs with
/// interrupts masked (trap entry), so it never nests with [`tx_push`].
pub fn drain_tx() {
  // A fresh handle, like `drain_rx` — no println `UART` mutex to deadlock on.
  let uart = unsafe { emergency_uart_at(emergency_uart_base()) };
  let mut ring = TX_RING.lock();
  while uart.thre() {
    let Some(byte) = ring.pop() else {
      uart.set_tx_interrupt(false);
      break;
    };
    uart.write_thr(byte);
  }
}

/// Pop up to `dst.len()` buffered input bytes into `dst`; returns how many.
/// The `ConsoleRead` syscall's drain side (consumer). Non-blocking: returns `0`
/// when nothing is buffered.
pub fn read_into(dst: &mut [u8]) -> usize {
  let mut ring = CONSOLE_RX.lock();
  let mut n = 0;
  while n < dst.len() {
    match ring.pop() {
      Some(byte) => {
        dst[n] = byte;
        n += 1;
      }
      None => break,
    }
  }
  n
}

/// Human-console output mode (`console=` bootarg). `0` = [`ConsoleMode::Text`],
/// the default before [`set_console_mode`] runs — so early boot is always text.
static CONSOLE_MODE: AtomicU8 = AtomicU8::new(0);

/// Re-entrancy guard for the frames path. Emitting a `Frame::Log` runs the
/// telemetry TX path (staging + virtqueue + intern locks); a `println!` from
/// inside that path would deadlock, so a re-entrant call drops its line instead.
static IN_FRAMES: AtomicBool = AtomicBool::new(false);

/// Longest kernel log line carried as a `Frame::Log` in frames mode; longer lines
/// are truncated at a UTF-8 boundary by [`kernel_obs::panic_log::MsgWriter`].
const LOG_LINE_MAX: usize = 256;

/// Record the console output mode. Call once from `kmain` after the `console=`
/// bootarg is parsed; before it, output defaults to [`ConsoleMode::Text`].
pub fn set_console_mode(mode: ConsoleMode) {
  CONSOLE_MODE.store(mode as u8, Ordering::Relaxed);
}

/// The current console output mode.
#[must_use]
pub fn console_mode() -> ConsoleMode {
  match CONSOLE_MODE.load(Ordering::Relaxed) {
    1 => ConsoleMode::Frames,
    _ => ConsoleMode::Text,
  }
}

/// The routing behind `print!`/`println!`. Pre-init (console not up) is always
/// raw emergency UART — early output must never depend on the frame sink (the
/// stale-image / `ph!`-markers lessons). Once the console is up, [`ConsoleMode`]
/// decides: `Text` writes the UART as before; `Frames` emits the formatted line
/// as a `Frame::Log` on the telemetry wire, so the wire carries one content type.
///
/// `newline` distinguishes `print!` from `println!`; in frames mode it's implicit
/// (a `Frame::Log` is one line), so `print!` fragments each become their own
/// `Log` — a known coarseness, fine for the whole-line output the kernel emits.
#[doc(hidden)]
pub fn write_console(args: core::fmt::Arguments<'_>, newline: bool) {
  use core::fmt::Write;
  let Some(uart) = UART.get() else {
    // SAFETY: pre-init fallback fires before console::init runs.
    let mut u = unsafe { pre_init_uart() };
    let _ = u.write_fmt(args);
    if newline {
      let _ = u.write_str("\n");
    }
    return;
  };
  match console_mode() {
    ConsoleMode::Text => {
      let mut g = uart.lock();
      let _ = g.write_fmt(args);
      if newline {
        let _ = g.write_str("\n");
      }
    }
    ConsoleMode::Frames => emit_console_frame(args),
  }
}

/// Format `args` into a fixed buffer and emit it as a `Frame::Log`, guarded
/// against re-entry (see [`IN_FRAMES`]). Empty lines are skipped.
fn emit_console_frame(args: core::fmt::Arguments<'_>) {
  use core::fmt::Write;
  if IN_FRAMES.swap(true, Ordering::Acquire) {
    return; // re-entrant emit — drop rather than deadlock
  }
  let mut buf = [0u8; LOG_LINE_MAX];
  let mut w = kernel_obs::panic_log::MsgWriter::new(&mut buf);
  let _ = w.write_fmt(args);
  let line = w.as_str();
  if !line.is_empty() {
    crate::tracing::emit_log(line);
  }
  IN_FRAMES.store(false, Ordering::Release);
}

/// Print formatted output to the kernel console (no trailing newline).
///
/// Routes through [`write_console`]: raw UART pre-init, then per [`ConsoleMode`]
/// once the console is up (UART text, or a `Frame::Log` on the telemetry wire).
#[macro_export]
macro_rules! print {
  ($($arg:tt)*) => {{
    $crate::console::write_console(::core::format_args!($($arg)*), false);
  }};
}

/// Print formatted output to the kernel console followed by a newline.
/// Same routing as `print!`.
#[macro_export]
macro_rules! println {
  () => { $crate::console::write_console(::core::format_args!(""), true) };
  ($($arg:tt)*) => {{
    $crate::console::write_console(::core::format_args!($($arg)*), true);
  }};
}
