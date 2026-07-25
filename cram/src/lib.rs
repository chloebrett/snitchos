//! cram — the host-side trainer: what stuffs a corpus into a model.
//!
//! Hand-written throughout, no framework. `llm.c` is the precedent; the value
//! here is understanding, and the [`Gemm`] seam is what keeps that choice
//! reversible — over 95% of training FLOPs are matmul, so one trait carries the
//! entire performance story.
//!
//! This module is the fast backends. Correctness lives in `kvetch_model`'s
//! `NaiveGemm`, which every backend here is checked against.

use std::ffi::c_int;

use kvetch_model::{Gemm, GemmSpec};

pub mod optim;
pub mod run;
pub mod train;

/// Rows per block in [`BlockedGemm`]. Sized so a block's slice of the output
/// and the corresponding rows of `a` stay in L1 while `b` streams past.
const BLOCK: usize = 64;

/// A portable fast path: cache-blocked and multi-threaded, no intrinsics and no
/// platform dependency.
///
/// Exists so the ladder is not hostage to one vendor's matrix coprocessor — and
/// so there is a fast backend to fall back to when `AccelerateGemm` is not the
/// platform.
pub struct BlockedGemm;

impl Gemm for BlockedGemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]) {
        let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let rows_per_worker = spec.m.div_ceil(workers.max(1)).max(BLOCK);

        std::thread::scope(|scope| {
            for (index, band) in c.chunks_mut(rows_per_worker * spec.n).enumerate() {
                let first_row = index * rows_per_worker;
                scope.spawn(move || {
                    let rows = band.len() / spec.n;
                    blocked_band(spec, a, b, band, first_row, rows);
                });
            }
        });
    }
}

/// One worker's horizontal band of the output.
///
/// Accumulating into the output row-by-row over `k` — rather than computing each
/// dot product independently — is what makes `b`'s access sequential, which is
/// the whole win over the naive triple loop.
fn blocked_band(spec: GemmSpec, a: &[f32], b: &[f32], band: &mut [f32], first_row: usize, rows: usize) {
    band.fill(0.0);

    for local_row in 0..rows {
        let row = first_row + local_row;
        let target = &mut band[local_row * spec.n..][..spec.n];

        for inner in 0..spec.k {
            let left = if spec.transpose_a {
                a[inner * spec.m + row]
            } else {
                a[row * spec.k + inner]
            };
            if left == 0.0 {
                continue;
            }

            if spec.transpose_b {
                for (column, slot) in target.iter_mut().enumerate() {
                    *slot += left * b[column * spec.k + inner];
                }
            } else {
                let b_row = &b[inner * spec.n..][..spec.n];
                for (slot, value) in target.iter_mut().zip(b_row) {
                    *slot += left * value;
                }
            }
        }
    }
}

/// Apple's Accelerate framework, which is the only way to reach the **AMX**
/// matrix coprocessor — NEON cannot, and it is several times faster than any
/// hand-written SIMD kernel on this hardware.
///
/// A system framework, so this costs a `#[link]` attribute and no dependency.
#[cfg(target_vendor = "apple")]
pub struct AccelerateGemm;

#[cfg(target_vendor = "apple")]
#[link(name = "Accelerate", kind = "framework")]
unsafe extern "C" {
    #[allow(clippy::too_many_arguments, reason = "the CBLAS signature is fixed")]
    fn cblas_sgemm(
        order: c_int,
        transpose_a: c_int,
        transpose_b: c_int,
        m: c_int,
        n: c_int,
        k: c_int,
        alpha: f32,
        a: *const f32,
        lda: c_int,
        b: *const f32,
        ldb: c_int,
        beta: f32,
        c: *mut f32,
        ldc: c_int,
    );
}

#[cfg(target_vendor = "apple")]
mod cblas {
    use std::ffi::c_int;

    pub const ROW_MAJOR: c_int = 101;
    pub const NO_TRANS: c_int = 111;
    pub const TRANS: c_int = 112;
}

#[cfg(target_vendor = "apple")]
impl Gemm for AccelerateGemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]) {
        let flag = |transposed: bool| {
            if transposed {
                cblas::TRANS
            } else {
                cblas::NO_TRANS
            }
        };
        // Leading dimension is the stride of the matrix *as stored*, so a
        // transposed operand's stride is its own row length, not the logical one.
        let lda = if spec.transpose_a { spec.m } else { spec.k };
        let ldb = if spec.transpose_b { spec.k } else { spec.n };

        assert!(
            a.len() >= spec.m * spec.k && b.len() >= spec.k * spec.n && c.len() >= spec.m * spec.n,
            "operand too small for the spec; BLAS would read out of bounds"
        );

        // SAFETY: the assert above establishes that every buffer is at least as
        // large as the dimensions passed to BLAS, and the leading dimensions
        // match how each operand is actually laid out. cblas_sgemm reads a and
        // b and writes c, all within those bounds, and does not retain them.
        unsafe {
            cblas_sgemm(
                cblas::ROW_MAJOR,
                flag(spec.transpose_a),
                flag(spec.transpose_b),
                spec.m as c_int,
                spec.n as c_int,
                spec.k as c_int,
                1.0,
                a.as_ptr(),
                lda as c_int,
                b.as_ptr(),
                ldb as c_int,
                0.0,
                c.as_mut_ptr(),
                spec.n as c_int,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use kvetch_model::{NaiveGemm, pseudo_random_weights};

    use super::*;

    /// Shapes drawn from the ladder itself: drivel's and ballad's projections,
    /// plus the tall-skinny tied-embedding multiply. A backend that is only
    /// checked on square matrices is not checked on anything we run.
    fn ladder_shapes() -> Vec<GemmSpec> {
        let mut shapes = Vec::new();
        for (m, k, n) in [(256, 128, 128), (256, 128, 512), (256, 384, 384), (64, 128, 571)] {
            for (transpose_a, transpose_b) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                shapes.push(GemmSpec {
                    m,
                    k,
                    n,
                    transpose_a,
                    transpose_b,
                });
            }
        }
        shapes
    }

    fn run<G: Gemm>(backend: &G, spec: GemmSpec, a: &[f32], b: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; spec.m * spec.n];
        backend.sgemm(spec, a, b, &mut out);
        out
    }

    fn assert_agrees<G: Gemm>(backend: &G, label: &str) {
        for spec in ladder_shapes() {
            let a = pseudo_random_weights(spec.m * spec.k, 1);
            let b = pseudo_random_weights(spec.k * spec.n, 2);

            let reference = run(&NaiveGemm, spec, &a, &b);
            let actual = run(backend, spec, &a, &b);

            for (index, (expected, got)) in reference.iter().zip(&actual).enumerate() {
                assert!(
                    (expected - got).abs() < 1e-4,
                    "{label} disagrees at {index} for {spec:?}: {expected} vs {got}"
                );
            }
        }
    }

    #[test]
    fn blocked_agrees_with_the_reference() {
        assert_agrees(&BlockedGemm, "BlockedGemm");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn accelerate_agrees_with_the_reference() {
        assert_agrees(&AccelerateGemm, "AccelerateGemm");
    }
}
