//! JH7110 GMAC reconnaissance — the read-only probe behind `workload=gmac-probe`.
//!
//! What to read, and what the words mean, is the host-tested
//! `kernel_devices::gmac`; this file is the thin `unsafe` MMIO glue that applies it.
//! Same split as [`crate::device::pwmdac`] over `kernel_devices::pwmdac`. Design:
//! `docs/vf2-gmac-design.md` ("Rung −1"); plan: `plans/vf2-gmac-driver.md`.
//!
//! **This module writes nothing.** The state it captures is what U-Boot left behind
//! after netbooting the kernel, and the driver's first reset assert destroys it — so
//! the reads cannot live inside bring-up, and a probe that perturbed what it measured
//! would be worse than no probe.
//!
//! **Board-only.** `0x1604_0000` is JH7110; under QEMU `virt` and snemu nothing
//! answers there. Unlike [`crate::device::pwmdac`] this is not address-driven and has
//! no emulated counterpart, which is why no itest scenario selects it.

use kernel_devices::gmac::{
    gmac1_resets_released, is_expected_core, snps_version, Region, Target, PROBE_REGIONS,
};

/// Read one register and report it. The breadcrumb is emitted **before** the access,
/// never after: the failure this guards is a bus hang on a gated peripheral, which
/// produces no output at all, so a trailing breadcrumb would say nothing about where
/// the boot stopped. The last `read ...` line on the wire names the register that
/// hung.
fn read_reported(region: &Region, target: &Target) -> u32 {
    let addr = region.base + target.offset;
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: read {}.{} @ {addr:#x}",
        region.label,
        target.label,
    ));
    // SAFETY: `addr` is inside `region`, which `every_region_is_covered_by_a_declared_
    // megapage` proves is covered by a megapage `probe` mapped above, and
    // `every_target_lies_inside_its_region` proves the offset does not run past it.
    // A `u32` MMIO register read is naturally aligned (every offset in the table is a
    // multiple of 4) and has no side effects on this device.
    let value = unsafe { (addr as *const u32).read_volatile() };
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: {}.{} = {value:#010x}",
        region.label,
        target.label,
    ));
    value
}

/// Dump what U-Boot left configured, then stop. Nothing is written, and nothing
/// depends on the result — this is reconnaissance whose output is read by a human
/// (or the board bridge) off the UART.
/// The megapages this needs are inserted by `kmain` pre-MMU from
/// `kernel_devices::gmac::PROBE_MEGAPAGES`, alongside SYSCRG's — see the
/// `mmio_regions` block there. Nothing is mapped here.
///
/// `dtb` must still be borrowable — this runs before `kmain` tears down the
/// identity gigapage the DTB lives in.
pub fn probe(dtb: &fdt::Fdt) {
    crate::tracing::emit_log("gmac-probe: start");

    // The device tree first, deliberately: none of it can hang the bus, and its
    // answers say whether the MMIO reads below are even worth attempting.
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: dtb dma-noncoherent={} (true means STOP — no Zicbom on this core)",
        crate::dtb::dma_is_noncoherent(dtb),
    ));
    crate::dtb::report_gmac_nodes(dtb);

    for (region_index, region) in PROBE_REGIONS.iter().enumerate() {
        for (target_index, target) in region.targets.iter().enumerate() {
            let value = read_reported(region, target);

            // The MAC's version register is first, and its verdict gates belief in
            // everything after it: a wrong base or a still-asserted reset yields
            // plausible words rather than an error. The dump continues either way —
            // the point is that it says which case it is.
            if region_index == 0 && target_index == 0 {
                crate::tracing::emit_log(&alloc::format!(
                    "gmac-probe: core version {:#04x} — {}",
                    snps_version(value),
                    if is_expected_core(value) {
                        "expected dwmac-5.20; the rest of this dump can be believed"
                    } else {
                        "NOT dwmac-5.20; base or offset is wrong, treat the rest as noise"
                    },
                ));
            }

            if target.label == "rst_gmac1_status" {
                crate::tracing::emit_log(&alloc::format!(
                    "gmac-probe: gmac1 resets {}",
                    if gmac1_resets_released(value) { "released" } else { "ASSERTED" },
                ));
            }
        }
    }

    crate::tracing::emit_log("gmac-probe: done");
}
