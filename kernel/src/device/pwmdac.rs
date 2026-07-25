//! JH7110 PWMDAC driver — brings up the audio clocks/reset and drives a
//! fixed-frequency square-wave tone out the `VisionFive` 2's 3.5mm jack. The
//! register layout, sample-rate planning, tone generation, and pacing are all the
//! host-tested `kernel_devices::pwmdac` / `kernel_devices::syscrg`; this file is
//! the thin `unsafe` MMIO glue that applies them, plus the boot task that plays
//! the beep. See `plans/vf2-audio-tier0.md` (Increment 6).
//!
//! Address-driven, not `cfg(vf2)`-gated: the same code runs on the board and under
//! snemu's synthetic PWMDAC device (the `audio-beep` workload), so the beep is
//! observable off-hardware via the `audio-beep-emits-samples` scenario and snemu's
//! `--audio-out` WAV. The `PWMDAC` block sits in the UART's already-mapped 2 MiB
//! megapage; SYSCRG needs `kmain`'s `insert(0x1300_0000)`.

use kernel_devices::pwmdac::{
    plan_rate, sample_interval_ticks, Ctrl, DataMode, DataShift, DutyCycle, Gain, Resolution, Tone,
    CTRL_OFFSET, WDATA_OFFSET,
};
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

/// `pwmdac_core` divider (`audio_root_rate / core_clk_hz`). The parent
/// `audio_root` rate isn't known statically — this needs calibration against the
/// board at hardware bring-up. Under snemu the SYSCRG writes are swallowed, so any
/// in-range value drives the beep for the itest; `1` is a safe placeholder.
const CORE_DIVIDER: u32 = 1;

/// The beep: a low-volume 440 Hz square at 8 kHz. `PEAK` is deliberately well
/// below full scale — a square wave is harsh, and this drives headphones.
const BEEP_FREQ_HZ: u32 = 440;
const BEEP_RATE_HZ: u32 = 8000;
const BEEP_PEAK: i16 = 4000;

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

/// Bring up the PWMDAC clocks + reset. No-op if the divider is somehow out of
/// range (it can't be — `CORE_DIVIDER` is a valid constant).
fn bringup() {
    if let Some(ops) = syscrg::pwmdac_bringup(CORE_DIVIDER) {
        apply_syscrg(&ops);
    }
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

/// Write one signed-PCM sample to `WDATA` (low 16 bits carry the sample).
fn write_sample(sample: i16) {
    let addr = PWMDAC_VA + WDATA_OFFSET;
    // SAFETY: PWMDAC `WDATA` MMIO, in the UART's mapped megapage.
    unsafe { (addr as *mut u32).write_volatile(u32::from(sample as u16)) };
}

/// Boot task: bring the DAC up and loop emitting the beep, yielding each period so
/// the heartbeat runs (and drains `SAMPLES_EMITTED`). `-> !` per the scheduler ABI.
pub extern "C" fn audio_beep_entry() -> ! {
    bringup();
    let interval = configure(BEEP_RATE_HZ);
    let tone = Tone::square(BEEP_FREQ_HZ, BEEP_RATE_HZ, Gain::UNITY, BEEP_PEAK);
    loop {
        if let (Some(tone), Some(interval)) = (tone, interval) {
            for sample in tone.samples() {
                write_sample(sample);
                SAMPLES_EMITTED.inc();
                let next = crate::trap::now_ticks() + interval;
                while crate::trap::now_ticks() < next {}
            }
        }
        crate::sched::yield_now();
    }
}
