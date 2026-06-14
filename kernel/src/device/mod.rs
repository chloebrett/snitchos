//! Device drivers: the ns16550a UART (`uart`), the human-readable boot-log
//! console built on it (`console`), and the virtio-console telemetry transport
//! (`virtio_console`).
//!
//! Re-exported at the crate root (`pub(crate) use device::…`) so call sites stay
//! `crate::uart`, `crate::virtio_console`, etc.

pub mod console;
pub mod uart;
pub mod virtio_console;
