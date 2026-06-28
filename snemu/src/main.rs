//! `snemu <kernel.elf> [max-steps]` — load an ELF64 RISC-V image, run it, and
//! print whatever it writes to the UART. On the first instruction snemu doesn't
//! implement, it halts and reports the program counter + raw word (the
//! meta-loop signal): run, see what it hits, implement that, repeat.

use std::process::ExitCode;

use snemu::cpu::StepError;

/// QEMU `virt` default RAM (128 MiB).
const RAM_SIZE: usize = 128 * 1024 * 1024;
const DEFAULT_MAX_STEPS: u64 = 50_000_000;

/// Device tree the guest sees, dumped from QEMU's `virt` machine:
/// `qemu-system-riscv64 -machine virt,dumpdtb=snemu/virt.dtb -smp 1 -m 128M`.
/// (`-smp 1` so the kernel boots uniprocessor — snemu is single-hart for now.)
const DTB: &[u8] = include_bytes!("../virt.dtb");

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: snemu <kernel.elf> [max-steps]");
        return ExitCode::FAILURE;
    };
    let max_steps = args
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

    let mut cpu = match snemu::loader::load(&image, RAM_SIZE, Some(DTB)) {
        Ok(cpu) => cpu,
        Err(e) => {
            eprintln!("snemu: load failed: {e:?}");
            return ExitCode::FAILURE;
        }
    };

    let mut steps = 0u64;
    while steps < max_steps {
        match cpu.step() {
            Ok(()) => steps += 1,
            Err(StepError::Unimplemented { pc, instr }) => {
                eprintln!("snemu: unimplemented instruction {instr:#010x} at pc {pc:#018x} (after {steps} steps, satp {:#x})", cpu.satp());
                break;
            }
            Err(other) => {
                eprintln!("snemu: halted with {other:?} at pc {:#018x} (after {steps} steps, satp {:#x})", cpu.pc(), cpu.satp());
                break;
            }
        }
    }
    if steps == max_steps {
        eprintln!("snemu: step limit reached ({max_steps})");
    }

    print!("{}", String::from_utf8_lossy(cpu.uart_output()));
    ExitCode::SUCCESS
}
