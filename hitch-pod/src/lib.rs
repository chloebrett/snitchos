//! `Pod` — the Plain-Old-Data memory primitive: a `#[repr(C)]`, padding-free,
//! every-bit-pattern-valid type whose `&[T]` reinterprets to/from `&[u8]` with no
//! serialization.
//!
//! This is a foundational `no_std`, **alloc-free** leaf so the lowest-level crates
//! (the kernel↔userspace `abi`) can mark their wire structs `Pod` without pulling
//! in the value model. `hitch` re-exports everything here (plus the allocating
//! `from_pod_bytes`), so `hitch` users see `hitch::Pod` / `hitch::pod_bytes`
//! unchanged. The layering is deliberate: `Pod` is a memory property *below* the
//! ABI, so `abi` depending on this is proper, not the inversion `abi -> hitch`
//! would be.

#![no_std]

/// A **P**lain **O**ld **D**ata type: its bytes *are* its value, so a `&[T]`
/// reinterprets to/from `&[u8]` with no serialization.
///
/// # Safety
/// Implementors must be `#[repr(C)]`, contain **no padding**, and have **every bit
/// pattern valid**. The first two guarantee no uninitialized byte is exposed (a
/// kernel info-leak if violated); the third guarantees any `&[u8]` of the right
/// length is a valid `&[Self]`. Implement via `#[derive(Pod)]`, which checks all
/// three — a hand-written `impl` carries the proof itself. `bool`/`char` are
/// deliberately **not** `Pod` (invalid bit patterns).
pub unsafe trait Pod: Copy + 'static {}

macro_rules! impl_pod {
    ($($t:ty),*) => { $( // SAFETY: every bit pattern of a fixed-width integer or
        // IEEE-754 float is a valid value; all are `repr`-stable with no padding.
        unsafe impl Pod for $t {} )* };
}
impl_pod!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64);

// SAFETY: `[T; N]` is `T` repeated contiguously with no padding between elements
// and none at the end, so if every `T` byte is initialized and valid (`T: Pod`),
// every array byte is too. Lets fixed-size byte-array fields (e.g. an inline name)
// live in a `#[derive(Pod)]` struct.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

/// The packed bytes of a POD slice, **zero-copy**: exactly its `repr(C)` image.
/// Because `T: Pod` has no padding, every byte is initialized — nothing
/// uninitialized crosses the boundary.
#[must_use]
pub fn pod_bytes<T: Pod>(slice: &[T]) -> &[u8] {
    // SAFETY: `T: Pod` is `repr(C)` with no padding, so all `size_of_val(slice)`
    // bytes are initialized and any pattern is a valid `u8`. The result borrows
    // `slice`, so it cannot outlive it.
    unsafe {
        core::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), core::mem::size_of_val(slice))
    }
}

/// `#[derive(Pod)]` — generates the `unsafe impl` only after compile-checking
/// `#[repr(C)]`, all-fields-`Pod`, and no padding. Behind the `derive` feature.
#[cfg(feature = "derive")]
pub use hitch_derive::Pod;
