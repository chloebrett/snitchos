//! The tour's docs-drift contract: every chapter still describes the machine.
//!
//! A chapter is prose plus falsifiable claims about a real boot. This runs the
//! boot; [`tour::verify::check`] renders the verdict. The split is the point —
//! the judgement is pure and host-tested, so the gate and (from step 8) the
//! browser cannot come to different conclusions about what a chapter claims.
//!
//! Runs inside `cargo xtask itest`, not the nextest phase: it boots a kernel, and
//! `cargo xtask test` is host checks. itest has already built the kernel, so the
//! marginal cost here is one boot rather than a riscv build.

use std::process::ExitCode;

/// Instructions to allow before giving up on a chapter's anchor.
///
/// A ceiling, not an expectation: the boot stops the moment the anchor arrives,
/// and every chapter's anchor is an early-boot event. Generous enough that a
/// genuine regression reads as "the anchor never happened" rather than as a
/// budget that was merely too tight.
const MAX_STEPS: u64 = 60_000_000;

/// Check every chapter against a live boot.
///
/// `opt` is the build regime the surrounding itest run used. Passing anything else
/// makes this boot build a *second* kernel — measured at 6m30s, for a guest that
/// then behaves the same.
pub fn run(opt: crate::qemu::OptLevel) -> ExitCode {
    let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../tour/chapters"));
    let chapters = match tour::Chapter::load_dir(dir) {
        Ok(chapters) => chapters,
        Err(err) => {
            println!("\n=== tour ===\n  FAIL  {err}");
            return ExitCode::from(1);
        }
    };

    println!("\n=== tour ===");
    let mut failed = false;
    for chapter in &chapters {
        let workload = (!chapter.workload.is_empty()).then_some(chapter.workload.as_str());
        let anchored = |frames: &[protocol::stream::OwnedFrame]| chapter.anchor.find(frames).is_some();

        let (frames, steps) =
            match crate::snemu_diff::collect_frames_until(workload, MAX_STEPS, opt, anchored) {
                Ok(collected) => collected,
                Err(err) => {
                    println!("  FAIL  {}: boot failed: {err}", chapter.slug);
                    failed = true;
                    continue;
                }
            };

        match tour::verify::check(chapter, &frames) {
            Ok(()) => println!(
                "  PASS  {} — {} claim(s) hold at its anchor ({steps} steps, {} frames)",
                chapter.slug,
                chapter.claims.len(),
                frames.len()
            ),
            Err(failures) => {
                failed = true;
                for failure in &failures {
                    println!("  FAIL  {failure}");
                }
                println!(
                    "        Either the kernel changed and the chapter needs rewriting, \
                     or this is the regression it was written to catch."
                );
            }
        }
    }

    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
