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

use crate::block::{AluOp, Op};

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

    /// A three-register data-processing op `Xd = Xn OP Xm`, selected by the family
    /// `base` opcode (e.g. ADD/SUB/AND/ORR/EOR — see [`alu_base`]).
    fn alu(&mut self, base: u32, xd: u32, xn: u32, xm: u32) {
        self.emit(base | (xm << 16) | (xn << 5) | xd);
    }

    /// `ldr Xt, [Xn, #byte_off]` — 64-bit load, unsigned scaled offset (a guest
    /// register lives at `reg_index * 8` from the register-file base).
    fn ldr(&mut self, xt: u32, xn: u32, byte_off: u32) {
        self.emit(0xF940_0000 | ((byte_off / 8) << 10) | (xn << 5) | xt);
    }

    /// `str Xt, [Xn, #byte_off]` — the store counterpart of [`ldr`](Self::ldr).
    fn str(&mut self, xt: u32, xn: u32, byte_off: u32) {
        self.emit(0xF900_0000 | ((byte_off / 8) << 10) | (xn << 5) | xt);
    }

    /// `movk Xd, #imm16, LSL #(16*hw)` — replace one 16-bit lane, keep the rest.
    fn movk(&mut self, xd: u32, imm16: u16, hw: u32) {
        self.emit(0xF280_0000 | (hw << 21) | (u32::from(imm16) << 5) | xd);
    }

    /// Materialise a full 64-bit constant into `Xd` — one `movz` for the low lane
    /// then a `movk` per higher lane (a zero lane's `movk` is a harmless no-op).
    pub(crate) fn mov_imm64(&mut self, xd: u32, value: u64) {
        self.emit(0xD280_0000 | (u32::from(value as u16) << 5) | xd); // movz Xd, #lane0
        self.movk(xd, (value >> 16) as u16, 1);
        self.movk(xd, (value >> 32) as u16, 2);
        self.movk(xd, (value >> 48) as u16, 3);
    }
}

/// The AArch64 opcode family for a RISC-V `AluOp` Backend B can emit as a single
/// register-register instruction, or `None` if increment 1 doesn't lower it yet (the
/// caller then leaves the whole block to Backend A). Add/Sub/And/Or/Xor map directly;
/// shifts, set-less-than, and the `.w` (32-bit) forms need extra masking/sign-extend
/// and come later.
fn alu_base(alu: AluOp) -> Option<u32> {
    Some(match alu {
        AluOp::Add => 0x8B00_0000,
        AluOp::Sub => 0xCB00_0000,
        AluOp::And => 0x8A00_0000,
        AluOp::Or => 0xAA00_0000,
        AluOp::Xor => 0xCA00_0000,
        _ => return None,
    })
}

/// A block lowered to native AArch64 (Backend B). Compiled from the reified `Op` IR;
/// runs against the guest register file passed by pointer, returning the retired
/// instruction count — architecturally identical to Backend A walking the same ops.
pub(crate) struct NativeBlock {
    buf: ExecBuffer,
}

impl NativeBlock {
    /// Guest register-file base pointer (first C-ABI arg) and two caller-saved scratch
    /// registers. A leaf function: no callee-saved clobbers, so no prologue/epilogue.
    const REGS: u32 = 0;
    const A: u32 = 9;
    const B: u32 = 10;

    /// Lower an all-emittable, fall-through block (ALU + `SetImm` only) to native code,
    /// or `None` if any op isn't lowerable yet — a branch/jump/memory op, or an ALU
    /// family increment 1 doesn't cover. `None` means "run this block on Backend A".
    pub(crate) fn compile(ops: &[Op]) -> Option<Self> {
        let mut code = Code::new();
        for op in ops {
            match *op {
                Op::SetImm { rd, value } => {
                    code.mov_imm64(Self::A, value);
                    Self::write_rd(&mut code, rd);
                }
                Op::AluImm { alu, rd, rs1, imm } => {
                    let base = alu_base(alu)?;
                    code.ldr(Self::A, Self::REGS, u32::from(rs1) * 8);
                    code.mov_imm64(Self::B, imm as u64);
                    code.alu(base, Self::A, Self::A, Self::B);
                    Self::write_rd(&mut code, rd);
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    let base = alu_base(alu)?;
                    code.ldr(Self::A, Self::REGS, u32::from(rs1) * 8);
                    code.ldr(Self::B, Self::REGS, u32::from(rs2) * 8);
                    code.alu(base, Self::A, Self::A, Self::B);
                    Self::write_rd(&mut code, rd);
                }
                // Block exits + memory ops aren't emitted in increment 1 → Backend A.
                _ => return None,
            }
        }
        // Fall-through: every op retired. Return the count in x0 (the reg-file pointer
        // is dead now), then return to the caller.
        let retired = u16::try_from(ops.len()).ok()?; // blocks are length-capped
        code.movz(0, retired);
        code.ret();

        let mut buf = ExecBuffer::new(code.bytes().len());
        buf.install(code.bytes());
        Some(Self { buf })
    }

    /// Store scratch `A` (the op's result) into guest `x[rd]`, skipping `rd == 0`
    /// (x0 is hardwired zero — a legal discarded write).
    fn write_rd(code: &mut Code, rd: u8) {
        if rd != 0 {
            code.str(Self::A, Self::REGS, u32::from(rd) * 8);
        }
    }

    /// Execute the block against `regs`, returning the instructions it retired.
    pub(crate) fn run(&self, regs: &mut [u64; 32]) -> u64 {
        // SAFETY: `compile` emitted a function of exactly this C ABI — one pointer arg
        // (the register-file base) and a `u64` return — that only reads/writes the 32
        // words behind `regs` and clobbers caller-saved scratch. The buffer is
        // executable and outlives the call (borrowed through `&self`).
        let f: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(self.buf.as_ptr()) };
        f(regs.as_mut_ptr())
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

    use crate::block::{AluOp, Op};

    /// Reference: apply an op to a register file with the *same* `AluOp::apply`
    /// semantics Backend A uses, x0 hardwired zero. Backend A is already proven
    /// identical to the interpreter, so native == this == A == interpreter.
    fn ref_apply(regs: &mut [u64; 32], op: &Op) {
        let (rd, value) = match *op {
            Op::SetImm { rd, value } => (rd, value),
            Op::AluImm { alu, rd, rs1, imm } => (rd, alu.apply(regs[rs1 as usize], imm as u64)),
            Op::AluReg { alu, rd, rs1, rs2 } => {
                (rd, alu.apply(regs[rs1 as usize], regs[rs2 as usize]))
            }
            _ => return,
        };
        if rd != 0 {
            regs[rd as usize] = value;
        }
    }

    #[test]
    fn a_native_alu_block_matches_the_op_semantics() {
        let ops = vec![
            Op::SetImm { rd: 5, value: 0x1234_5678_9abc_def0 },
            Op::AluImm { alu: AluOp::Add, rd: 6, rs1: 5, imm: -0x100 },
            Op::AluReg { alu: AluOp::Xor, rd: 7, rs1: 5, rs2: 6 },
            Op::AluReg { alu: AluOp::Sub, rd: 8, rs1: 6, rs2: 5 },
            Op::AluReg { alu: AluOp::And, rd: 9, rs1: 5, rs2: 7 },
            Op::AluReg { alu: AluOp::Or, rd: 10, rs1: 8, rs2: 9 },
            Op::AluImm { alu: AluOp::Add, rd: 0, rs1: 7, imm: 0xff }, // rd=0: discarded write
        ];
        let mut regs = [0u64; 32];
        regs[5] = 111;
        regs[6] = 222;

        let mut expected = regs;
        for op in &ops {
            ref_apply(&mut expected, op);
        }

        let block = NativeBlock::compile(&ops).expect("an all-ALU block is emittable");
        let retired = block.run(&mut regs);

        assert_eq!(retired, ops.len() as u64, "a fall-through block retires every op");
        assert_eq!(regs, expected, "native register file matches the Op semantics");
    }

    #[test]
    fn a_block_with_an_unemittable_op_does_not_compile() {
        // A branch (a block exit) isn't part of increment 1's ALU set → None, so the
        // caller falls back to Backend A. Also a shift, not yet emitted.
        use crate::block::Cond;
        let with_branch = vec![
            Op::SetImm { rd: 5, value: 1 },
            Op::Branch { cond: Cond::Eq, rs1: 5, rs2: 0, taken: 0x1000, not_taken: 0x1004 },
        ];
        assert!(NativeBlock::compile(&with_branch).is_none(), "a branch isn't emittable yet");
        let with_shift = vec![Op::AluReg { alu: AluOp::Sll, rd: 1, rs1: 2, rs2: 3 }];
        assert!(NativeBlock::compile(&with_shift).is_none(), "shifts aren't emittable yet");
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
