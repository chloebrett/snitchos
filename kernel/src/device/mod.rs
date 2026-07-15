//! Device drivers: the ns16550a UART (`uart`), the human-readable boot-log
//! console built on it (`console`), the virtio-console telemetry transport
//! (`virtio_console`), the `fw_cfg` guest-configuration channel (`fwcfg`),
//! and the `ramfb` display bring-up built on it (`ramfb`).
//!
//! Re-exported at the crate root (`pub(crate) use device::…`) so call sites stay
//! `crate::uart`, `crate::virtio_console`, etc.

pub mod console;
pub mod fwcfg;
pub mod ramfb;
pub mod uart;
pub mod virtio_console;
