//! Device Tree Blob helpers — pull load-bearing values out of the DTB that
//! `OpenSBI` hands us at boot.

use fdt::Fdt;
use kernel_boot::harts::{is_usable, HartInfo};

/// Find the first `ns16550a`-compatible serial port in the DTB and return
/// its MMIO base address.
///
/// Known weaknesses:
/// - Panics on missing/malformed entries via `.unwrap()`. Fine for v0.1
///   (no DTB → no kernel), but a real driver would surface the error.
/// - Hardcodes the `"ns16550a"` compatible string. Boards that report
///   `"snps,dw-apb-uart"` or `"arm,pl011"` etc. won't match. Will need a
///   compatibility list when we add a second platform.
pub fn uart_addr(dtb: &Fdt) -> usize {
  let uart = dtb.find_compatible(&["ns16550a"]).unwrap();
  uart.reg().unwrap().next().unwrap().starting_address as usize
}

/// Enumerate the harts the DTB advertises under `/cpus`, filling `out` with one
/// [`HartInfo`] per `cpu@N` node — its `reg` (the `mhartid`) and whether its
/// `status` marks it usable (the JH7110's S7 monitor is `status="disabled"` and
/// comes back `usable = false`; QEMU's harts have no status and come back usable).
/// Writes at most `out.len()` entries and returns how many were written.
///
/// The `usable` decision and the subsequent logical-id assignment are pure and
/// host-tested in `kernel_boot::harts`; this is the thin `fdt` glue, like
/// [`uart_addr`]. It runs post-MMU (during secondary bring-up), so the
/// higher-level `fdt` iterators are safe here — unlike the pre-MMU `timebase_hz`
/// path, which deliberately avoids closure chains.
#[expect(
  dead_code,
  reason = "wired into secondary bring-up in the next step (Step 6 of plans/vf2-b6-multihart.md); the glue lands separately so the enumeration + its pure logic are reviewable in isolation"
)]
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
