//! `snemu [--frames] <kernel.elf> [max-steps]` — load an ELF64 RISC-V image,
//! run it, print whatever it writes to the UART, and decode the telemetry
//! frames it transmits over the virtio-console. On the first instruction snemu
//! doesn't implement, it halts and reports the program counter + raw word (the
//! meta-loop signal): run, see what it hits, implement that, repeat.
//!
//! `--frames` dumps every decoded telemetry frame (otherwise just a count).

use std::io::Cursor;
use std::process::ExitCode;

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

fn main() -> ExitCode {
    let mut dump_frames = false;
    let mut workload: Option<String> = None;
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => dump_frames = true,
            // Firmware role: select a runtime workload by injecting
            // `workload=<name>` into /chosen/bootargs (needs an itest-workloads
            // kernel; a plain build ignores it and boots the default).
            "--workload" => workload = args.next(),
            _ => positional.push(arg),
        }
    }

    let mut positional = positional.into_iter();
    let Some(path) = positional.next() else {
        eprintln!("usage: snemu [--frames] [--workload <name>] <kernel.elf> [max-steps]");
        return ExitCode::FAILURE;
    };
    let max_steps = positional
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_STEPS);

    let image = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("snemu: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let dtb = match &workload {
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

    print!("{}", String::from_utf8_lossy(machine.uart_output()));
    report_frames(machine.virtio_tx_output(), dump_frames);
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
