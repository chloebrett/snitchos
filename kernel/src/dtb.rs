//! Device Tree Blob helpers — pull load-bearing values out of the DTB that
//! `OpenSBI` hands us at boot.

use fdt::Fdt;
use kernel_boot::harts::{is_usable, HartInfo};

/// Find the console serial port in the DTB and return its MMIO base plus register
/// layout — `(base, reg_shift, io_width)`.
///
/// Accepts QEMU's `ns16550a` and the JH7110's `snps,dw-apb-uart`; both are
/// 8250-register-compatible and differ only in `reg-shift` (register spacing) and
/// `reg-io-width` (access width). Absent `reg-shift` / `reg-io-width` default to
/// `0` / `1` — QEMU's byte-spaced, byte-wide layout. The offset math these feed is
/// host-tested in `kernel_devices::uart`.
///
/// Runs post-MMU (`kmain` calls it after the trampoline), so the higher-level
/// `fdt` accessors are safe — unlike the pre-MMU MMIO-discovery path.
///
/// Known weakness: panics on a missing/malformed UART node via `.unwrap()`. Fine
/// for boot (no console → no kernel); a real driver would surface the error.
pub fn uart_config(dtb: &Fdt) -> (usize, u8, u8) {
  let uart = dtb
    .find_compatible(&["ns16550a", "snps,dw-apb-uart"])
    .unwrap();
  let base = uart.reg().unwrap().next().unwrap().starting_address as usize;
  // `reg-shift` / `reg-io-width` are 4-byte big-endian `u32`s; decode by hand
  // (the `fdt` 0.1.5 accessor set doesn't surface them).
  let prop_u8 = |name: &str, default: u8| -> u8 {
    match uart.property(name) {
      Some(p) if p.value.len() == 4 => {
        u32::from_be_bytes([p.value[0], p.value[1], p.value[2], p.value[3]]) as u8
      }
      _ => default,
    }
  };
  (base, prop_u8("reg-shift", 0), prop_u8("reg-io-width", 1))
}

/// Whether this machine has QEMU's `fw_cfg` guest-configuration device, per the
/// DTB. QEMU `virt` advertises `fw-cfg@10100000`; the JH7110 has no such node.
///
/// Ask the DTB, never the bus. The tempting alternative — read the device's
/// signature register and see if it answers `QEMU` — is unsafe on real silicon
/// in a way that isn't obvious: a load to an address with no responding slave
/// need not fault or return garbage, it can simply never complete, stalling the
/// hart forever on one instruction. QEMU always answers, so that design tests
/// clean and hangs the board. This is the same discovery discipline the UART
/// already uses; see `uart_config`.
///
/// Known weakness: the `fw_cfg` MMIO base stays hardcoded in
/// `device::fwcfg::BASE_PA` rather than being read from this node's `reg` — two
/// sources for one truth, worth collapsing when something needs the address.
pub fn has_fw_cfg(dtb: &Fdt) -> bool {
  dtb.find_compatible(&["qemu,fw-cfg-mmio"]).is_some()
}

/// True when the DTB marks peripheral DMA as **non**-coherent (`/soc` carrying
/// `dma-noncoherent`). Read by `workload=gmac-probe`.
///
/// This is the single fact the GMAC driver's whole estimate rests on. Mainline's
/// `jh7110.dtsi` omits the property — the JH7110 routes peripheral DMA through a
/// coherent port on the L2, unlike its JH7100 predecessor, which carried it and
/// needed a `SiFive` cache-flush layer. If this ever returns `true` on the board,
/// **stop**: the U74 has no `Zicbom`, so cache maintenance would be a sub-project
/// this kernel has no primitives for. See `docs/vf2-gmac-design.md`, Finding 2.
pub fn dma_is_noncoherent(dtb: &Fdt) -> bool {
  dtb.find_node("/soc").is_some_and(|soc| soc.property("dma-noncoherent").is_some())
}

/// Report what the DTB says about the GMAC nodes over the telemetry channel: which
/// are enabled, their `phy-mode`, and whether a PHY reset GPIO is named. Answers
/// the Phase 0 questions that are about the device tree rather than about MMIO —
/// and unlike the register reads, none of this can hang the bus, which is why the
/// probe does it first.
pub fn report_gmac_nodes(dtb: &Fdt) {
  for path in ["/soc/ethernet@16030000", "/soc/ethernet@16040000"] {
    let Some(node) = dtb.find_node(path) else {
      crate::tracing::emit_log(&alloc::format!("gmac-probe: dtb {path} absent"));
      continue;
    };
    let text = |name| {
      node.property(name).and_then(fdt::node::NodeProperty::as_str).unwrap_or("-")
    };
    crate::tracing::emit_log(&alloc::format!(
      "gmac-probe: dtb {path} status={} phy-mode={} phy-handle={} reset-gpios={}",
      text("status"),
      text("phy-mode"),
      if node.property("phy-handle").is_some() { "yes" } else { "no" },
      if node.property("reset-gpios").is_some() { "yes" } else { "no" },
    ));
  }
}

/// Enumerate the harts the DTB advertises under `/cpus`, filling `out` with one
/// [`HartInfo`] per `cpu@N` node — its `reg` (the `mhartid`) and whether its
/// `status` marks it usable (the JH7110's S7 monitor is `status="disabled"` and
/// comes back `usable = false`; QEMU's harts have no status and come back usable).
/// Writes at most `out.len()` entries and returns how many were written.
///
/// The `usable` decision and the subsequent logical-id assignment are pure and
/// host-tested in `kernel_boot::harts`; this is the thin `fdt` glue, like
/// [`uart_config`]. It runs post-MMU (during secondary bring-up), so the
/// higher-level `fdt` iterators are safe here — unlike the pre-MMU `timebase_hz`
/// path, which deliberately avoids closure chains.
pub fn enumerate_harts(dtb: &Fdt, out: &mut [HartInfo]) -> usize {
  let mut n = 0;
  for cpu in dtb.cpus() {
    if n >= out.len() {
      break;
    }
    let mhartid = cpu.ids().first() as u64;
    let usable = is_usable(cpu.property("status").map(|p| p.value));
    out[n] = HartInfo { mhartid, usable };
    n += 1;
  }
  n
}

/// CPU timebase frequency in Hz, parsed from the `cpus` node's
/// `timebase-frequency` property. Manual decode because `fdt` 0.1.5 doesn't
/// surface it as a typed accessor.
///
/// The property lives on the parent `cpus` node, not on `cpu@0`. Earlier
/// code looked it up via `dtb.cpus().next().properties()` which returns
/// cpu@0's own properties — the result was silently 0. Returns `None` if
/// the property is missing or malformed; 0 is never a meaningful answer,
/// so we make the absence explicit instead of papering over it.
pub fn timebase_hz(dtb: &Fdt) -> Option<u32> {
  // Explicit for-loop rather than .and_then().find().and_then() — the
  // chained-closures form crashes pre-MMU with higher-half link in a
  // way we never isolated. See plans/v0.4-memory-findings.md.
  let node = dtb.find_node("/cpus")?;
  for p in node.properties() {
    if p.name == "timebase-frequency" {
      let bytes = p.value;
      if bytes.len() == 4 {
        return Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
      }
    }
  }
  None
}

/// Print the load-bearing values we extract from the DTB: memory regions,
/// the CPU timebase frequency, and the UART MMIO range. Mostly a v0.1
/// sanity-check — once we wire these into the real allocator, clock, and
/// driver registry, this function goes away.
pub fn print_info(dtb: &Fdt, uart_addr: usize) {
  for region in dtb.memory().regions() {
    crate::println!(
      "memory: {:#x} ({} bytes)",
      region.starting_address as usize,
      region.size.unwrap_or(0),
    );
  }

  if let Some(hz) = timebase_hz(dtb) {
    crate::println!("timebase: {} Hz", hz);
  } else {
    crate::println!("timebase: <missing>");
  }

  crate::println!("uart: {:#x}", uart_addr);
}
