//! Measure each `Gemm` backend at the shapes the ladder actually runs.
//!
//! `cargo run --release -p cram --bin bench-gemm`
//!
//! Sweeping **drivel-shaped and ballad-shaped** matmuls rather than one size is
//! the point: small-`k` projections sit well below any backend's peak, and a
//! single-shape result would generalize in exactly the flattering direction.

use std::time::Instant;

use cram::BlockedGemm;
use kvetch_model::{Gemm, GemmSpec, NaiveGemm, Rung, pseudo_random_weights};

/// Rows per multiply: a plausible `batch × sequence` for a training step.
const ROWS: usize = 2048;

fn main() {
    println!(
        "{:<26} {:>10} {:>12} {:>12}",
        "shape (GFLOP/s)", "naive", "blocked", "accelerate"
    );

    for rung in [Rung::Drivel, Rung::Ballad] {
        let config = rung.config();
        let projections = [
            ("attn", config.d_model, config.d_model),
            ("ffn-up", config.d_model, config.ffn),
            ("ffn-down", config.ffn, config.d_model),
        ];

        for (label, k, n) in projections {
            let spec = GemmSpec {
                m: ROWS,
                k,
                n,
                transpose_a: false,
                transpose_b: false,
            };
            let name = format!("{} {label} {k}x{n}", rung.name());

            print!("{name:<26}");
            print!("{:>10}", rate(&NaiveGemm, spec, 1));
            print!("{:>12}", rate(&BlockedGemm, spec, 5));
            #[cfg(target_vendor = "apple")]
            print!("{:>12}", rate(&cram::AccelerateGemm, spec, 20));
            println!();
        }
    }
}

/// GFLOP/s for `spec` on `backend`, best of `repeats` after one warm-up.
///
/// Best-of rather than mean: we want the backend's capability, not the
/// machine's background noise, and a slow run is always contamination.
fn rate<G: Gemm>(backend: &G, spec: GemmSpec, repeats: usize) -> String {
    let a = pseudo_random_weights(spec.m * spec.k, 1);
    let b = pseudo_random_weights(spec.k * spec.n, 2);
    let mut c = vec![0.0; spec.m * spec.n];

    backend.sgemm(spec, &a, &b, &mut c);

    let best = (0..repeats)
        .map(|_| {
            let started = Instant::now();
            backend.sgemm(spec, &a, &b, &mut c);
            started.elapsed().as_secs_f64()
        })
        .fold(f64::INFINITY, f64::min);

    let flops = 2.0 * spec.m as f64 * spec.k as f64 * spec.n as f64;
    format!("{:.0}", flops / best / 1e9)
}
