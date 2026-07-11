//! `snemu` — load an ELF64 RISC-V image, run it, print whatever it writes to the
//! UART, and decode the telemetry frames it transmits over the virtio-console. On
//! the first instruction snemu doesn't implement, it halts and reports the program
//! counter + raw word (the meta-loop signal): run, see what it hits, implement
//! that, repeat.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use protocol::stream::{OwnedFrame, decode_stream};

/// QEMU `virt` default RAM (128 MiB).
const RAM_SIZE: usize = 128 * 1024 * 1024;
const DEFAULT_MAX_STEPS: u64 = 50_000_000;
/// Harts the machine boots with. The kernel is `MAX_HARTS = 2` and brings up its
/// secondary unconditionally, so the DTB and machine must offer two.
const HART_COUNT: usize = 2;

/// Device tree the guest sees, dumped from QEMU's `virt` machine:
/// `qemu-system-riscv64 -machine virt,dumpdtb=snemu/virt.dtb -smp 2 -m 128M`.
const DTB: &[u8] = include_bytes!("../virt.dtb");

/// Run a `SnitchOS` kernel ELF under the snemu RV64GC emulator.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The kernel ELF64 image to run.
    kernel: PathBuf,
    /// Instruction (scheduler-step) budget before giving up.
    #[arg(default_value_t = DEFAULT_MAX_STEPS)]
    max_steps: u64,
    /// Dump every decoded telemetry frame (otherwise just a count).
    #[arg(long)]
    frames: bool,
    /// Firmware role: select a runtime workload by injecting `workload=<name>` into
    /// `/chosen/bootargs` (needs an itest-workloads kernel; a plain build ignores it).
    #[arg(long)]
    workload: Option<String>,
    /// Native-op helper (tier-0.5 JIT): fast-path guest memset/memcpy.
    #[arg(long)]
    native_ops: bool,
    /// Tier-2 block JIT (M6): compile + run hot basic blocks.
    #[arg(long)]
    block_jit: bool,
    /// Measure each memop's real interpreted cost against `memop_charge` (forces
    /// native ops off).
    #[arg(long)]
    calibrate_memops: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let image = match std::fs::read(&cli.kernel) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("snemu: cannot read {}: {e}", cli.kernel.display());
            return ExitCode::FAILURE;
        }
    };

    let dtb = match &cli.workload {
        Some(name) => snemu::dtb::set_bootargs(DTB, &format!("workload={name}"))
            .unwrap_or_else(|| DTB.to_vec()),
        None => DTB.to_vec(),
    };

    let mut machine = match snemu::loader::load_machine(&image, RAM_SIZE, Some(&dtb), HART_COUNT) {
        Ok(machine) => machine,
        Err(e) => {
            eprintln!("snemu: load failed: {e:?}");
            return ExitCode::FAILURE;
        }
    };
    // The probe measures the interpreter's real per-memop cost, so it needs memops
    // interpreted, not collapsed — force native ops off when calibrating.
    machine.set_native_ops(cli.native_ops && !cli.calibrate_memops);
    machine.set_block_jit(cli.block_jit);
    if cli.calibrate_memops {
        machine.enable_memop_probe();
    }

    let max_steps = cli.max_steps;
    let mut steps = 0u64;
    while steps < max_steps {
        match machine.step() {
            Ok(()) => steps += 1,
            // The error carries the faulting pc / instruction; report the harts'
            // satp for boot-phase context.
            Err(err) => {
                eprintln!(
                    "snemu: halted with {err:?} (after {steps} steps, hart0 satp {:#x}, hart1 satp {:#x})",
                    machine.satp(0),
                    machine.satp(1)
                );
                break;
            }
        }
    }
    if steps == max_steps {
        eprintln!("snemu: step limit reached ({max_steps})");
    }
    let fires = machine.timer_fires();
    let instret = machine.instret();
    eprintln!(
        "snemu: {instret} instret, {fires} timer fires ({} instret/fire)",
        instret.checked_div(fires).unwrap_or(0),
    );

    if let Some((invocations, real, charged)) = machine.memop_probe_report() {
        let ratio = if charged == 0 { 0.0 } else { real as f64 / charged as f64 };
        eprintln!(
            "snemu: memop calibration — {invocations} memops, {real} real instret, \
             {charged} charged (real/charged = {ratio:.3}; 1.000 = faithful clock)"
        );
    }

    print!("{}", String::from_utf8_lossy(machine.uart_output()));
    report_frames(machine.virtio_tx_output(), cli.frames);
    ExitCode::SUCCESS
}

/// Decode the bytes the kernel transmitted over the virtio-console back into
/// telemetry frames and report them. A trailing partial frame (the kernel was
/// mid-send when the run stopped) decodes cleanly as EOF; only genuinely
/// malformed bytes error, and we keep whatever decoded before that.
///
/// Stream discipline: the decoded frames (`--frames`) are the *requested data*,
/// so they go to **stdout** — `snemu --frames … | grep …` works, and silencing
/// build/diagnostic noise with `2>/dev/null` never eats them. The byte/frame
/// count and any decode-error note are diagnostics and stay on **stderr**.
/// (This was a real trap: with the dump on stderr, `--frames 2>/dev/null | grep`
/// silently returned nothing and I mistook it for "zero frames" — see
/// notes/snemu-guard-page-fail-is-timing-not-mmu.md.)
fn report_frames(bytes: &[u8], dump: bool) {
    let mut frames = Vec::new();
    let mut cursor = Cursor::new(bytes);
    let result = decode_stream(&mut cursor, |f| frames.push(OwnedFrame::from_borrowed(f)));

    eprintln!(
        "snemu: virtio-console transmitted {} bytes, decoded {} telemetry frames",
        bytes.len(),
        frames.len()
    );
    if let Err(e) = result {
        eprintln!("snemu: (telemetry stream ended with: {e})");
    }
    if dump {
        for frame in &frames {
            println!("  {frame:?}");
        }
    }
}
