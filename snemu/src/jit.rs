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

use crate::block::{AluOp, Cond, Op};

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

/// One `MAP_JIT` region of executable memory: a bump-allocated chunk of the arena.
struct Chunk {
    ptr: *mut u8,
    cap: usize,
    used: usize,
}

impl Chunk {
    /// Map a fresh `MAP_JIT` RWX region of at least `cap` bytes (rounded to a page).
    fn new(cap: usize) -> Self {
        let page = 16 * 1024; // Apple Silicon page size
        let cap = cap.max(1).div_ceil(page) * page;
        // SAFETY: a standard anonymous mmap. `MAP_JIT` + RWX is the Apple-sanctioned
        // way to get a JIT region under the hardened runtime; a null addr lets the
        // kernel choose. Checked against `MAP_FAILED`.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                cap,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_ANON | libc::MAP_PRIVATE | libc::MAP_JIT,
                -1,
                0,
            )
        };
        assert!(ptr != libc::MAP_FAILED, "mmap MAP_JIT failed (JIT arena)");
        Self { ptr: ptr.cast(), cap, used: 0 }
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`cap` are the exact mapping from `new`, unmapped once.
        unsafe {
            libc::munmap(self.ptr.cast(), self.cap);
        }
    }
}

/// One bump size for the code arena. Blocks are a few hundred bytes, so a 1 MiB chunk
/// holds thousands — amortising the `mmap` syscall from once-per-block to
/// once-per-chunk (the whole point of the arena over the old page-per-block buffer).
const CHUNK_BYTES: usize = 1 << 20;

/// A growable arena of executable memory that compiled blocks are **bump-allocated**
/// into. Replaces the old one-`mmap`-per-block scheme: a single `mmap` per 1 MiB chunk,
/// and [`reset`](CodeArena::reset) rewinds every chunk to reuse the mapping on a cache
/// flush (the caller drops the block pointers at the same instant, so no stale code
/// executes).
pub(crate) struct CodeArena {
    chunks: Vec<Chunk>,
}

impl CodeArena {
    pub(crate) fn new() -> Self {
        Self { chunks: Vec::new() }
    }

    /// Copy `code` into the arena and return its entry pointer. Allocates a fresh chunk
    /// when the current one can't fit it. The write itself is the W^X dance: unlock this
    /// thread's `MAP_JIT` pages, copy, re-lock (making it executable), flush the I-cache.
    /// Instructions are 4 bytes, so `used` stays 4-aligned.
    pub(crate) fn install(&mut self, code: &[u8]) -> *const u8 {
        let need = code.len();
        if self.chunks.last().is_none_or(|c| c.cap - c.used < need) {
            self.chunks.push(Chunk::new(CHUNK_BYTES.max(need)));
        }
        let chunk = self.chunks.last_mut().expect("just pushed a chunk");
        // SAFETY: `dst` is within the chunk (room checked above). Unlocking makes this
        // thread's MAP_JIT pages writable; the copy stays in bounds; re-locking restores
        // execute permission before anyone calls in; the I-cache flush covers exactly the
        // bytes written. The returned pointer is valid until `reset`/drop, and the caller
        // (the block cache) drops it before either happens.
        let dst = unsafe {
            let dst = chunk.ptr.add(chunk.used);
            pthread_jit_write_protect_np(0);
            std::ptr::copy_nonoverlapping(code.as_ptr(), dst, need);
            pthread_jit_write_protect_np(1);
            sys_icache_invalidate(dst.cast(), need);
            dst
        };
        chunk.used += need;
        dst.cast_const()
    }

    /// Rewind every chunk to empty, keeping the mappings for reuse. Called on a cache
    /// flush — safe because the block cache that owns the entry pointers is cleared at
    /// the same moment, so no rewound-over code is ever executed.
    pub(crate) fn reset(&mut self) {
        for chunk in &mut self.chunks {
            chunk.used = 0;
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

    /// `mov Xd, Xm` — register move (`orr Xd, xzr, Xm`).
    fn mov_reg(&mut self, xd: u32, xm: u32) {
        self.emit(0xAA00_0000 | (xm << 16) | (31 << 5) | xd);
    }

    /// `cmp Xn, Xm` — set the condition flags from `Xn − Xm` (`subs xzr, Xn, Xm`).
    fn cmp(&mut self, xn: u32, xm: u32) {
        self.emit(0xEB00_0000 | (xm << 16) | (xn << 5) | 31);
    }

    /// `csel Xd, Xn, Xm, cond` — `Xd = cond ? Xn : Xm`, reading the flags `cmp` set.
    fn csel(&mut self, xd: u32, xn: u32, xm: u32, cond: u32) {
        self.emit(0x9A80_0000 | (xm << 16) | (cond << 12) | (xn << 5) | xd);
    }
}

/// The AArch64 condition code that matches a RISC-V branch condition after a `cmp`
/// of the two operands (signed conditions use N/V, unsigned use C).
fn cond_code(cond: Cond) -> u32 {
    match cond {
        Cond::Eq => 0b0000,  // EQ
        Cond::Ne => 0b0001,  // NE
        Cond::Lt => 0b1011,  // LT (signed <)
        Cond::Ge => 0b1010,  // GE (signed >=)
        Cond::Ltu => 0b0011, // LO/CC (unsigned <)
        Cond::Geu => 0b0010, // HS/CS (unsigned >=)
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

/// What a native block returns: the instructions it retired and the PC to resume at
/// (a branch's resolved target, a jump's target, or the fall-through `exit_pc`).
/// `#[repr(C)]` two-`u64` struct → the AArch64 C ABI returns it in `x0`/`x1`.
#[repr(C)]
pub(crate) struct NativeExit {
    pub retired: u64,
    pub pc: u64,
}

/// A block lowered to native AArch64 (Backend B) — an entry pointer into a
/// [`CodeArena`]. Compiled from the reified `Op` IR; runs against the guest register
/// file passed by pointer and returns [`NativeExit`], architecturally identical to
/// Backend A walking the same ops. The arena owns the memory; the pointer is valid
/// until the arena resets (which the block cache is cleared alongside).
pub(crate) struct NativeBlock {
    entry: *const u8,
}

impl NativeBlock {
    /// Guest register-file base pointer (first C-ABI arg) and caller-saved scratch
    /// registers. A leaf function: no callee-saved clobbers, so no prologue/epilogue.
    const REGS: u32 = 0;
    const A: u32 = 9;
    const B: u32 = 10;
    const T: u32 = 11; // extra scratch for branch/jump targets
    const U: u32 = 12;

    /// Lower a block into `arena`, or `None` if any op isn't lowerable yet (a memory
    /// op, or an ALU family Backend B doesn't emit) — `None` means "run on Backend A".
    /// A block ends at its terminator (`Branch`/`Jump`/`JumpReg`), which sets the exit
    /// PC; a block with no terminator falls through to `exit_pc`.
    pub(crate) fn compile_into(ops: &[Op], exit_pc: u64, arena: &mut CodeArena) -> Option<Self> {
        let retired = u16::try_from(ops.len()).ok()?; // blocks are length-capped
        let mut code = Code::new();
        let mut terminated = false;
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
                // Terminators set x1 = exit pc, then fall through to the shared
                // `finish` (x0 = retired; ret). A terminator is always the block's
                // last op, so nothing follows.
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    code.ldr(Self::A, Self::REGS, u32::from(rs1) * 8);
                    code.ldr(Self::B, Self::REGS, u32::from(rs2) * 8);
                    code.cmp(Self::A, Self::B);
                    code.mov_imm64(Self::T, taken);
                    code.mov_imm64(Self::U, not_taken);
                    code.csel(1, Self::T, Self::U, cond_code(cond));
                    terminated = true;
                }
                Op::Jump { rd, link, target } => {
                    Self::write_link(&mut code, rd, link);
                    code.mov_imm64(1, target);
                    terminated = true;
                }
                Op::JumpReg { rd, rs1, imm, link } => {
                    // Target from rs1 (+imm, clear bit 0) *before* writing rd — they
                    // may alias, and the target uses rs1's pre-write value.
                    code.ldr(Self::A, Self::REGS, u32::from(rs1) * 8);
                    code.mov_imm64(Self::B, imm as u64);
                    code.alu(0x8B00_0000, Self::A, Self::A, Self::B); // add
                    code.mov_imm64(Self::B, !1u64);
                    code.alu(0x8A00_0000, Self::A, Self::A, Self::B); // and → clear bit 0
                    Self::write_link(&mut code, rd, link);
                    code.mov_reg(1, Self::A);
                    terminated = true;
                }
                // Memory ops aren't emitted yet → Backend A.
                Op::Load { .. } | Op::Store { .. } => return None,
            }
            if terminated {
                break;
            }
        }
        if !terminated {
            code.mov_imm64(1, exit_pc); // fall-through PC
        }
        code.movz(0, retired); // x0 = retired
        code.ret();

        Some(Self { entry: arena.install(code.bytes()) })
    }

    /// Store scratch `A` (the op's result) into guest `x[rd]`, skipping `rd == 0`
    /// (x0 is hardwired zero — a legal discarded write).
    fn write_rd(code: &mut Code, rd: u8) {
        if rd != 0 {
            code.str(Self::A, Self::REGS, u32::from(rd) * 8);
        }
    }

    /// Write a jump's link address into `x[rd]` (skipping `rd == 0`), via scratch `T`.
    fn write_link(code: &mut Code, rd: u8, link: u64) {
        if rd != 0 {
            code.mov_imm64(Self::T, link);
            code.str(Self::T, Self::REGS, u32::from(rd) * 8);
        }
    }

    /// Execute the block against `regs`, returning the retired count + resume PC.
    pub(crate) fn run(&self, regs: &mut [u64; 32]) -> NativeExit {
        // SAFETY: `compile_into` emitted a function of exactly this C ABI — one pointer
        // arg (the register-file base) returning a two-word `NativeExit` in x0/x1 — that
        // only reads/writes the 32 words behind `regs` and clobbers caller-saved scratch.
        // `entry` points into a live arena chunk (not yet reset), executable.
        let f: extern "C" fn(*mut u64) -> NativeExit =
            unsafe { std::mem::transmute(self.entry) };
        f(regs.as_mut_ptr())
    }
}

/// A per-hart cache of compiled native blocks, keyed by entry PC. Mirrors the block
/// cache's lifecycle: populated lazily, flushed with it on `satp`/`sfence` (stale
/// translations ⇒ stale native code). A miss that isn't natively compilable caches
/// `None`, so we don't re-attempt compilation every visit.
pub(crate) struct NativeCache {
    blocks: std::collections::HashMap<u64, Option<NativeBlock>>,
    arena: CodeArena,
}

// SAFETY: a `NativeCache` owns its `CodeArena` (and thus its mmap'd chunks)
// exclusively (no aliasing), and the code is immutable once installed and lives in
// process-global memory. Moving the cache to another thread — which the audit does
// when it clones a `Machine` onto a worker — is therefore sound. (`Machine: Send +
// Sync` requires this, since the raw chunk pointers are otherwise neither.)
unsafe impl Send for NativeCache {}
unsafe impl Sync for NativeCache {}

impl NativeCache {
    pub(crate) fn new() -> Self {
        Self { blocks: std::collections::HashMap::new(), arena: CodeArena::new() }
    }

    /// Drop every compiled block and rewind the arena — the design's "rebuild lazily"
    /// invalidation, called wherever the block cache flushes. Clearing `blocks` (the
    /// entry pointers) *before* resetting the arena is what makes the rewind safe.
    pub(crate) fn flush(&mut self) {
        self.blocks.clear();
        self.arena.reset();
    }

    /// Run the block entered at `pc` natively, compiling + caching it on first visit.
    /// Returns the exit (retired + resume PC), or `None` if the block isn't natively
    /// compilable — the caller then runs Backend A.
    pub(crate) fn run(
        &mut self,
        pc: u64,
        ops: &[Op],
        exit_pc: u64,
        regs: &mut [u64; 32],
    ) -> Option<NativeExit> {
        let compiled = self
            .blocks
            .entry(pc)
            .or_insert_with(|| NativeBlock::compile_into(ops, exit_pc, &mut self.arena));
        compiled.as_ref().map(|block| block.run(regs))
    }
}

impl Clone for NativeCache {
    /// A cloned `Machine` (a snapshot/fork) starts with a **cold** native cache — the
    /// code is a pure, rebuildable function of the immutable kernel, so it must not
    /// enter the snapshot (the block cache does the same). This is why the raw-pointer
    /// buffers never need to be `Clone`.
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl Default for NativeCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_function_returning_a_constant_runs() {
        // `movz x0, #42 ; ret` — the smallest possible generated function.
        let mut arena = CodeArena::new();
        let mut code = Code::new();
        code.movz(0, 42);
        code.ret();
        let entry = arena.install(code.bytes());
        // SAFETY: the arena holds a valid `movz;ret` with the C ABI (no args, u64
        // return in x0), matching this fn-pointer type.
        let f: extern "C" fn() -> u64 = unsafe { std::mem::transmute(entry) };
        assert_eq!(f(), 42);
    }

    use crate::block::{AluOp, Cond, Op};

    /// Reference execution mirroring Backend A over the same ops (using the canonical
    /// `AluOp::apply` / `Cond::eval`), returning `(retired, resume_pc)` and mutating
    /// `regs`. Backend A is already proven identical to the interpreter, so native ==
    /// this == A == interpreter.
    fn ref_exec(regs: &mut [u64; 32], ops: &[Op], exit_pc: u64) -> (u64, u64) {
        let w = |regs: &mut [u64; 32], rd: u8, v: u64| {
            if rd != 0 {
                regs[rd as usize] = v;
            }
        };
        for (i, op) in ops.iter().enumerate() {
            let retired = i as u64 + 1;
            match *op {
                Op::SetImm { rd, value } => w(regs, rd, value),
                Op::AluImm { alu, rd, rs1, imm } => {
                    w(regs, rd, alu.apply(regs[rs1 as usize], imm as u64));
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    w(regs, rd, alu.apply(regs[rs1 as usize], regs[rs2 as usize]));
                }
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    let take = cond.eval(regs[rs1 as usize], regs[rs2 as usize]);
                    return (retired, if take { taken } else { not_taken });
                }
                Op::Jump { rd, link, target } => {
                    w(regs, rd, link);
                    return (retired, target);
                }
                Op::JumpReg { rd, rs1, imm, link } => {
                    let target = regs[rs1 as usize].wrapping_add(imm as u64) & !1;
                    w(regs, rd, link);
                    return (retired, target);
                }
                Op::Load { .. } | Op::Store { .. } => unreachable!("no mem ops in these tests"),
            }
        }
        (ops.len() as u64, exit_pc)
    }

    /// Run `ops` natively and against the reference; assert identical retired count,
    /// resume PC, and full register file — the codegen oracle.
    #[track_caller]
    fn assert_native_matches(ops: &[Op], init: [u64; 32], exit_pc: u64) {
        let mut expected = init;
        let (exp_retired, exp_pc) = ref_exec(&mut expected, ops, exit_pc);

        let mut regs = init;
        let mut arena = CodeArena::new();
        let block = NativeBlock::compile_into(ops, exit_pc, &mut arena).expect("block is emittable");
        let exit = block.run(&mut regs);

        assert_eq!(exit.retired, exp_retired, "retired count");
        assert_eq!(exit.pc, exp_pc, "resume pc");
        assert_eq!(regs, expected, "register file");
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
        let mut init = [0u64; 32];
        init[5] = 111;
        init[6] = 222;
        // A fall-through block resumes at exit_pc.
        assert_native_matches(&ops, init, 0xffff_ffff_8020_1234);
    }

    #[test]
    fn a_native_conditional_branch_resolves_both_directions() {
        let mut init = [0u64; 32];
        init[5] = 7;
        init[6] = 7;
        // rs1==rs2 → Eq taken; also exercise Lt (signed) with 7 vs 7 (not <).
        let taken = 0x1000;
        let not_taken = 0x1004;
        // Equal operands: Beq → taken, Bne → not_taken, Blt → not_taken.
        assert_native_matches(
            &[Op::Branch { cond: Cond::Eq, rs1: 5, rs2: 6, taken, not_taken }],
            init,
            0,
        );
        assert_native_matches(
            &[Op::Branch { cond: Cond::Ne, rs1: 5, rs2: 6, taken, not_taken }],
            init,
            0,
        );
        // Signed vs unsigned: rs1 = -1 (0xFFFF..), rs2 = 1. Signed -1 < 1 (Lt taken);
        // unsigned 0xFFFF.. > 1 (Ltu not-taken) — proves the flag choice.
        let mut signed = [0u64; 32];
        signed[5] = u64::MAX;
        signed[6] = 1;
        for cond in [Cond::Lt, Cond::Ge, Cond::Ltu, Cond::Geu] {
            assert_native_matches(
                &[Op::Branch { cond, rs1: 5, rs2: 6, taken, not_taken }],
                signed,
                0,
            );
        }
    }

    #[test]
    fn native_jump_and_jumpreg_set_the_link_and_target() {
        let mut init = [0u64; 32];
        init[5] = 0x2000_0003; // JumpReg base; +imm then clear bit 0
        // JAL: link into rd, jump to a constant target.
        assert_native_matches(
            &[Op::Jump { rd: 1, link: 0xffff_0004, target: 0x8000_0000 }],
            init,
            0,
        );
        // JALR: target = (x5 + 8) & ~1, link into rd (aliasing rd=5 with rs1=5).
        assert_native_matches(
            &[Op::JumpReg { rd: 5, rs1: 5, imm: 8, link: 0xabc0_0004 }],
            init,
            0,
        );
    }

    #[test]
    fn a_block_with_a_memory_op_does_not_compile() {
        // Memory ops aren't emitted yet → None, so the caller falls back to Backend A.
        let mut arena = CodeArena::new();
        let with_load = vec![Op::Load { funct3: 3, rd: 1, rs1: 2, imm: 0, pc: 0x1000 }];
        assert!(
            NativeBlock::compile_into(&with_load, 0, &mut arena).is_none(),
            "a load isn't emittable yet",
        );
        // A shift ALU also isn't emitted yet.
        let with_shift = vec![Op::AluReg { alu: AluOp::Sll, rd: 1, rs1: 2, rs2: 3 }];
        assert!(
            NativeBlock::compile_into(&with_shift, 0, &mut arena).is_none(),
            "shifts aren't emittable yet",
        );
    }

    #[test]
    fn a_generated_function_can_add_its_two_arguments() {
        // `add x0, x0, x1 ; ret` — proves argument passing (x0, x1) + a real ALU op.
        let mut arena = CodeArena::new();
        let mut code = Code::new();
        code.add(0, 0, 1);
        code.ret();
        let entry = arena.install(code.bytes());
        // SAFETY: the arena holds `add x0,x0,x1; ret`, matching this two-arg C ABI.
        let f: extern "C" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(entry) };
        assert_eq!(f(3, 4), 7);
        assert_eq!(f(1000, 337), 1337);
    }

    #[test]
    fn the_arena_bump_allocates_many_blocks_and_resets() {
        // Two distinct blocks in one arena get distinct entries and both run; reset
        // rewinds so the space is reused (the flush path).
        let mut arena = CodeArena::new();
        let a = NativeBlock::compile_into(
            &[Op::SetImm { rd: 1, value: 7 }],
            0x10,
            &mut arena,
        )
        .unwrap();
        let b = NativeBlock::compile_into(
            &[Op::SetImm { rd: 1, value: 9 }],
            0x20,
            &mut arena,
        )
        .unwrap();
        let mut regs = [0u64; 32];
        assert_eq!(a.run(&mut regs).pc, 0x10);
        assert_eq!(regs[1], 7);
        assert_eq!(b.run(&mut regs).pc, 0x20);
        assert_eq!(regs[1], 9);
        arena.reset(); // safe once the block handles above are dropped by the caller
    }
}
