//! Guest instret profiler (`cargo xtask snemu-profile`).
//!
//! The snemu-itest audit ranks *scenarios* by total guest-instret (the "ruler").
//! This ranks *functions*: boot a workload to the `entering heartbeat` checkpoint,
//! then run under snemu with exact per-PC instret counting on (every retired
//! instruction attributed to where it ran — deterministic, no sampling), and roll
//! the histogram up to kernel function names via the ELF symbol table. Turns "which
//! scenario is the pole" into "which *code* the pole spends its cycles in" — e.g. a
//! cross-hart spin-wait vs. real work. Motivation: the `smp-tlb-shootdown-visible`
//! pole was 100% negative-oracle scanning, invisible in the per-scenario total.

use std::collections::HashMap;
use std::process::ExitCode;

use snemu::symbols::{Symbol, SymbolTable};

use crate::snemu_diff;

/// Kernel higher-half link base: PCs at or above this are kernel VAs and resolve
/// directly against the ELF symbols.
const KERNEL_HIGH_HALF: u64 = 0xffff_ffff_8000_0000;
/// `KERNEL_OFFSET`: physical → higher-half, for early-boot kernel PCs that run
/// before the trampoline (a small slice; most kernel execution is higher-half).
const KERNEL_OFFSET: u64 = 0xffff_ffff_0000_0000;
/// Userspace programs link here (`user/*/user.ld`).
const USER_BASE: u64 = 0x1000_0000;
const USER_END: u64 = 0x4000_0000;
/// `OpenSBI` firmware sits at the RAM base; the kernel image starts at +2 MiB.
const FW_BASE: u64 = 0x8000_0000;
const KERNEL_PHYS: u64 = 0x8020_0000;

/// The bucket a single PC rolls up to: a kernel function name, or a coarse
/// non-kernel category (userspace / firmware / other).
///
/// `user_detail` splits userspace out per-PC (`[user:0x100053ac]`) instead of
/// collapsing it to one `[userspace]` bucket. Userspace ELFs are separate +
/// release-stripped so there are no symbols to resolve against — but a raw hot
/// PC is exactly what locates a spin-loop (objdump the owning program at that
/// address). Off by default (the profiler's main job is kernel profiling, where
/// per-PC userspace would be noise).
fn classify(pc: u64, symtab: &SymbolTable, user_detail: bool) -> String {
    if pc >= KERNEL_HIGH_HALF {
        return symtab.resolve(pc).map_or_else(|| "[kernel:unknown]".to_owned(), str::to_owned);
    }
    if (USER_BASE..USER_END).contains(&pc) {
        return if user_detail { format!("[user:{pc:#010x}]") } else { "[userspace]".to_owned() };
    }
    if (FW_BASE..KERNEL_PHYS).contains(&pc) {
        return "[firmware/OpenSBI]".to_owned();
    }
    // Early-boot kernel code executes at physical VAs (pre-trampoline); recover the
    // symbol by lifting to higher-half.
    if pc >= KERNEL_PHYS
        && let Some(name) = symtab.resolve(pc + KERNEL_OFFSET)
    {
        return format!("{name} [boot-phys]");
    }
    "[other]".to_owned()
}

/// Fold a `PC → instret` histogram into `bucket → instret`, sorted by instret
/// descending (ties broken by name for determinism).
fn aggregate(profile: &HashMap<u64, u64>, symtab: &SymbolTable, user_detail: bool) -> Vec<(String, u64)> {
    let mut by_bucket: HashMap<String, u64> = HashMap::new();
    for (&pc, &count) in profile {
        *by_bucket.entry(classify(pc, symtab, user_detail)).or_insert(0) += count;
    }
    let mut rows: Vec<(String, u64)> = by_bucket.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Parse `Text` (function) symbols out of the kernel ELF.
fn load_symbols(elf: &[u8]) -> Result<SymbolTable, String> {
    use object::{Object, ObjectSymbol, SymbolKind};
    let file = object::File::parse(elf).map_err(|e| format!("parse kernel ELF: {e}"))?;
    let syms = file
        .symbols()
        .filter(|s| s.kind() == SymbolKind::Text)
        .filter_map(|s| {
            let name = s.name().ok()?;
            (!name.is_empty()).then(|| Symbol {
                addr: s.address(),
                size: s.size(),
                // Demangle + strip the hash suffix so the report reads as source
                // paths (`kernel::sched::prepare_switch`), not `_ZN6kernel...`.
                name: format!("{:#}", rustc_demangle::demangle(name)),
            })
        })
        .collect();
    Ok(SymbolTable::new(syms))
}

const CHECKPOINT: &[u8] = b"entering heartbeat";
const CHECKPOINT_BUDGET: u64 = 60_000_000;

/// Boot `workload` to the heartbeat checkpoint (unprofiled), then run `steps`
/// instructions with profiling on and print the top `top` functions by instret.
pub fn run(workload: Option<&str>, steps: u64, top: usize, opt: crate::qemu::OptLevel, user_detail: bool) -> ExitCode {
    // `opt` picks the build regime, including the userspace opt-level: `--opt hi`
    // profiles the opt-2 userspace (where the UB class first bites), `--opt max`
    // the opt-3 one. Pair with `--user-detail` to see *which* userspace PC a hang
    // is spinning at.
    let (kernel, dtb) = match snemu_diff::prepare_profiled(true, opt) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-profile: {e}");
            return ExitCode::from(1);
        }
    };
    let symtab = match load_symbols(&kernel) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("snemu-profile: {e}");
            return ExitCode::from(1);
        }
    };

    let mut machine = match snemu_diff::load_workload_machine(&kernel, &dtb, workload, false) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("snemu-profile: load machine: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = machine.run_until_uart(CHECKPOINT, CHECKPOINT_BUDGET) {
        eprintln!("snemu-profile: boot to checkpoint: {e}");
        return ExitCode::from(1);
    }

    machine.enable_profiling();
    let mut ran = 0u64;
    while ran < steps {
        if machine.step().is_err() {
            break; // a guest fault (e.g. a deliberate-panic workload) ends the run
        }
        ran += 1;
    }
    let profile = machine.take_profile().unwrap_or_default();
    let rows = aggregate(&profile, &symtab, user_detail);
    // The denominator is instructions *retired* (the histogram sum), not scheduler
    // rounds: a round retires one instruction per running hart, so with 2 harts the
    // retired count is up to 2× `ran`. Summing the histogram is the honest total.
    let total: u64 = profile.values().sum();
    print_report(workload, total, &rows, top);
    ExitCode::SUCCESS
}

fn print_report(workload: Option<&str>, total: u64, rows: &[(String, u64)], top: usize) {
    let label = workload.unwrap_or("(default/init)");
    #[allow(clippy::cast_precision_loss)]
    let pct = |n: u64| if total == 0 { 0.0 } else { n as f64 * 100.0 / total as f64 };
    println!(
        "\n=== instret profile: workload={label} (post-boot, {} over {} PCs) ===",
        magnitude::format(total),
        rows.len(),
    );
    println!("  {:>7}  {:>8}  function", "share", "instret");
    for (name, count) in rows.iter().take(top) {
        println!("  {:>6.1}%  {:>8}  {name}", pct(*count), magnitude::format(*count));
    }
    let shown: u64 = rows.iter().take(top).map(|(_, c)| c).sum();
    let rest = total.saturating_sub(shown);
    if rest > 0 {
        println!(
            "  {:>6.1}%  {:>8}  [{} more]",
            pct(rest),
            magnitude::format(rest),
            rows.len().saturating_sub(top)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symtab() -> SymbolTable {
        SymbolTable::new(vec![
            Symbol { addr: 0xffff_ffff_8020_0000, size: 0x100, name: "kmain".to_owned() },
            Symbol { addr: 0xffff_ffff_8020_1000, size: 0x100, name: "yield_now".to_owned() },
        ])
    }

    #[test]
    fn classify_buckets_kernel_userspace_and_firmware() {
        let t = symtab();
        assert_eq!(classify(0xffff_ffff_8020_1040, &t, false), "yield_now");
        assert_eq!(classify(0x1000_1234, &t, false), "[userspace]");
        assert_eq!(classify(0x8000_4000, &t, false), "[firmware/OpenSBI]");
    }

    #[test]
    fn classify_user_detail_splits_userspace_per_pc() {
        let t = symtab();
        // Off: one bucket. On: the raw PC, so a spin-loop address surfaces.
        assert_eq!(classify(0x1000_53ac, &t, false), "[userspace]");
        assert_eq!(classify(0x1000_53ac, &t, true), "[user:0x100053ac]");
        // Kernel PCs are unaffected by the flag.
        assert_eq!(classify(0xffff_ffff_8020_1040, &t, true), "yield_now");
    }

    #[test]
    fn classify_lifts_early_boot_physical_pcs_to_the_symbol() {
        let t = symtab();
        // A physical PC inside kmain's higher-half range, pre-trampoline.
        assert_eq!(classify(0x8020_0040, &t, false), "kmain [boot-phys]");
    }

    #[test]
    fn aggregate_sums_pcs_per_function_and_sorts_descending() {
        let t = symtab();
        let profile = HashMap::from([
            (0xffff_ffff_8020_0000, 10), // kmain
            (0xffff_ffff_8020_0004, 5),  // kmain
            (0xffff_ffff_8020_1000, 30), // yield_now
            (0x1000_0000, 7),            // userspace
        ]);
        let rows = aggregate(&profile, &t, false);
        assert_eq!(rows[0], ("yield_now".to_owned(), 30));
        assert_eq!(rows[1], ("kmain".to_owned(), 15));
        assert_eq!(rows[2], ("[userspace]".to_owned(), 7));
    }
}
