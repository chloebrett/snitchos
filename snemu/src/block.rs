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
//! See `plans/snemu-milestone-6-block-jit.md`.

// The IR + executor are consumed by block discovery (increment 2) and the
// `PC → block` dispatch in `Hart::step` (increment 3). Until the interpreter calls
// `Block::exec`, they are exercised only by this module's tests. Removed the moment
// increment 3 wires them in.
#![allow(dead_code, reason = "wired into Hart::step in M6 increment 3")]

use crate::bus::Bus;
use crate::cpu::{Hart, StepError};
use crate::decode::{Instr, funct3, funct7, opcode};

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
    match instr.opcode() {
        opcode::OP_IMM if instr.funct3() == funct3::ADD => Compiled::Continue(Op::AluImm {
            alu: AluOp::Add,
            rd: instr.rd() as u8,
            rs1: instr.rs1() as u8,
            imm: instr.i_imm() as i64,
        }),
        opcode::OP if instr.funct7() != funct7::MULDIV && instr.funct3() == funct3::ADD && !instr.is_alt_op() => {
            Compiled::Continue(Op::AluReg {
                alu: AluOp::Add,
                rd: instr.rd() as u8,
                rs1: instr.rs1() as u8,
                rs2: instr.rs2() as u8,
            })
        }
        opcode::BRANCH if instr.funct3() == funct3::branch::BNE => Compiled::Terminate(Op::Branch {
            cond: Cond::Ne,
            rs1: instr.rs1() as u8,
            rs2: instr.rs2() as u8,
            taken: pc.wrapping_add(instr.b_imm()),
            not_taken: pc.wrapping_add(ilen),
        }),
        _ => Compiled::Unsupported,
    }
}

/// A register-immediate / register-register integer op (`OP-IMM` / `OP`). The
/// interpreter's `op_imm`/`op` semantics, pre-decoded. Grows as discovery needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AluOp {
    Add,
}

/// A branch condition (`funct3` of a `BRANCH`), pre-decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    Ne,
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
    /// Block exit: `pc = cond(x[rs1], x[rs2]) ? taken : not_taken`. Targets are
    /// absolute (resolved at compile time from the branch's PC + immediate).
    Branch { cond: Cond, rs1: u8, rs2: u8, taken: u64, not_taken: u64 },
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

    /// Execute the block against `hart` (and `bus`, for the memory ops later
    /// increments add). Walks the reified ops in order; a `Branch` sets `pc` and
    /// ends the block.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "exec becomes fallible once memory-op (page fault) / unimplemented-op arms land in increment 2+"
    )]
    pub(crate) fn exec(&self, hart: &mut Hart, _bus: &mut Bus) -> Result<(), StepError> {
        for op in &self.ops {
            match *op {
                Op::AluImm { alu, rd, rs1, imm } => {
                    let value = alu.apply(hart.reg(rs1 as usize), imm as u64);
                    hart.set_reg(rd as usize, value);
                }
                Op::AluReg { alu, rd, rs1, rs2 } => {
                    let value = alu.apply(hart.reg(rs1 as usize), hart.reg(rs2 as usize));
                    hart.set_reg(rd as usize, value);
                }
                Op::Branch { cond, rs1, rs2, taken, not_taken } => {
                    let take = cond.eval(hart.reg(rs1 as usize), hart.reg(rs2 as usize));
                    hart.set_pc(if take { taken } else { not_taken });
                    return Ok(());
                }
            }
        }
        hart.set_pc(self.exit_pc); // fell through without a branch
        Ok(())
    }
}

impl AluOp {
    /// Apply the op to two 64-bit operands (`b` is the immediate or `x[rs2]`).
    fn apply(self, a: u64, b: u64) -> u64 {
        match self {
            AluOp::Add => a.wrapping_add(b),
        }
    }
}

impl Cond {
    /// Whether the branch is taken for operands `a = x[rs1]`, `b = x[rs2]`.
    fn eval(self, a: u64, b: u64) -> bool {
        match self {
            Cond::Ne => a != b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AluOp, Block, Compiled, Cond, Op, compile_op};
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
        // addi x1,x0,5 -> a mid-block AluImm; bne -> a terminator; a load -> Unsupported.
        let addi = 0x0050_0093; // addi x1, x0, 5
        assert!(matches!(
            compile_op(addi, 4, 0x1000),
            Compiled::Continue(Op::AluImm { rd: 1, rs1: 0, imm: 5, .. })
        ));
        let bne = 0x0020_9463; // bne x1, x2, +8
        assert!(matches!(
            compile_op(bne, 4, 0x1000),
            Compiled::Terminate(Op::Branch { rs1: 1, rs2: 2, taken, not_taken: 0x1004, .. })
                if taken == 0x1008
        ));
        let lw = 0x0000_2083; // lw x1, 0(x0) — a memory op, a block boundary for now
        assert!(matches!(compile_op(lw, 4, 0x1000), Compiled::Unsupported));
    }
}
