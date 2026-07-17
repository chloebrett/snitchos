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
    #[arg(long = "jit")]
    block_jit: bool,
    /// Measure each memop's real interpreted cost against `memop_charge` (forces
    /// native ops off).
    #[arg(long)]
    calibrate_memops: bool,
    /// Make `etc/ramfb` exist in the guest's fw_cfg directory — the snemu
    /// equivalent of QEMU's `-device ramfb`. Off by default.
    #[arg(long)]
    ramfb: bool,
    /// After running, write the captured `etc/ramfb` framebuffer to this
    /// path as a binary PPM (P6) image (open with any image viewer). No-op
    /// with a clear stderr message if no framebuffer was ever captured
    /// (`--ramfb` wasn't passed, or the guest hasn't presented yet).
    #[arg(long)]
    dump_framebuffer: Option<PathBuf>,
    /// Open a live window showing the captured `etc/ramfb` framebuffer,
    /// updated periodically as the guest runs. Black until the guest's
    /// first present. Close the window or press Esc to stop the run.
    #[arg(long)]
    window: bool,
}

/// Fixed mode this milestone's kernel hardcodes
/// (`kernel/src/device/ramfb.rs::{WIDTH,HEIGHT}`) — used to size the window
/// before a config has been captured (nothing to derive dimensions from
/// yet).
const DEFAULT_WINDOW_WIDTH: usize = 1024;
const DEFAULT_WINDOW_HEIGHT: usize = 768;

/// How many guest steps between window redraws. `update_with_buffer` pumps
/// OS events *and* redraws in one call — doing that every single `step()`
/// would both tank throughput and redraw far more often than a human eye
/// benefits from. A fixed constant for a first cut; tune once it's running
/// (or promote to a `--window-interval` flag) if it's ever wrong.
const WINDOW_UPDATE_INTERVAL: u64 = 200_000;

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

    // Scramble is driven by the `SNEMU_SCRAMBLE_FRAMES` env var for the standalone
    // binary (`load_memory` honours it), so pass `false` here.
    let mut machine = match snemu::loader::load_machine(&image, RAM_SIZE, Some(&dtb), HART_COUNT, false) {
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
    if cli.ramfb {
        machine.enable_fwcfg_ramfb();
    }

    let mut window = if cli.window {
        match minifb::Window::new(
            "snemu framebuffer",
            DEFAULT_WINDOW_WIDTH,
            DEFAULT_WINDOW_HEIGHT,
            minifb::WindowOptions::default(),
        ) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("snemu: --window: failed to open a window: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let max_steps = cli.max_steps;
    let mut steps = 0u64;
    'run: while steps < max_steps {
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
        if let Some(win) = &mut window
            && steps.is_multiple_of(WINDOW_UPDATE_INTERVAL)
        {
            present_window(win, &machine);
            if !win.is_open() || win.is_key_down(minifb::Key::Escape) {
                break 'run;
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
    if let Some(path) = &cli.dump_framebuffer {
        dump_framebuffer(&machine, path);
    }
    ExitCode::SUCCESS
}

/// Redraw `window` with the machine's captured framebuffer, or an all-black
/// buffer of the default size if nothing has been captured yet (before
/// `--ramfb`'s first present) or the captured dimensions are unexpected —
/// never panics on a size mismatch.
fn present_window(window: &mut minifb::Window, machine: &snemu::machine::Machine) {
    let (buffer, width, height) = match machine.framebuffer_pixels() {
        Some((buf, w, h)) if buf.len() == (w * h) as usize => (buf, w as usize, h as usize),
        _ => (vec![0u32; DEFAULT_WINDOW_WIDTH * DEFAULT_WINDOW_HEIGHT], DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT),
    };
    let _ = window.update_with_buffer(&buffer, width, height);
}

/// Write the machine's captured `etc/ramfb` framebuffer to `path` as a PPM
/// image, or report on stderr why there's nothing to write — never a
/// silent empty file.
fn dump_framebuffer(machine: &snemu::machine::Machine, path: &std::path::Path) {
    let Some(ppm) = machine.dump_framebuffer() else {
        eprintln!(
            "snemu: --dump-framebuffer requested but no framebuffer was captured \
             (pass --ramfb, and make sure the guest reached its first present)"
        );
        return;
    };
    match std::fs::write(path, &ppm) {
        Ok(()) => eprintln!("snemu: wrote {} ({} bytes)", path.display(), ppm.len()),
        Err(e) => eprintln!("snemu: failed to write {}: {e}", path.display()),
    }
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
