//! Device drivers: the ns16550a UART (`uart`), the human-readable boot-log
//! console built on it (`console`), the virtio-console telemetry transport
//! (`virtio_console`), the `fw_cfg` guest-configuration channel (`fwcfg`),
//! and the `ramfb` display bring-up built on it (`ramfb`).
//!
//! Re-exported at the crate root (`pub(crate) use device::…`) so call sites stay
//! `crate::uart`, `crate::virtio_console`, etc.

pub mod console;
pub mod fwcfg;
// Board-only: the PLIC base faults snemu's bus, so it stays out of the itest
// build until snemu models a PLIC. See `device/plic.rs`.
#[cfg(feature = "vf2")]
pub mod plic;
pub mod pwmdac;
pub mod ramfb;
pub mod uart;
pub mod virtio_console;
