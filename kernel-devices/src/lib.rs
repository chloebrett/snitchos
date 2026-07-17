//! Device protocol logic, host-tested: the virtqueue/TX-staging sequencing
//! (`virtio`), the fw_cfg selector + DMA handshake (`fwcfg`), the ramfb config
//! blob (`ramfb`), framebuffer geometry (`framebuffer`), and the console input
//! ring (`console`).
//!
//! Carved out of `kernel-core` — see `plans/kernel-core-split.md`. **No MMIO.**
//! Every register poke lives in `kernel/src/device/`; what's here is the part
//! that decides *what* to write and *in what order* — the part with branches
//! worth asserting on. The kernel side reaches it through trait seams
//! (`FwCfgTransport`, the `stage_and_emit` callback).
//!
//! Production code here is pure `core` over caller-supplied buffers: the device
//! logic allocates nothing, which is why `alloc` below is `cfg(test)`-only.

#![no_std]
#![forbid(unsafe_code)]

// Only the virtio test reaches for `Vec`; the device logic itself never allocates.
#[cfg(test)]
extern crate alloc;

pub mod console;
pub mod framebuffer;
pub mod fwcfg;
pub mod ramfb;
pub mod virtio;
