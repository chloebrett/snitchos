//! Backend B — the native block JIT (design: `plans/snemu-milestone-6-block-jit.md`).
//!
//! Backend A walks the reified `Op` IR interpretively; Backend B lowers the same IR
//! to **native AArch64** in an executable buffer and runs it, falling back to A for
//! anything it can't emit. This module is host-only (`cfg(not(wasm))`) and the one
//! place snemu uses `unsafe` — it generates and executes machine code.
//!
//! Increment 0 (here): prove we can generate and run AArch64 on Apple Silicon at all.
//! macOS enforces W^X in hardware, so the code buffer is `MAP_JIT` memory whose
//! write-vs-execute state is toggled per-thread with `pthread_jit_write_protect_np`,
//! and the instruction cache is flushed before execution. Everything else builds on
//! this foundation.

#![cfg(all(target_arch = "aarch64", target_os = "macos"))]
// Increment 0 is the foundation only — the emitter + exec buffer exist and are
// proven by tests, but nothing wires them into block execution yet. Later increments
// (lower `Op`s, run compiled blocks) consume every item here; the allow goes then.
#![allow(dead_code, reason = "increment-0 JIT scaffolding; wired in by later increments")]

// Apple-specific libSystem entry points, not surfaced by the `libc` crate. Linked by
// default on macOS (every binary links libSystem).
unsafe extern "C" {
    /// Per-thread toggle of the calling thread's `MAP_JIT` pages between writable
    /// (`enabled == 0`) and executable (`enabled == 1`). This is how Apple Silicon
    /// upholds W^X: the pages are never simultaneously writable and executable.
    fn pthread_jit_write_protect_np(enabled: libc::c_int);
    /// Flush the instruction cache over `[start, start+len)` — required after writing
    /// code, since the CPU's I-cache and D-cache are not coherent on ARM.
    fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
}

/// A page of executable memory generated code lives in. Allocated `MAP_JIT` so the
/// hardened runtime permits toggling it writable→executable per thread; [`install`]
/// writes code and flips it to executable, [`as_ptr`] hands back a callable pointer.
///
/// [`install`]: ExecBuffer::install
/// [`as_ptr`]: ExecBuffer::as_ptr
pub(crate) struct ExecBuffer {
    ptr: *mut u8,
    len: usize,
}

impl ExecBuffer {
    /// Reserve `len` bytes (rounded up to a page) of `MAP_JIT` read/write/exec memory.
    /// Panics if the mapping fails — a JIT with nowhere to write can't proceed.
    pub(crate) fn new(len: usize) -> Self {
        let page = 16 * 1024; // Apple Silicon page size
        let len = len.max(1).div_ceil(page) * page;
        // SAFETY: a standard anonymous mmap. `MAP_JIT` + RWX is the Apple-sanctioned
        // way to get a JIT region under the hardened runtime; null addr lets the
        // kernel choose. We check the result against `MAP_FAILED` below.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_ANON | libc::MAP_PRIVATE | libc::MAP_JIT,
                -1,
                0,
            )
        };
        assert!(ptr != libc::MAP_FAILED, "mmap MAP_JIT failed (JIT region)");
        Self { ptr: ptr.cast(), len }
    }

    /// Copy `code` into the buffer and make it executable. The sequence is the W^X
    /// dance: unlock writes for this thread, copy, re-lock (making it executable),
    /// then flush the I-cache so the CPU fetches the freshly written bytes.
    pub(crate) fn install(&mut self, code: &[u8]) {
        assert!(code.len() <= self.len, "generated code exceeds the buffer");
        // SAFETY: `write_protect(false)` makes this thread's MAP_JIT pages writable;
        // `code` fits (checked); `copy` writes within the mapping; `write_protect(true)`
        // restores execute permission before anyone calls in; `icache` flush covers
        // exactly the region we wrote. The pointer stays valid for `self.len`.
        unsafe {
            pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(code.as_ptr(), self.ptr, code.len());
            pthread_jit_write_protect_np(1);
            sys_icache_invalidate(self.ptr.cast(), code.len());
        }
    }

    /// The entry pointer, to `transmute` into a callable `extern "C"` fn. The caller
    /// asserts the buffer holds valid code matching the fn-pointer signature.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.ptr.cast_const()
    }
}

impl Drop for ExecBuffer {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`len` are the exact mapping from `new`, unmapped once.
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

/// A growable buffer of AArch64 machine code — a tiny hand-written assembler. Each
/// method appends one little-endian 32-bit instruction. This is Backend B's emitter;
/// increment 0 needs only the three ops the foundation test exercises.
pub(crate) struct Code {
    bytes: Vec<u8>,
}

impl Code {
    pub(crate) fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Append one 32-bit instruction (AArch64 is fixed-width, little-endian).
    fn emit(&mut self, insn: u32) {
        self.bytes.extend_from_slice(&insn.to_le_bytes());
    }

    /// `movz Xd, #imm16` — load a 16-bit immediate into `Xd`, zeroing the rest.
    pub(crate) fn movz(&mut self, xd: u32, imm16: u16) {
        self.emit(0xD280_0000 | (u32::from(imm16) << 5) | xd);
    }

    /// `add Xd, Xn, Xm` — 64-bit register add.
    pub(crate) fn add(&mut self, xd: u32, xn: u32, xm: u32) {
        self.emit(0x8B00_0000 | (xm << 16) | (xn << 5) | xd);
    }

    /// `ret` — return to the address in the link register (X30).
    pub(crate) fn ret(&mut self) {
        self.emit(0xD65F_03C0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_function_returning_a_constant_runs() {
        // `movz x0, #42 ; ret` — the smallest possible generated function.
        let mut buf = ExecBuffer::new(64);
        let mut code = Code::new();
        code.movz(0, 42);
        code.ret();
        buf.install(code.bytes());
        // SAFETY: the buffer holds a valid `movz;ret` with the C ABI (no args, u64
        // return in x0), matching this fn-pointer type.
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(), 42);
    }

    #[test]
    fn a_generated_function_can_add_its_two_arguments() {
        // `add x0, x0, x1 ; ret` — proves argument passing (x0, x1) + a real ALU op.
        let mut buf = ExecBuffer::new(64);
        let mut code = Code::new();
        code.add(0, 0, 1);
        code.ret();
        buf.install(code.bytes());
        // SAFETY: the buffer holds `add x0,x0,x1; ret`, matching this two-arg C ABI.
        let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
        assert_eq!(f(3, 4), 7);
        assert_eq!(f(1000, 337), 1337);
    }
}
