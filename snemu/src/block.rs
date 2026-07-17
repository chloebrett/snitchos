//! snemu M6 — the block JIT's **reified IR** and portable executor (backend A).
//!
//! A basic block (a straight run of instructions ending at a control-flow or
//! trapping instruction) is compiled once into a `Vec<Op>` of plain data — a
//! reified enum with operands already resolved (register indices, sign-extended
//! immediates, absolute PCs). Executing it walks the ops; a future native backend
//! (B) lowers the same ops to machine-code stencils. **The IR is data, never
//! closures** — that is the seam that lets backend B reuse this frontend.
//!
//! This module is the IR + backend A only. Block *discovery* (guest bytes → `Block`)
//! and the `PC → block` cache land in later increments. Correctness rides the same
//! oracle discipline as the decode cache: a block must execute byte-identically to
//! the interpreter, proven by the `snemu-itest` on↔off A/B.
//!
//! See `plans/legacy/snemu-milestone-6-block-jit.md`.

use std::sync::Arc;

use crate::bus::Bus;
use crate::cpu::{Hart, StepError};
use crate::decode::{Instr, funct3, funct7, opcode};

/// How many times an entry PC is interpreted before it's compiled — so genuinely
/// one-shot code never pays compilation, only hot blocks do.
const HOT_THRESHOLD: u32 = 2;

/// Direct-mapped cache index bits: 2^14 = 16384 slots. Block entries are far
/// sparser than instructions (only branch/jump targets start blocks), so this is
/// ample while keeping the per-fork snapshot clone cheap. Indexed by `(pc >> 1)` —
/// instructions are ≥2-byte aligned.
const INDEX_BITS: u32 = 14;
const SLOTS: usize = 1 << INDEX_BITS;
const INDEX_MASK: u64 = (SLOTS as u64) - 1;

/// What a cache slot holds for its PC: a hotness count (still being interpreted) or
/// a compiled block (`Arc` so a lookup is a refcount clone, not a deep copy — the
/// executor needs `&mut Hart`, which the cache lives inside).
#[derive(Clone, Default)]
enum Entry {
    #[default]
    Empty,
    Hot(u32),
    Compiled(Arc<Block>),
}

/// One direct-mapped slot: the epoch it was written in (for O(1) flush), the full
/// PC tag (to reject an aliasing neighbour that shares the index), and its entry.
#[derive(Clone, Default)]
struct Slot {
    epoch: u64,
    tag: u64,
    entry: Entry,
}

/// A per-hart `PC → compiled block` cache with hotness tiering — the block JIT's
/// analogue of the decode cache, and **direct-mapped for the same reason**: a PC
/// indexes straight into `slots` with a shift+mask (no hashing, which the decode
/// cache measured *slower* than the work it saved; the HashMap version of this
/// cache made the JIT net-slower than the interpreter). Invalidation rides the
/// guest's TLB contract via an O(1) epoch bump — cleared on a `satp` write and
/// `sfence.vma`. Clones as plain data for the snapshot; blocks are immutable, so
/// the shared `Arc`s across a fork are harmless.
#[derive(Clone)]
pub(crate) struct BlockCache {
    /// Slots written with an earlier epoch are stale. Starts at 1 so the
    /// zero-initialised slots (epoch 0) are invalid from birth.
    epoch: u64,
    slots: Box<[Slot]>,
    hits: u64,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self { epoch: 1, slots: vec![Slot::default(); SLOTS].into_boxed_slice(), hits: 0 }
    }
}

impl BlockCache {
    #[inline]
    fn index(pc: u64) -> usize {
        ((pc >> 1) & INDEX_MASK) as usize
    }

    /// The compiled block at `pc` (a cheap `Arc` clone), or `None` if the slot holds
    /// no live compiled block for this exact PC. Bumps the hit counter; the cache
    /// borrow ends with the clone, freeing `&mut Hart` for the executor.
    pub(crate) fn get(&mut self, pc: u64) -> Option<Arc<Block>> {
        let slot = &self.slots[Self::index(pc)];
        if slot.epoch == self.epoch
            && slot.tag == pc
            && let Entry::Compiled(block) = &slot.entry
        {
            self.hits += 1;
            return Some(Arc::clone(block));
        }
        None
    }

    /// Count a cold interpretation of entry `pc` (called only after `get` missed);
    /// return `true` on the visit that crosses the hotness threshold, so the caller
    /// compiles once. A miss or an aliasing eviction restarts the count at 1.
    pub(crate) fn record_hot(&mut self, pc: u64) -> bool {
        let epoch = self.epoch;
        let slot = &mut self.slots[Self::index(pc)];
        let count = match &slot.entry {
            Entry::Hot(n) if slot.epoch == epoch && slot.tag == pc => *n + 1,
            _ => 1,
        };
        *slot = Slot { epoch, tag: pc, entry: Entry::Hot(count) };
        count >= HOT_THRESHOLD
    }

    pub(crate) fn insert(&mut self, pc: u64, block: Arc<Block>) {
        self.slots[Self::index(pc)] =
            Slot { epoch: self.epoch, tag: pc, entry: Entry::Compiled(block) };
    }

    /// Invalidate every slot in O(1) — the guest invalidated translations (`satp`
    /// write / `sfence.vma`), so cached (translated) blocks are stale.
    pub(crate) fn flush(&mut self) {
        self.epoch += 1;
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }
}

/// The result of trying to lower one guest instruction to IR.
pub(crate) enum Compiled {
    /// A mid-block op — keep walking to the next instruction.
    Continue(Op),
    /// A block-ending op (a branch) — append it and stop.
    Terminate(Op),
    /// An instruction the compiler doesn't lower yet (or a block boundary like a
    /// jump/`ecall`/memory op). End the block *before* it; the interpreter runs it.
    Unsupported,
}

/// Lower one already-decoded instruction (`raw`, `ilen`, at absolute `pc`) to IR.
/// Pure — the frontend both backends share. Branch targets are resolved to absolute
/// addresses here so the executor never re-reads `pc`. Grows one instruction family
/// at a time, each driven by an equivalence test against the interpreter.
pub(crate) fn compile_op(raw: u32, ilen: u64, pc: u64) -> Compiled {
    let instr = Instr(raw);
    let mid = |op: Option<Op>| op.map_or(Compiled::Unsupported, Compiled::Continue);
    match instr.opcode() {
        opcode::LUI => Compiled::Continue(Op::SetImm { rd: instr.rd() as u8, value: instr.u_imm() }),
        opcode::AUIPC => Compiled::Continue(Op::SetImm {
            rd: instr.rd() as u8,
            value: pc.wrapping_add(instr.u_imm()),
        }),
        opcode::OP_IMM => mid(alu_imm(instr)),
        opcode::OP if instr.funct7() != funct7::MULDIV => mid(alu_reg(instr)),
        opcode::OP_IMM_32 => mid(alu_imm_32(instr)),
        opcode::OP_32 if instr.funct7() != funct7::MULDIV => mid(alu_reg_32(instr)),
        opcode::LOAD => mid(load_op(instr, pc)),
        opcode::STORE => mid(store_op(instr, pc)),
        opcode::JAL => Compiled::Terminate(Op::Jump {
            rd: instr.rd() as u8,
            link: pc.wrapping_add(ilen),
            target: pc.wrapping_add(instr.j_imm()),
        }),
        opcode::JALR if instr.funct3() == 0 => Compiled::Terminate(Op::JumpReg {
            rd: instr.rd() as u8,
            rs1: instr.rs1() as u8,
            imm: instr.i_imm() as i64,
            link: pc.wrapping_add(ilen),
        }),
        opcode::BRANCH => branch_cond(instr.funct3()).map_or(Compiled::Unsupported, |cond| {
            Compiled::Terminate(Op::Branch {
                cond,
                rs1: instr.rs1() as u8,
                rs2: instr.rs2() as u8,
                taken: pc.wrapping_add(instr.b_imm()),
                not_taken: pc.wrapping_add(ilen),
            })
        }),
        _ => Compiled::Unsupported,
    }
}

/// Lower an `OP-IMM` instruction. Shifts carry their shift amount as `imm`; the
/// rest carry the sign-extended immediate. Mirrors the interpreter's `op_imm`.
fn alu_imm(instr: Instr) -> Option<Op> {
    let (alu, imm) = match instr.funct3() {
        funct3::ADD => (AluOp::Add, instr.i_imm() as i64),
        funct3::SLT => (AluOp::Slt, instr.i_imm() as i64),
        funct3::SLTU => (AluOp::Sltu, instr.i_imm() as i64),
        funct3::XOR => (AluOp::Xor, instr.i_imm() as i64),
        funct3::OR => (AluOp::Or, instr.i_imm() as i64),
        funct3::AND => (AluOp::And, instr.i_imm() as i64),
        funct3::SLL => (AluOp::Sll, i64::from(instr.shamt6())),
        funct3::SR if instr.is_alt_op() => (AluOp::Sra, i64::from(instr.shamt6())),
        funct3::SR => (AluOp::Srl, i64::from(instr.shamt6())),
        _ => return None,
    };
    Some(Op::AluImm { alu, rd: instr.rd() as u8, rs1: instr.rs1() as u8, imm })
}

/// Lower an `OP` (register-register) instruction. Mirrors the interpreter's `op`.
fn alu_reg(instr: Instr) -> Option<Op> {
    let alu = match instr.funct3() {
        funct3::ADD if instr.is_alt_op() => AluOp::Sub,
        funct3::ADD => AluOp::Add,
        funct3::SLL => AluOp::Sll,
        funct3::SLT => AluOp::Slt,
        funct3::SLTU => AluOp::Sltu,
        funct3::XOR => AluOp::Xor,
        funct3::SR if instr.is_alt_op() => AluOp::Sra,
        funct3::SR => AluOp::Srl,
        funct3::OR => AluOp::Or,
        funct3::AND => AluOp::And,
        _ => return None,
    };
    Some(Op::AluReg { alu, rd: instr.rd() as u8, rs1: instr.rs1() as u8, rs2: instr.rs2() as u8 })
}

/// Lower an `OP-IMM-32` (`.w`) instruction. Mirrors `op_imm_32`.
fn alu_imm_32(instr: Instr) -> Option<Op> {
    let (alu, imm) = match instr.funct3() {
        funct3::ADD => (AluOp::AddW, instr.i_imm() as i64),
        funct3::SLL => (AluOp::SllW, i64::from(instr.shamt5())),
        funct3::SR if instr.is_alt_op() => (AluOp::SraW, i64::from(instr.shamt5())),
        funct3::SR => (AluOp::SrlW, i64::from(instr.shamt5())),
        _ => return None,
    };
    Some(Op::AluImm { alu, rd: instr.rd() as u8, rs1: instr.rs1() as u8, imm })
}

/// Lower an `OP-32` (`.w` register-register) instruction. Mirrors `op_32`.
fn alu_reg_32(instr: Instr) -> Option<Op> {
    let alu = match instr.funct3() {
        funct3::ADD if instr.is_alt_op() => AluOp::SubW,
        funct3::ADD => AluOp::AddW,
        funct3::SLL => AluOp::SllW,
        funct3::SR if instr.is_alt_op() => AluOp::SraW,
        funct3::SR => AluOp::SrlW,
        _ => return None,
    };
    Some(Op::AluReg { alu, rd: instr.rd() as u8, rs1: instr.rs1() as u8, rs2: instr.rs2() as u8 })
}

/// Lower a `LOAD` instruction (a mid-block op that may fault). Only known widths
/// compile; an unknown funct3 ends the block (the interpreter reports it).
fn load_op(instr: Instr, pc: u64) -> Option<Op> {
    match instr.funct3() {
        funct3::load::LB
        | funct3::load::LH
        | funct3::load::LW
        | funct3::load::LD
        | funct3::load::LBU
        | funct3::load::LHU
        | funct3::load::LWU => Some(Op::Load {
            funct3: instr.funct3(),
            rd: instr.rd() as u8,
            rs1: instr.rs1() as u8,
            imm: instr.i_imm() as i64,
            pc,
        }),
        _ => None,
    }
}

/// Lower a `STORE` instruction (a mid-block op that may fault).
fn store_op(instr: Instr, pc: u64) -> Option<Op> {
    match instr.funct3() {
        funct3::store::SB | funct3::store::SH | funct3::store::SW | funct3::store::SD => {
            Some(Op::Store {
                funct3: instr.funct3(),
                rs1: instr.rs1() as u8,
                rs2: instr.rs2() as u8,
                imm: instr.s_imm() as i64,
                pc,
            })
        }
        _ => None,
    }
}

/// Map a branch `funct3` to its condition.
fn branch_cond(funct3: u32) -> Option<Cond> {
    Some(match funct3 {
        funct3::branch::BEQ => Cond::Eq,
        funct3::branch::BNE => Cond::Ne,
        funct3::branch::BLT => Cond::Lt,
        funct3::branch::BGE => Cond::Ge,
        funct3::branch::BLTU => Cond::Ltu,
        funct3::branch::BGEU => Cond::Geu,
        _ => return None,
    })
}

/// A register-immediate / register-register integer op (`OP-IMM` / `OP` and their
/// 32-bit `.w` forms). The interpreter's `op_imm`/`op`/`op_imm_32`/`op_32`
/// semantics, pre-decoded — `apply(a, b)` treats `b` as the immediate (for
/// `AluImm`) or `x[rs2]` (for `AluReg`); shift ops mask it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AluOp {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    AddW,
    SubW,
    SllW,
    SrlW,
    SraW,
}

/// A branch condition (`funct3` of a `BRANCH`), pre-decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Ltu,
    Geu,
}

/// One reified IR operation. Operands are resolved so execution never re-decodes;
/// PC-dependent instructions carry their target resolved to an absolute address, so
/// a straight block touches `pc` only at its branch exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// `x[rd] = alu(x[rs1], imm)` — a register-immediate op. `rd == 0` is a legal
    /// discarded write (x0 is hardwired zero).
    AluImm { alu: AluOp, rd: u8, rs1: u8, imm: i64 },
    /// `x[rd] = alu(x[rs1], x[rs2])` — a register-register op.
    AluReg { alu: AluOp, rd: u8, rs1: u8, rs2: u8 },
    /// `x[rd] = value` — `LUI` (value = the upper immediate) or `AUIPC` (value =
    /// `pc + imm`, resolved to an absolute at compile time).
    SetImm { rd: u8, value: u64 },
    /// `x[rd] = mem[x[rs1] + imm]` at the width/signedness of `funct3`. Can page
    /// fault: on a fault the trap is taken (with `sepc` = `pc`) and the block bails.
    Load { funct3: u32, rd: u8, rs1: u8, imm: i64, pc: u64 },
    /// `mem[x[rs1] + imm] = x[rs2]` (truncated to the width of `funct3`). Can page
    /// fault and bail like `Load`. `pc` is set before the access so a fault reports it.
    Store { funct3: u32, rs1: u8, rs2: u8, imm: i64, pc: u64 },
    /// Block exit: `pc = cond(x[rs1], x[rs2]) ? taken : not_taken`. Targets are
    /// absolute (resolved at compile time from the branch's PC + immediate).
    Branch { cond: Cond, rs1: u8, rs2: u8, taken: u64, not_taken: u64 },
    /// Block exit (`JAL`): `x[rd] = link; pc = target` — both compile-time absolute.
    Jump { rd: u8, link: u64, target: u64 },
    /// Block exit (`JALR`): `x[rd] = link; pc = (x[rs1] + imm) & !1` — a runtime
    /// target, so it can't be resolved at compile time.
    JumpReg { rd: u8, rs1: u8, imm: i64, link: u64 },
}

/// A compiled basic block: a straight sequence of ops. It ends either at a
/// `Branch` (which sets `pc` itself) or by falling through — running off the end
/// without a branch (block-length cap, or an instruction the compiler couldn't
/// lower), in which case `pc` is set to `exit_pc`, the address just past the last
/// compiled instruction (where the interpreter resumes).
#[derive(Clone, Debug)]
pub(crate) struct Block {
    ops: Vec<Op>,
    exit_pc: u64,
}

impl Block {
    pub(crate) fn new(ops: Vec<Op>, exit_pc: u64) -> Self {
        Self { ops, exit_pc }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The reified ops, for Backend B to lower to native code.
    pub(crate) fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The fall-through resume PC (used when the block has no terminating branch).
    pub(crate) fn exit_pc(&self) -> u64 {
        self.exit_pc
    }

    /// Execute the block against `hart`/`bus`, returning the number of instructions
    /// it retired. Walks the reified ops in order. A `Branch` sets `pc` and ends the
    /// block; a `Load`/`Store` that page-faults takes the trap (setting `pc` to the
    /// handler) and bails — so the returned count can be short of the block length,
    /// exactly the interpreter's instret for the same run. A block with no branch or
    /// fault falls through, setting `pc` to `exit_pc`. The two paths are architecturally
    /// identical (proven by the on↔off A/B); register caching only changes speed.
    pub(crate) fn exec(&self, hart: &mut Hart, bus: &mut Bus) -> Result<u64, StepError> {
        if hart.reg_cache_enabled() {
            self.exec_cached(hart, bus)
        } else {
            self.exec_uncached(hart, bus)
        }
    }

    /// Register-caching executor (M6 increment 4, default).
    fn exec_cached(&self, hart: &mut Hart, bus: &mut Bus) -> Result<u64, StepError> {
        // Register caching (increment 4): copy the register file into a host local,
        // run the whole block against it (no per-op array-through-`&mut Hart`), and
        // write it back once at every exit. `w!` writes, keeping x0 hardwired. Memory
        // ops take/return values (`load_value`/`store_value`) so they don't flush the
        // cache; on a fault we write the cache back before the trap propagates.
        let mut regs = hart.registers();
        macro_rules! w {
            ($rd:expr, $val:expr) => {
                if $rd != 0 {
                    regs[$rd as usize] = $val;
                }
            };
        }
        let mut retired = 0u64;
        for op in &self.ops {
            retired += 1; // this op is about to retire (a faulting mem op still counts)
            match *op {
                Op::AluImm { alu, rd, rs1, imm } => {
                    w!(rd, alu.apply(regs[rs1 as usize], imm as u64));
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    w!(rd, alu.apply(regs[rs1 as usize], regs[rs2 as usize]));
                }
                Op::SetImm { rd, value } => w!(rd, value),
                Op::Load { funct3, rd, rs1, imm, pc } => {
                    // Set pc to this op first, so a page fault's trap reports the
                    // faulting instruction (not the block entry) in sepc.
                    hart.set_pc(pc);
                    match hart.load_value(bus, funct3, regs[rs1 as usize], imm)? {
                        Some(value) => w!(rd, value),
                        None => {
                            hart.set_registers(regs); // faulted → trap took pc; stop
                            return Ok(retired);
                        }
                    }
                }
                Op::Store { funct3, rs1, rs2, imm, pc } => {
                    hart.set_pc(pc);
                    if hart.store_value(bus, funct3, regs[rs1 as usize], imm, regs[rs2 as usize])? {
                        hart.set_registers(regs);
                        return Ok(retired);
                    }
                }
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    let take = cond.eval(regs[rs1 as usize], regs[rs2 as usize]);
                    hart.set_registers(regs);
                    hart.set_pc(if take { taken } else { not_taken });
                    return Ok(retired);
                }
                Op::Jump { rd, link, target } => {
                    w!(rd, link);
                    hart.set_registers(regs);
                    hart.set_pc(target);
                    return Ok(retired);
                }
                Op::JumpReg { rd, rs1, imm, link } => {
                    // Target from rs1 *before* writing rd (they may alias).
                    let target = regs[rs1 as usize].wrapping_add(imm as u64) & !1;
                    w!(rd, link);
                    hart.set_registers(regs);
                    hart.set_pc(target);
                    return Ok(retired);
                }
            }
        }
        hart.set_registers(regs);
        hart.set_pc(self.exit_pc); // fell through without a branch
        Ok(retired)
    }

    /// Through-hart executor: every op reads/writes the register file directly (no
    /// host-local cache). The A/B baseline for register caching — architecturally
    /// identical to [`exec_cached`](Self::exec_cached), only slower (or not).
    fn exec_uncached(&self, hart: &mut Hart, bus: &mut Bus) -> Result<u64, StepError> {
        let mut retired = 0u64;
        for op in &self.ops {
            retired += 1;
            match *op {
                Op::AluImm { alu, rd, rs1, imm } => {
                    hart.set_reg(rd as usize, alu.apply(hart.reg(rs1 as usize), imm as u64));
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    let v = alu.apply(hart.reg(rs1 as usize), hart.reg(rs2 as usize));
                    hart.set_reg(rd as usize, v);
                }
                Op::SetImm { rd, value } => hart.set_reg(rd as usize, value),
                Op::Load { funct3, rd, rs1, imm, pc } => {
                    hart.set_pc(pc);
                    let base = hart.reg(rs1 as usize);
                    match hart.load_value(bus, funct3, base, imm)? {
                        Some(value) => hart.set_reg(rd as usize, value),
                        None => return Ok(retired),
                    }
                }
                Op::Store { funct3, rs1, rs2, imm, pc } => {
                    hart.set_pc(pc);
                    let (base, value) = (hart.reg(rs1 as usize), hart.reg(rs2 as usize));
                    if hart.store_value(bus, funct3, base, imm, value)? {
                        return Ok(retired);
                    }
                }
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    let take = cond.eval(hart.reg(rs1 as usize), hart.reg(rs2 as usize));
                    hart.set_pc(if take { taken } else { not_taken });
                    return Ok(retired);
                }
                Op::Jump { rd, link, target } => {
                    hart.set_reg(rd as usize, link);
                    hart.set_pc(target);
                    return Ok(retired);
                }
                Op::JumpReg { rd, rs1, imm, link } => {
                    let target = hart.reg(rs1 as usize).wrapping_add(imm as u64) & !1;
                    hart.set_reg(rd as usize, link);
                    hart.set_pc(target);
                    return Ok(retired);
                }
            }
        }
        hart.set_pc(self.exit_pc);
        Ok(retired)
    }
}

/// Sign-extend a 32-bit result to 64 bits (the `.w` convention), matching the
/// interpreter's `sext32`.
fn sext32(v: u32) -> u64 {
    i64::from(v as i32) as u64
}

impl AluOp {
    /// Apply the op to two 64-bit operands (`b` is the immediate or `x[rs2]`).
    /// Shifts mask `b` to the width's shift range, exactly as the interpreter does.
    /// `pub(crate)` so Backend B's tests can oracle native codegen against the same
    /// semantics Backend A executes.
    pub(crate) fn apply(self, a: u64, b: u64) -> u64 {
        let sh = (b & 0x3f) as u32; // RV64 shift amount
        let shw = (b & 0x1f) as u32; // .w shift amount
        match self {
            AluOp::Add => a.wrapping_add(b),
            AluOp::Sub => a.wrapping_sub(b),
            AluOp::Sll => a << sh,
            AluOp::Slt => u64::from((a as i64) < (b as i64)),
            AluOp::Sltu => u64::from(a < b),
            AluOp::Xor => a ^ b,
            AluOp::Srl => a >> sh,
            AluOp::Sra => ((a as i64) >> sh) as u64,
            AluOp::Or => a | b,
            AluOp::And => a & b,
            AluOp::AddW => sext32((a as u32).wrapping_add(b as u32)),
            AluOp::SubW => sext32((a as u32).wrapping_sub(b as u32)),
            AluOp::SllW => sext32((a as u32) << shw),
            AluOp::SrlW => sext32((a as u32) >> shw),
            AluOp::SraW => sext32(((a as i32) >> shw) as u32),
        }
    }
}

impl Cond {
    /// Whether the branch is taken for operands `a = x[rs1]`, `b = x[rs2]`.
    /// `pub(crate)` so Backend B's tests can oracle native branch codegen against the
    /// same condition Backend A evaluates.
    pub(crate) fn eval(self, a: u64, b: u64) -> bool {
        match self {
            Cond::Eq => a == b,
            Cond::Ne => a != b,
            Cond::Lt => (a as i64) < (b as i64),
            Cond::Ge => (a as i64) >= (b as i64),
            Cond::Ltu => a < b,
            Cond::Geu => a >= b,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{AluOp, Block, BlockCache, Compiled, Cond, HOT_THRESHOLD, Op, compile_op};
    use crate::bus::Bus;
    use crate::cpu::Hart;
    use crate::mem::Memory;

    fn hart_and_bus() -> (Hart, Bus) {
        (Hart::new(), Bus::new(Memory::new(0x1000)))
    }

    #[test]
    fn a_block_runs_alu_ops_then_takes_a_true_branch() {
        // x1 = x0 + 5; x2 = x1 + 3; if x1 != x2 -> taken. The executor walks the
        // reified IR and lands registers + pc exactly where the instructions would.
        let (mut hart, mut bus) = hart_and_bus();
        let block = Block::new(
            vec![
                Op::AluImm { alu: AluOp::Add, rd: 1, rs1: 0, imm: 5 },
                Op::AluImm { alu: AluOp::Add, rd: 2, rs1: 1, imm: 3 },
                Op::Branch { cond: Cond::Ne, rs1: 1, rs2: 2, taken: 0x2010, not_taken: 0x2004 },
            ],
            0, // exit_pc unused: the branch sets pc
        );

        block.exec(&mut hart, &mut bus).unwrap();

        assert_eq!(hart.reg(1), 5);
        assert_eq!(hart.reg(2), 8);
        assert_eq!(hart.pc(), 0x2010, "x1(5) != x2(8) -> branch taken");
    }

    #[test]
    fn a_reg_reg_op_reads_two_sources() {
        // x3 = x1 + x2, with x1/x2 seeded; then an equal-operand branch falls
        // through (Ne of x3 with itself is false).
        let (mut hart, mut bus) = hart_and_bus();
        hart.set_reg(1, 40);
        hart.set_reg(2, 2);
        let block = Block::new(
            vec![
                Op::AluReg { alu: AluOp::Add, rd: 3, rs1: 1, rs2: 2 },
                Op::Branch { cond: Cond::Ne, rs1: 3, rs2: 3, taken: 0x2010, not_taken: 0x2004 },
            ],
            0,
        );

        block.exec(&mut hart, &mut bus).unwrap();

        assert_eq!(hart.reg(3), 42);
        assert_eq!(hart.pc(), 0x2004, "x3 == x3 -> not taken, fall through");
    }

    #[test]
    fn a_write_to_x0_is_discarded() {
        // x0 is hardwired zero — an ALU op targeting it must not change it.
        let (mut hart, mut bus) = hart_and_bus();
        let block = Block::new(vec![Op::AluImm { alu: AluOp::Add, rd: 0, rs1: 0, imm: 99 }], 0x2004);

        block.exec(&mut hart, &mut bus).unwrap();

        assert_eq!(hart.reg(0), 0);
        assert_eq!(hart.pc(), 0x2004, "no branch -> pc advances to the block's exit");
    }

    #[test]
    fn compile_op_lowers_supported_families_and_rejects_the_rest() {
        // addi -> a mid-block AluImm; lw -> a mid-block Load; bne -> a terminator;
        // jal (a jump) -> a block boundary the compiler doesn't lower yet.
        let addi = 0x0050_0093; // addi x1, x0, 5
        assert!(matches!(
            compile_op(addi, 4, 0x1000),
            Compiled::Continue(Op::AluImm { rd: 1, rs1: 0, imm: 5, .. })
        ));
        let lw = 0x0000_2083; // lw x1, 0(x0)
        assert!(matches!(
            compile_op(lw, 4, 0x1000),
            Compiled::Continue(Op::Load { rd: 1, rs1: 0, imm: 0, .. })
        ));
        let bne = 0x0020_9463; // bne x1, x2, +8
        assert!(matches!(
            compile_op(bne, 4, 0x1000),
            Compiled::Terminate(Op::Branch { rs1: 1, rs2: 2, taken, not_taken: 0x1004, .. })
                if taken == 0x1008
        ));
        let ecall = 0x0000_0073; // ecall — a trap boundary the compiler doesn't lower
        assert!(matches!(compile_op(ecall, 4, 0x1000), Compiled::Unsupported));
    }

    #[test]
    fn a_pc_compiles_only_once_it_is_hot() {
        // The hotness gate: an entry PC is interpreted `HOT_THRESHOLD` times before
        // `record_hot` says compile — genuinely one-shot code never compiles.
        let mut cache = BlockCache::default();
        for _ in 0..HOT_THRESHOLD - 1 {
            assert!(!cache.record_hot(0x1000), "cold visits don't compile");
        }
        assert!(cache.record_hot(0x1000), "the threshold visit triggers a compile");
    }

    #[test]
    fn get_returns_an_inserted_block_and_flush_drops_it() {
        let mut cache = BlockCache::default();
        assert!(cache.get(0x2000).is_none(), "cold lookup misses");
        cache.insert(0x2000, Arc::new(Block::new(vec![], 0x2004)));
        assert!(cache.get(0x2000).is_some(), "warm lookup hits");
        cache.flush();
        assert!(cache.get(0x2000).is_none(), "flush (satp/sfence) drops the block");
    }
}
