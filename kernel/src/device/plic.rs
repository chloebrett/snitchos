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

/// PLIC interrupt source for UART0 on QEMU `virt`. `// board: derive from DTB.`
const UART_SOURCE: u32 = 10;

/// PLIC context for hart 0's S-mode on QEMU `virt` (context 0 is hart-0 M-mode,
/// context 1 is hart-0 S-mode). `// board: derive from DTB.`
const HART0_S_CONTEXT: u32 = 1;

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
    plic::enable_source(&mut Mmio, HART0_S_CONTEXT, UART_SOURCE);
}

/// Claim the highest-priority pending interrupt for hart 0's S-context, or `None`
/// if the claim came back empty. The external-interrupt handler calls this, then
/// dispatches on the source id and [`complete`]s it.
pub fn claim() -> Option<u32> {
    plic::claim(&Mmio, HART0_S_CONTEXT)
}

/// Signal completion of `source`, re-arming it for the next interrupt.
pub fn complete(source: u32) {
    plic::complete(&mut Mmio, HART0_S_CONTEXT, source);
}

/// Whether `source` is the UART's — the one interrupt the kernel routes today.
#[must_use]
pub fn is_uart(source: u32) -> bool {
    source == UART_SOURCE
}
