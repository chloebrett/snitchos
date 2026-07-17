//! Backend B — the native block JIT (design: `plans/legacy/snemu-milestone-6-block-jit.md`).
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
    /// Reserved host registers. `REGS` (x0) holds the guest register-file base pointer
    /// during the block and the retired count at return; `PC` (x1) returns the resume
    /// PC; `SA`/`SB` (x9/x10) are scratch — immediates, branch/jump targets, the compare.
    /// A leaf function: no callee-saved clobbers, so no prologue/epilogue.
    const REGS: u32 = 0;
    const PC: u32 = 1;
    const SA: u32 = 9;
    const SB: u32 = 10;
    /// The AArch64 zero register (`xzr`) — guest `x0` reads as this and needs no slot.
    const XZR: u32 = 31;
    /// Caller-saved host registers a block's guest registers live in across its whole
    /// run — loaded once at entry, stored once at exit, so the body is pure register
    /// ALU (this is what beats the register-*array* interpreter). 12 slots cover typical
    /// blocks; a block touching more distinct guest registers falls back to Backend A.
    const POOL: [u32; 12] = [2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15];

    /// Lower a block into `arena` with **cross-op register allocation** — guest
    /// registers stay in host registers for the whole block. `None` if any op isn't
    /// lowerable (a memory op, an ALU family Backend B doesn't emit, or more distinct
    /// guest registers than the host pool holds) — `None` means "run on Backend A". A
    /// block ends at its terminator, which computes the exit PC; a block with no
    /// terminator falls through to `exit_pc`.
    pub(crate) fn compile_into(ops: &[Op], exit_pc: u64, arena: &mut CodeArena) -> Option<Self> {
        let retired = u16::try_from(ops.len()).ok()?; // blocks are length-capped

        // Pass 1: which guest registers are read (load at entry) / written (store at
        // exit), rejecting any op Backend B can't emit. `x0` is never tracked (`xzr`).
        let mut read = [false; 32];
        let mut dirty = [false; 32];
        let src = |r: u8, read: &mut [bool; 32]| {
            if r != 0 {
                read[usize::from(r)] = true;
            }
        };
        let dst = |r: u8, dirty: &mut [bool; 32]| {
            if r != 0 {
                dirty[usize::from(r)] = true;
            }
        };
        for op in ops {
            match *op {
                // `SetImm` and `Jump` both only write `rd` (no source register).
                Op::SetImm { rd, .. } | Op::Jump { rd, .. } => dst(rd, &mut dirty),
                Op::AluImm { alu, rd, rs1, .. } => {
                    alu_base(alu)?;
                    src(rs1, &mut read);
                    dst(rd, &mut dirty);
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    alu_base(alu)?;
                    src(rs1, &mut read);
                    src(rs2, &mut read);
                    dst(rd, &mut dirty);
                }
                Op::Branch { rs1, rs2, .. } => {
                    src(rs1, &mut read);
                    src(rs2, &mut read);
                }
                Op::JumpReg { rd, rs1, .. } => {
                    src(rs1, &mut read);
                    dst(rd, &mut dirty);
                }
                Op::Load { .. } | Op::Store { .. } => return None,
            }
        }

        // Assign a host register to every guest register the block touches.
        let mut host = [Self::XZR; 32];
        let mut next = 0;
        for g in 1..32 {
            if read[g] || dirty[g] {
                host[g] = *Self::POOL.get(next)?; // more regs than the pool → Backend A
                next += 1;
            }
        }
        let h = |g: u8| host[usize::from(g)];

        let mut code = Code::new();
        // Prologue: load each read guest register into its host register.
        for g in 1..32 {
            if read[g] {
                code.ldr(host[g], Self::REGS, g as u32 * 8);
            }
        }

        // Body: pure host-register ALU. A terminator computes the exit PC into `SA`.
        let mut terminated = false;
        let mut exit_in_sa = false;
        for op in ops {
            match *op {
                Op::SetImm { rd, value } => {
                    if rd != 0 {
                        code.mov_imm64(h(rd), value);
                    }
                }
                Op::AluImm { alu, rd, rs1, imm } => {
                    if rd != 0 {
                        let base = alu_base(alu)?;
                        code.mov_imm64(Self::SA, imm as u64);
                        code.alu(base, h(rd), h(rs1), Self::SA);
                    }
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    if rd != 0 {
                        let base = alu_base(alu)?;
                        code.alu(base, h(rd), h(rs1), h(rs2));
                    }
                }
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    code.cmp(h(rs1), h(rs2));
                    code.mov_imm64(Self::SA, taken);
                    code.mov_imm64(Self::SB, not_taken);
                    code.csel(Self::SA, Self::SA, Self::SB, cond_code(cond));
                    exit_in_sa = true;
                    terminated = true;
                }
                Op::Jump { rd, link, target } => {
                    if rd != 0 {
                        code.mov_imm64(h(rd), link);
                    }
                    code.mov_imm64(Self::SA, target);
                    exit_in_sa = true;
                    terminated = true;
                }
                Op::JumpReg { rd, rs1, imm, link } => {
                    // Target = (x[rs1] + imm) & !1, computed into SA *before* writing rd
                    // (they may alias); then the link goes to rd's host register.
                    code.mov_imm64(Self::SB, imm as u64);
                    code.alu(0x8B00_0000, Self::SA, h(rs1), Self::SB); // add
                    code.mov_imm64(Self::SB, !1u64);
                    code.alu(0x8A00_0000, Self::SA, Self::SA, Self::SB); // and → clear bit 0
                    if rd != 0 {
                        code.mov_imm64(h(rd), link);
                    }
                    exit_in_sa = true;
                    terminated = true;
                }
                Op::Load { .. } | Op::Store { .. } => return None,
            }
            if terminated {
                break;
            }
        }

        // Epilogue: store every dirtied guest register back (reads host regs + the base
        // in x0, so it must precede overwriting x0/x1; the exit PC in SA survives — the
        // stores never touch it), then return `(retired, pc)`.
        for g in 1..32 {
            if dirty[g] {
                code.str(host[g], Self::REGS, g as u32 * 8);
            }
        }
        code.movz(0, retired); // x0 = retired (base no longer needed)
        if exit_in_sa {
            code.mov_reg(Self::PC, Self::SA);
        } else {
            code.mov_imm64(Self::PC, exit_pc); // fall-through PC
        }
        code.ret();

        Some(Self { entry: arena.install(code.bytes()) })
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

/// Number of direct-mapped native-cache slots (power of two). Large enough that a
/// scenario's hot block set rarely aliases (an alias just recompiles).
const NATIVE_SLOTS: usize = 4096;

/// One direct-mapped slot: the compiled result for `tag` (its entry PC), valid while
/// `epoch` matches the cache. `result` is `Some` for a compiled block, `None` for a
/// resolved-but-uncompilable PC (so we don't re-attempt compilation) — the two are
/// distinguished from an *empty* slot by the epoch/tag match, not by `result`.
#[derive(Default)]
struct NativeSlot {
    epoch: u64,
    tag: u64,
    result: Option<NativeBlock>,
}

/// A per-hart cache of compiled native blocks, **direct-mapped by entry PC** (not a
/// `HashMap` — a `SipHash` probe per block execution measured as real overhead in the
/// hot loop, which Backend A doesn't pay). Populated lazily; flushed with the block
/// cache on `satp`/`sfence` (stale translations ⇒ stale native code) via an O(1)
/// epoch bump plus an arena rewind.
pub(crate) struct NativeCache {
    epoch: u64,
    slots: Box<[NativeSlot]>,
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
        // Epoch starts at 1 so the zero-initialised slots (epoch 0) are stale.
        let slots = (0..NATIVE_SLOTS).map(|_| NativeSlot::default()).collect();
        Self { epoch: 1, slots, arena: CodeArena::new() }
    }

    #[inline]
    fn index(pc: u64) -> usize {
        ((pc >> 1) as usize) & (NATIVE_SLOTS - 1)
    }

    /// Invalidate every slot (O(1) epoch bump) and rewind the arena — the design's
    /// "rebuild lazily" invalidation, called wherever the block cache flushes. Bumping
    /// the epoch (so no slot is read) *before* rewinding the arena is what makes reuse
    /// of the arena memory safe.
    pub(crate) fn flush(&mut self) {
        self.epoch += 1;
        self.arena.reset();
    }

    /// Run the block entered at `pc` natively, compiling + caching it on first visit.
    /// Returns the exit (retired + resume PC), or `None` if the block isn't natively
    /// compilable — the caller then runs Backend A. The hot path (a live slot) is a
    /// single direct-mapped array index + tag compare, then the native call.
    pub(crate) fn run(
        &mut self,
        pc: u64,
        ops: &[Op],
        exit_pc: u64,
        regs: &mut [u64; 32],
    ) -> Option<NativeExit> {
        let idx = Self::index(pc);
        let slot = &self.slots[idx];
        if slot.epoch == self.epoch && slot.tag == pc {
            return slot.result.as_ref().map(|block| block.run(regs));
        }
        // Miss (empty / stale / aliased): compile, run, then install into the slot.
        let result = NativeBlock::compile_into(ops, exit_pc, &mut self.arena);
        let exit = result.as_ref().map(|block| block.run(regs));
        self.slots[idx] = NativeSlot { epoch: self.epoch, tag: pc, result };
        exit
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
    fn register_reuse_across_a_long_block_matches_the_semantics() {
        // An accumulator-style block that reads/writes the same handful of registers
        // many times — exactly the pattern where keeping them in host registers wins,
        // and where a reg-alloc bug (stale load, missed store, alias) would show.
        let mut ops = Vec::new();
        for _ in 0..8 {
            ops.push(Op::AluReg { alu: AluOp::Add, rd: 5, rs1: 5, rs2: 6 });
            ops.push(Op::AluReg { alu: AluOp::Xor, rd: 6, rs1: 6, rs2: 7 });
            ops.push(Op::AluImm { alu: AluOp::Sub, rd: 7, rs1: 7, imm: 1 });
        }
        ops.push(Op::Branch { cond: Cond::Ne, rs1: 5, rs2: 6, taken: 0x400, not_taken: 0x404 });
        let mut init = [0u64; 32];
        init[5] = 0x1111;
        init[6] = 0x2222;
        init[7] = 40;
        assert_native_matches(&ops, init, 0);
    }

    #[test]
    fn a_block_touching_too_many_registers_falls_back() {
        // 13 distinct destination registers > the 12-slot host pool → None (Backend A).
        let mut arena = CodeArena::new();
        let ops: Vec<Op> = (1..=13).map(|g| Op::SetImm { rd: g, value: g as u64 }).collect();
        assert!(
            NativeBlock::compile_into(&ops, 0, &mut arena).is_none(),
            "more guest regs than the host pool → fall back to Backend A",
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
