//! PLIC MMIO adapter — the volatile-register `PlicTransport` impl over the
//! host-tested sequence logic in `kernel_devices::plic`. Mirrors `fwcfg`'s `Mmio`:
//! this module owns only the register pokes; *what* to write and *in what order*
//! lives in the pure crate.
//!
//! Runs everywhere: QEMU `virt` and the JH7110 have a real PLIC, and snemu now
//! models one, so this is exercised in the itest gate too. Inert until the UART's
//! THRE interrupt is enabled (see the trap handler) — [`init`] just routes the
//! source, and nothing asserts until a device raises its line.

use kernel_devices::plic::{self, PlicTransport};

/// PLIC MMIO base. Hardcoded for QEMU `virt` (`0x0c00_0000`), which is also the
/// JH7110's PLIC base. `kmain` inserts the two megapages covering it (`0x0c00_0000`
/// for priority + enable, `0x0c20_0000` for the hart-0 S-context threshold + claim)
/// into `MmioRegions`, so `mmu::enable` leaf-maps them in the higher-half MMIO
/// window — the mid table only covers the pages that are explicitly inserted.
///
/// `// board:` the exact base and the UART's interrupt source/context should come
/// from the DTB (`/soc/plic`, the UART node's `interrupts`, `interrupts-extended`)
/// — hardcoded here the same way the MMIO regions and UART layout are, pending a
/// DTB-driven pass.
const PLIC_BASE: usize = 0x0c00_0000;

/// PLIC interrupt source for UART0. `// board: derive from DTB.`
///
/// QEMU `virt` numbers UART0 as source 10; the JH7110 numbers it **32**
/// (`jh7110.dtsi`, `serial@10000000 { interrupts = <32> }`).
#[cfg(not(feature = "vf2"))]
const UART_SOURCE: u32 = 10;
#[cfg(feature = "vf2")]
const UART_SOURCE: u32 = 32;

/// Offset of a hart's S-mode context from `2 × mhartid`. `// board: derive from DTB.`
///
/// PLIC contexts are numbered by the `interrupts-extended` order, so the formula
/// depends on which harts contribute which privilege levels.
///
/// - **QEMU `virt`** is symmetric — every hart contributes M then S — so hart `m`
///   owns contexts `2m` (M) and `2m + 1` (S). Offset **1**.
/// - **JH7110** is not: one S7 monitor core plus four U74s, and the S7 contributes
///   *only* an M-mode context. The order is `cpu0 M`, `cpu1 M`, `cpu1 S`,
///   `cpu2 M`, `cpu2 S`, … — the missing `cpu0 S` shifts everything down one, so
///   U74 `m` owns S-context `2m` exactly. Offset **0**.
#[cfg(not(feature = "vf2"))]
const S_CONTEXT_OFFSET: u32 = 1;
#[cfg(feature = "vf2")]
const S_CONTEXT_OFFSET: u32 = 0;

/// The S-mode PLIC context of the hart the kernel booted on.
///
/// **Computed, not a constant, because the boot hart is not fixed.** Two
/// consecutive power cycles of the same board reported secondaries
/// `mhartid 1, 2, 4` and then `1, 3, 4` — i.e. the kernel booted on mhartid 3 and
/// then on 2, because OpenSBI hands off to whichever hart wins the race. Any
/// compile-time context is therefore right only by luck.
///
/// Reads logical hart 0's mhartid, which `kmain` fills in from the DTB before it
/// calls [`init`].
///
/// Getting this wrong is **silent**: the source is enabled in some other context's
/// bitmap, every register write succeeds, nothing faults, and the interrupt simply
/// never arrives. It presents as a working UART with a TX ring that fills and
/// never drains.
fn boot_hart_s_context() -> u32 {
    let mhartid =
        crate::percpu::LOGICAL_TO_MHARTID[0].load(core::sync::atomic::Ordering::Relaxed) as u32;
    2 * mhartid + S_CONTEXT_OFFSET
}

fn base() -> usize {
    PLIC_BASE + crate::mmu::KERNEL_OFFSET
}

/// Adapts volatile PLIC register access to the host-tested [`PlicTransport`],
/// mirroring `fwcfg`'s `Mmio`.
struct Mmio;

impl PlicTransport for Mmio {
    fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: PLIC registers are 32-bit; `offset` comes from the pure layout
        // helpers, and the higher-half MMIO mapping is live post-`mmu::enable`.
        unsafe { ((base() + offset) as *const u32).read_volatile() }
    }

    fn write_reg(&mut self, offset: usize, value: u32) {
        // SAFETY: as `read_reg`.
        unsafe { ((base() + offset) as *mut u32).write_volatile(value) };
    }
}

/// Route the UART's interrupt to hart 0's S-mode context: enable the source at a
/// nonzero priority with the context threshold at 0.
///
/// Inert on its own — the UART won't assert until its THRE interrupt is enabled
/// (a later increment). Call once at boot, after `mmu::enable`.
pub fn init() {
    plic::enable_source(&mut Mmio, boot_hart_s_context(), UART_SOURCE);
}

/// Claim the highest-priority pending interrupt for hart 0's S-context, or `None`
/// if the claim came back empty. The external-interrupt handler calls this, then
/// dispatches on the source id and [`complete`]s it.
pub fn claim() -> Option<u32> {
    plic::claim(&Mmio, boot_hart_s_context())
}

/// Signal completion of `source`, re-arming it for the next interrupt.
pub fn complete(source: u32) {
    plic::complete(&mut Mmio, boot_hart_s_context(), source);
}

/// Whether `source` is the UART's — the one interrupt the kernel routes today.
#[must_use]
pub fn is_uart(source: u32) -> bool {
    source == UART_SOURCE
}
