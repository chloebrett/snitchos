//! JH7110 PWMDAC driver — brings up the audio clocks/reset and drives signed-PCM
//! samples out the `VisionFive` 2's 3.5mm jack. The register layout, sample-rate
//! planning, and pacing are the host-tested `kernel_devices::pwmdac` /
//! `kernel_devices::syscrg`; this file is the thin `unsafe` MMIO glue that applies
//! them. Synthesis lives in userspace now — the `glitch` server generates samples
//! and hands them down via the cap-gated `AudioWrite` syscall ([`play_samples`]);
//! the in-kernel beep task was retired once `glitch-beep` shipped. See
//! `plans/glitch.md` and `plans/vf2-audio-tier0.md`.
//!
//! Address-driven, not `cfg(vf2)`-gated: the same code runs on the board and under
//! snemu's synthetic PWMDAC device, so the path is observable off-hardware via the
//! `glitch-beep-plays` scenario and snemu's `--audio-out` WAV. The `PWMDAC` block
//! sits in the UART's already-mapped 2 MiB megapage; SYSCRG needs `kmain`'s
//! `insert(0x1300_0000)`.

use kernel_devices::pwmdac::{
    plan_rate, sample_interval_ticks, Ctrl, DataMode, DataShift, DutyCycle, Resolution, CTRL_OFFSET,
    WDATA_OFFSET,
};
use kernel_devices::iomux::{self, FieldWrite};
use kernel_devices::syscrg::{self, Op};

use crate::obs::counter::DeferredCounter;

/// Samples written to `WDATA`, drained to the wire by the heartbeat. The itest
/// asserts this climbs; it also ships to Grafana on hardware.
pub static SAMPLES_EMITTED: DeferredCounter =
    DeferredCounter::new("snitchos.audio.samples_emitted_total");

/// PWMDAC block base (in the UART's mapped megapage). MMIO is reached through the
/// higher-half mapping (`pa + KERNEL_OFFSET`) — the identity map is torn down by
/// `unmap_identity`, same as the UART driver (`device/console.rs`).
const PWMDAC_BASE: usize = 0x100b_0000;
const PWMDAC_VA: usize = PWMDAC_BASE + crate::mmu::KERNEL_OFFSET;
/// SYSCRG registers, likewise via the higher-half mapping (`kmain` inserts the
/// `0x1300_0000` megapage).
const SYSCRG_VA: usize = syscrg::BASE + crate::mmu::KERNEL_OFFSET;
/// `SYS_IOMUX` (pin-mux) registers — same megapage as SYSCRG, so already mapped.
const IOMUX_VA: usize = iomux::BASE + crate::mmu::KERNEL_OFFSET;

/// `pwmdac_core` divider (`audio_root_rate / core_clk_hz`). The parent
/// `audio_root` rate isn't known statically — this needs calibration against the
/// board at hardware bring-up. Under snemu the SYSCRG writes are swallowed, so any
/// in-range value drives the beep for the itest; `1` is a safe placeholder.
const CORE_DIVIDER: u32 = 1;

/// The sample rate the DAC is configured for. **Must match the userspace
/// synthesis rate** (`glitch_core::FS_HZ`): the kernel paces `WDATA` at this rate,
/// so samples generated at any other rate play at the wrong pitch.
const DAC_RATE_HZ: u32 = 8000;

/// Apply a SYSCRG bring-up op via read-modify-write / poll on its MMIO register.
fn apply_syscrg(ops: &[Op]) {
    for op in ops {
        match *op {
            Op::Rmw { offset, mask, value } => {
                let addr = SYSCRG_VA + offset;
                // SAFETY: SYSCRG MMIO, mapped by `kmain`'s `insert(0x1300_0000)`.
                unsafe {
                    let cur = (addr as *const u32).read_volatile();
                    (addr as *mut u32).write_volatile((cur & !mask) | (value & mask));
                }
            }
            Op::PollUntilSet { offset, mask } => {
                let addr = SYSCRG_VA + offset;
                // SAFETY: SYSCRG status register read; spins until the reset releases.
                unsafe { while (addr as *const u32).read_volatile() & mask != mask {} }
            }
        }
    }
}

/// Apply one IOMUX pad field via read-modify-write.
fn apply_field(fw: FieldWrite) {
    let addr = IOMUX_VA + fw.offset;
    // SAFETY: SYS_IOMUX MMIO, in SYSCRG's already-mapped megapage.
    unsafe {
        let cur = (addr as *const u32).read_volatile();
        (addr as *mut u32).write_volatile((cur & !fw.mask) | fw.value);
    }
}

/// Route the PWMDAC's LEFT/RIGHT output signals onto their pads (GPIO 33/34) —
/// without this the DAC runs but its output reaches no pin (silence).
fn route_pins() {
    for fw in iomux::route_output(iomux::PWMDAC_LEFT_GPIO, iomux::PWMDAC_LEFT_SIGNAL) {
        apply_field(fw);
    }
    for fw in iomux::route_output(iomux::PWMDAC_RIGHT_GPIO, iomux::PWMDAC_RIGHT_SIGNAL) {
        apply_field(fw);
    }
}

/// Bring up the PWMDAC clocks + reset, then route its output pins. No-op on the
/// clock step if the divider is somehow out of range (it can't be — `CORE_DIVIDER`
/// is a valid constant).
fn bringup() {
    if let Some(ops) = syscrg::pwmdac_bringup(CORE_DIVIDER) {
        apply_syscrg(&ops);
    }
    route_pins();
}

/// Program `CTRL` for `sample_rate_hz`, enabling the DAC. Returns the per-sample
/// pacing interval in timer ticks, or `None` for an unsupported rate.
fn configure(sample_rate_hz: u32) -> Option<u64> {
    let plan = plan_rate(sample_rate_hz)?;
    let ctrl = Ctrl {
        enable: true,
        resolution: Resolution::Bits8,
        duty_cycle: DutyCycle::Center,
        cnt_n: plan.cnt_n,
        data_change: false,
        data_mode: DataMode::InverterMsb,
        data_shift: DataShift::new(0).expect("0 fits the 3-bit data-shift field"),
    };
    let addr = PWMDAC_VA + CTRL_OFFSET;
    // SAFETY: PWMDAC `CTRL` MMIO, in the UART's mapped megapage.
    unsafe { (addr as *mut u32).write_volatile(ctrl.to_bits()) };
    let timebase = crate::trap::TIMEBASE_HZ.load(core::sync::atomic::Ordering::Relaxed);
    sample_interval_ticks(sample_rate_hz, timebase)
}

/// Cached per-sample pacing interval, set by the first [`ensure_up`] (which also
/// runs the one-time bring-up). `Once` so the DAC is brought up exactly once no
/// matter how many `AudioWrite`s land.
static PACING_TICKS: crate::sync::Once<u64> = crate::sync::Once::new();

/// Bring the DAC up on first call and return the per-sample pacing interval. The
/// bring-up (clocks/reset/pins/`CTRL`) is idempotent but only run once.
fn ensure_up() -> u64 {
    *PACING_TICKS.call_once(|| {
        bringup();
        configure(DAC_RATE_HZ).unwrap_or(0)
    })
}

/// Play signed-PCM samples (little-endian bytes) to the DAC, paced at the
/// configured rate — the kernel half of the `AudioWrite` syscall, doing the MMIO
/// the `glitch` server can't. Brings the DAC up on first call. Trailing odd byte
/// (if any) is ignored; samples are 16-bit.
pub fn play_samples(bytes: &[u8]) {
    let interval = ensure_up();
    for chunk in bytes.chunks_exact(2) {
        write_sample(i16::from_le_bytes([chunk[0], chunk[1]]));
        SAMPLES_EMITTED.inc();
        let next = crate::trap::now_ticks() + interval;
        while crate::trap::now_ticks() < next {}
    }
}

/// Write one signed-PCM sample to `WDATA` (low 16 bits carry the sample).
fn write_sample(sample: i16) {
    let addr = PWMDAC_VA + WDATA_OFFSET;
    // SAFETY: PWMDAC `WDATA` MMIO, in the UART's mapped megapage.
    unsafe { (addr as *mut u32).write_volatile(u32::from(sample as u16)) };
}
