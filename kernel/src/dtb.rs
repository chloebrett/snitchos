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

/// Print the load-bearing values we extract from the DTB: memory regions,
/// the CPU timebase frequency, and the UART MMIO range. Mostly a v0.1
/// sanity-check — once we wire these into the real allocator, clock, and
/// driver registry, this function goes away.
///
/// Known weaknesses:
/// - Manually decodes `timebase-frequency` because the `fdt` 0.1.5 crate
///   doesn't surface it as a typed accessor.
/// - Re-runs `find_compatible("ns16550a")` after `uart_addr` already did
///   the same lookup. Cheap but wasted work.
pub fn print_info(dtb: &Fdt) {
  for region in dtb.memory().regions() {
    crate::println!(
      "memory: {:#x} ({} bytes)",
      region.starting_address as usize,
      region.size.unwrap_or(0),
    );
  }

  let timebase = dtb
    .cpus()
    .next()
    .and_then(|c| c.properties().find(|p| p.name == "timebase-frequency"))
    .and_then(|p| {
      let bytes = p.value;
      (bytes.len() == 4).then(|| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    })
    .unwrap_or(0);
  crate::println!("timebase: {} Hz", timebase);

  let uart = dtb.find_compatible(&["ns16550a"]).unwrap();
  let uart_reg = uart.reg().unwrap().next().unwrap();
  crate::println!(
    "uart: {:#x} ({} bytes)",
    uart_reg.starting_address as usize,
    uart_reg.size.unwrap_or(0),
  );
}
