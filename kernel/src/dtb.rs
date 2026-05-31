//! Device Tree Blob helpers — pull load-bearing values out of the DTB that
//! OpenSBI hands us at boot.

use fdt::Fdt;

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

/// CPU timebase frequency in Hz, parsed from the `cpus` node's
/// `timebase-frequency` property. Manual decode because `fdt` 0.1.5 doesn't
/// surface it as a typed accessor.
///
/// The property lives on the parent `cpus` node, not on `cpu@0`. Earlier
/// code looked it up via `dtb.cpus().next().properties()` which returns
/// cpu@0's own properties — the result was silently 0.
pub fn timebase_hz(dtb: &Fdt) -> u32 {
  dtb
    .find_node("/cpus")
    .and_then(|n| n.properties().find(|p| p.name == "timebase-frequency"))
    .and_then(|p| {
      let bytes = p.value;
      (bytes.len() == 4).then(|| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    })
    .unwrap_or(0)
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

  crate::println!("timebase: {} Hz", timebase_hz(dtb));

  crate::println!("uart: {:#x}", uart_addr);
}
