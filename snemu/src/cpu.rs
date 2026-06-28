//! The hart: register file, program counter, instruction-count clock, and
//! the fetch/decode/execute `step`. The single API everything tests through.

use crate::decode::{Instr, funct3, funct7, opcode};
use crate::mem::{BusError, Memory, RAM_BASE};

/// Size in bytes of a (non-compressed) instruction.
const INSTR_SIZE: u64 = 4;

/// Sign-extend a 32-bit result to 64 bits (the `.w` instruction convention).
fn sext32(v: u32) -> u64 {
    i64::from(v as i32) as u64
}

/// Generates RISC-V signed `div`/`rem` for a width: div-by-zero yields all-ones
/// (`-1`), and `MIN / -1` overflows back to the dividend (rem to 0).
macro_rules! signed_div_rem {
    ($div:ident, $rem:ident, $ty:ty) => {
        fn $div(a: $ty, b: $ty) -> $ty {
            if b == 0 {
                -1
            } else if a == <$ty>::MIN && b == -1 {
                a
            } else {
                a.wrapping_div(b)
            }
        }
        fn $rem(a: $ty, b: $ty) -> $ty {
            if b == 0 {
                a
            } else if a == <$ty>::MIN && b == -1 {
                0
            } else {
                a.wrapping_rem(b)
            }
        }
    };
}

/// Generates RISC-V unsigned `div`/`rem`: div-by-zero yields all-ones, rem the
/// dividend.
macro_rules! unsigned_div_rem {
    ($div:ident, $rem:ident, $ty:ty) => {
        fn $div(a: $ty, b: $ty) -> $ty {
            if b == 0 { <$ty>::MAX } else { a / b }
        }
        fn $rem(a: $ty, b: $ty) -> $ty {
            if b == 0 { a } else { a % b }
        }
    };
}

signed_div_rem!(div_s, rem_s, i64);
signed_div_rem!(div_s32, rem_s32, i32);
unsigned_div_rem!(div_u, rem_u, u64);
unsigned_div_rem!(div_u32, rem_u32, u32);

/// Why a `step` could not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepError {
    /// Instruction fetch or a memory access fell outside RAM.
    Bus(BusError),
    /// The decoder doesn't know this instruction yet (the meta-loop signal).
    Unimplemented { pc: u64, instr: u32 },
}

impl From<BusError> for StepError {
    fn from(e: BusError) -> Self {
        StepError::Bus(e)
    }
}

/// A single RISC-V hart over a flat memory.
pub struct Cpu {
    x: [u64; 32],
    pc: u64,
    instret: u64,
    mem: Memory,
}

impl Cpu {
    #[must_use]
    pub fn new(mem: Memory) -> Self {
        Self {
            x: [0; 32],
            pc: RAM_BASE,
            instret: 0,
            mem,
        }
    }

    #[must_use]
    pub fn reg(&self, i: usize) -> u64 {
        self.x[i]
    }

    pub fn set_reg(&mut self, i: usize, value: u64) {
        if i != 0 {
            self.x[i] = value;
        }
    }

    #[must_use]
    pub fn pc(&self) -> u64 {
        self.pc
    }

    pub fn set_pc(&mut self, addr: u64) {
        self.pc = addr;
    }

    #[must_use]
    pub fn instret(&self) -> u64 {
        self.instret
    }

    #[must_use]
    pub fn mem(&self) -> &Memory {
        &self.mem
    }

    pub fn mem_mut(&mut self) -> &mut Memory {
        &mut self.mem
    }

    /// Fetch, decode, and execute one instruction.
    pub fn step(&mut self) -> Result<(), StepError> {
        let instr = self.mem.read_u32(self.pc)?;
        self.execute(instr)?;
        self.instret += 1;
        Ok(())
    }

    fn execute(&mut self, raw: u32) -> Result<(), StepError> {
        let instr = Instr(raw);
        match instr.opcode() {
            opcode::LUI => {
                self.set_reg(instr.rd(), instr.u_imm());
                self.advance();
                Ok(())
            }
            opcode::AUIPC => {
                self.set_reg(instr.rd(), self.pc.wrapping_add(instr.u_imm()));
                self.advance();
                Ok(())
            }
            opcode::OP_IMM => self.op_imm(instr),
            opcode::OP => self.op(instr),
            opcode::OP_IMM_32 => self.op_imm_32(instr),
            opcode::OP_32 => self.op_32(instr),
            opcode::BRANCH => self.branch(instr),
            opcode::JAL => {
                self.jal(instr);
                Ok(())
            }
            opcode::JALR => {
                self.jalr(instr);
                Ok(())
            }
            opcode::LOAD => self.load(instr),
            opcode::STORE => self.store(instr),
            _ => Err(self.unimplemented(raw)),
        }
    }

    /// OP: register-register integer ops (shift amount is `rs2 & 0x3f`).
    fn op(&mut self, instr: Instr) -> Result<(), StepError> {
        if instr.funct7() == funct7::MULDIV {
            return self.op_m(instr);
        }
        let a = self.x[instr.rs1()];
        let b = self.x[instr.rs2()];
        let shamt = (b & 0x3f) as u32;
        let value = match instr.funct3() {
            funct3::ADD if instr.is_alt_op() => a.wrapping_sub(b),           // sub
            funct3::ADD => a.wrapping_add(b),                               // add
            funct3::SLL => a << shamt,                                      // sll
            funct3::SLT => u64::from((a as i64) < (b as i64)),             // slt
            funct3::SLTU => u64::from(a < b),                              // sltu
            funct3::XOR => a ^ b,                                          // xor
            funct3::SR if instr.is_alt_op() => ((a as i64) >> shamt) as u64, // sra
            funct3::SR => a >> shamt,                                      // srl
            funct3::OR => a | b,                                           // or
            funct3::AND => a & b,                                          // and
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// OP-IMM: register-immediate integer ops.
    fn op_imm(&mut self, instr: Instr) -> Result<(), StepError> {
        let a = self.x[instr.rs1()];
        let imm = instr.i_imm();
        let shamt = instr.shamt6();
        let value = match instr.funct3() {
            funct3::ADD => a.wrapping_add(imm),                  // addi
            funct3::SLT => u64::from((a as i64) < (imm as i64)), // slti
            funct3::SLTU => u64::from(a < imm),                  // sltiu
            funct3::XOR => a ^ imm,                              // xori
            funct3::OR => a | imm,                               // ori
            funct3::AND => a & imm,                              // andi
            funct3::SLL => a << shamt,                           // slli
            funct3::SR if instr.is_alt_op() => ((a as i64) >> shamt) as u64, // srai
            funct3::SR => a >> shamt,                            // srli
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// OP-IMM-32: 32-bit register-immediate ops, sign-extended to 64.
    fn op_imm_32(&mut self, instr: Instr) -> Result<(), StepError> {
        let a = self.x[instr.rs1()] as u32;
        let imm = instr.i_imm() as u32;
        let shamt = instr.shamt5();
        let value = match instr.funct3() {
            funct3::ADD => sext32(a.wrapping_add(imm)), // addiw
            funct3::SLL => sext32(a << shamt),          // slliw
            funct3::SR if instr.is_alt_op() => sext32(((a as i32) >> shamt) as u32), // sraiw
            funct3::SR => sext32(a >> shamt),           // srliw
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// OP-32: 32-bit register-register ops, sign-extended to 64.
    fn op_32(&mut self, instr: Instr) -> Result<(), StepError> {
        if instr.funct7() == funct7::MULDIV {
            return self.op_m_32(instr);
        }
        let a = self.x[instr.rs1()] as u32;
        let b = self.x[instr.rs2()] as u32;
        let shamt = b & 0x1f;
        let value = match instr.funct3() {
            funct3::ADD if instr.is_alt_op() => sext32(a.wrapping_sub(b)), // subw
            funct3::ADD => sext32(a.wrapping_add(b)),                      // addw
            funct3::SLL => sext32(a << shamt),                            // sllw
            funct3::SR if instr.is_alt_op() => sext32(((a as i32) >> shamt) as u32), // sraw
            funct3::SR => sext32(a >> shamt),                            // srlw
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// M extension on OP: 64-bit multiply (low / high) and divide / remainder.
    fn op_m(&mut self, instr: Instr) -> Result<(), StepError> {
        let a = self.x[instr.rs1()];
        let b = self.x[instr.rs2()];
        let value = match instr.funct3() {
            funct3::m::MUL => a.wrapping_mul(b),
            funct3::m::MULH => ((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64,
            funct3::m::MULHSU => ((i128::from(a as i64) * i128::from(b)) >> 64) as u64,
            funct3::m::MULHU => ((u128::from(a) * u128::from(b)) >> 64) as u64,
            funct3::m::DIV => div_s(a as i64, b as i64) as u64,
            funct3::m::DIVU => div_u(a, b),
            funct3::m::REM => rem_s(a as i64, b as i64) as u64,
            funct3::m::REMU => rem_u(a, b),
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// M extension on OP-32: 32-bit multiply low and divide / remainder, sign-extended.
    fn op_m_32(&mut self, instr: Instr) -> Result<(), StepError> {
        let a = self.x[instr.rs1()] as u32;
        let b = self.x[instr.rs2()] as u32;
        let value = match instr.funct3() {
            funct3::m::MUL => sext32(a.wrapping_mul(b)),                  // mulw
            funct3::m::DIV => sext32(div_s32(a as i32, b as i32) as u32), // divw
            funct3::m::DIVU => sext32(div_u32(a, b)),                     // divuw
            funct3::m::REM => sext32(rem_s32(a as i32, b as i32) as u32), // remw
            funct3::m::REMU => sext32(rem_u32(a, b)),                     // remuw
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// BRANCH: conditionally add the offset to pc, else advance by 4.
    fn branch(&mut self, instr: Instr) -> Result<(), StepError> {
        let a = self.x[instr.rs1()];
        let b = self.x[instr.rs2()];
        let take = match instr.funct3() {
            funct3::branch::BEQ => a == b,
            funct3::branch::BNE => a != b,
            funct3::branch::BLT => (a as i64) < (b as i64),
            funct3::branch::BGE => (a as i64) >= (b as i64),
            funct3::branch::BLTU => a < b,
            funct3::branch::BGEU => a >= b,
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.pc = if take {
            self.pc.wrapping_add(instr.b_imm())
        } else {
            self.pc.wrapping_add(INSTR_SIZE)
        };
        Ok(())
    }

    /// JAL: link `pc+4` into rd, jump to `pc + offset`.
    fn jal(&mut self, instr: Instr) {
        self.set_reg(instr.rd(), self.pc.wrapping_add(INSTR_SIZE));
        self.pc = self.pc.wrapping_add(instr.j_imm());
    }

    /// JALR: link `pc+4` into rd, jump to `(rs1 + offset) & !1`.
    fn jalr(&mut self, instr: Instr) {
        let target = self.x[instr.rs1()].wrapping_add(instr.i_imm()) & !1;
        self.set_reg(instr.rd(), self.pc.wrapping_add(INSTR_SIZE));
        self.pc = target;
    }

    /// LOAD: read memory at `rs1 + imm`, sign/zero-extend into rd.
    fn load(&mut self, instr: Instr) -> Result<(), StepError> {
        let addr = self.x[instr.rs1()].wrapping_add(instr.i_imm());
        let value = match instr.funct3() {
            funct3::load::LB => i64::from(self.mem.read_u8(addr)? as i8) as u64,
            funct3::load::LH => i64::from(self.mem.read_u16(addr)? as i16) as u64,
            funct3::load::LW => i64::from(self.mem.read_u32(addr)? as i32) as u64,
            funct3::load::LD => self.mem.read_u64(addr)?,
            funct3::load::LBU => u64::from(self.mem.read_u8(addr)?),
            funct3::load::LHU => u64::from(self.mem.read_u16(addr)?),
            funct3::load::LWU => u64::from(self.mem.read_u32(addr)?),
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// STORE: write rs2 (truncated to the access width) to `rs1 + imm`.
    fn store(&mut self, instr: Instr) -> Result<(), StepError> {
        let addr = self.x[instr.rs1()].wrapping_add(instr.s_imm());
        let value = self.x[instr.rs2()];
        match instr.funct3() {
            funct3::store::SB => self.mem.write_u8(addr, value as u8)?,
            funct3::store::SH => self.mem.write_u16(addr, value as u16)?,
            funct3::store::SW => self.mem.write_u32(addr, value as u32)?,
            funct3::store::SD => self.mem.write_u64(addr, value)?,
            _ => return Err(self.unimplemented(instr.0)),
        }
        self.advance();
        Ok(())
    }

    /// Move the program counter to the next sequential instruction.
    fn advance(&mut self) {
        self.pc = self.pc.wrapping_add(INSTR_SIZE);
    }

    fn unimplemented(&self, instr: u32) -> StepError {
        StepError::Unimplemented {
            pc: self.pc,
            instr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{ALT_OP_BIT, funct3, funct7, opcode};
    use crate::mem::{Memory, RAM_BASE};

    /// Run a single R-type op `enc(rd=3, rs1=1, rs2=2)` with x1=a, x2=b
    /// (operands set directly via the public API), and return x3.
    fn run_rrr(enc: fn(u32, u32, u32) -> u32, a: u64, b: u64) -> u64 {
        let mut mem = Memory::new(0x1000);
        mem.write_u32(RAM_BASE, enc(3, 1, 2)).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(1, a);
        cpu.set_reg(2, b);
        cpu.step().unwrap();
        cpu.reg(3)
    }

    fn m_op(opcode: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode, funct3, funct7::MULDIV << 25, rd, rs1, rs2)
    }
    fn mul(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::MUL, rd, rs1, rs2)
    }
    fn mulh(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::MULH, rd, rs1, rs2)
    }
    fn mulhsu(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::MULHSU, rd, rs1, rs2)
    }
    fn mulhu(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::MULHU, rd, rs1, rs2)
    }
    fn div(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::DIV, rd, rs1, rs2)
    }
    fn divu(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::DIVU, rd, rs1, rs2)
    }
    fn rem(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::REM, rd, rs1, rs2)
    }
    fn remu(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP, funct3::m::REMU, rd, rs1, rs2)
    }
    fn mulw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP_32, funct3::m::MUL, rd, rs1, rs2)
    }
    fn divw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP_32, funct3::m::DIV, rd, rs1, rs2)
    }
    fn divuw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP_32, funct3::m::DIVU, rd, rs1, rs2)
    }
    fn remw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP_32, funct3::m::REM, rd, rs1, rs2)
    }
    fn remuw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        m_op(opcode::OP_32, funct3::m::REMU, rd, rs1, rs2)
    }

    fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::ADD, rd, rs1, imm)
    }

    /// Encode a U-type instruction (`imm20` lands in bits 31:12).
    fn u_type(opcode: u32, rd: u32, imm20: u32) -> u32 {
        ((imm20 & 0xf_ffff) << 12) | (rd << 7) | opcode
    }

    fn lui(rd: u32, imm20: u32) -> u32 {
        u_type(opcode::LUI, rd, imm20)
    }

    fn auipc(rd: u32, imm20: u32) -> u32 {
        u_type(opcode::AUIPC, rd, imm20)
    }

    /// Encode an I-type instruction.
    fn i_type(opcode: u32, funct3: u32, rd: u32, rs1: u32, imm: i32) -> u32 {
        let imm = (imm as u32) & 0xfff;
        (imm << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
    }

    fn slti(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::SLT, rd, rs1, imm)
    }
    fn sltiu(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::SLTU, rd, rs1, imm)
    }
    fn xori(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::XOR, rd, rs1, imm)
    }
    fn ori(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::OR, rd, rs1, imm)
    }
    fn andi(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM, funct3::AND, rd, rs1, imm)
    }

    fn shift_imm(opcode: u32, funct3: u32, alt: u32, rd: u32, rs1: u32, shamt: u32) -> u32 {
        alt | (shamt << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
    }
    fn slli(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM, funct3::SLL, 0, rd, rs1, shamt)
    }
    fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM, funct3::SR, 0, rd, rs1, shamt)
    }
    fn srai(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM, funct3::SR, ALT_OP_BIT, rd, rs1, shamt)
    }
    fn addiw(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::OP_IMM_32, funct3::ADD, rd, rs1, imm)
    }
    fn slliw(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM_32, funct3::SLL, 0, rd, rs1, shamt)
    }
    fn srliw(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM_32, funct3::SR, 0, rd, rs1, shamt)
    }
    fn sraiw(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(opcode::OP_IMM_32, funct3::SR, ALT_OP_BIT, rd, rs1, shamt)
    }

    /// Encode an R-type instruction (`alt` is 0 or `ALT_OP_BIT`).
    fn r_type(opcode: u32, funct3: u32, alt: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
        alt | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
    }
    fn add(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::ADD, 0, rd, rs1, rs2)
    }
    fn sub(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::ADD, ALT_OP_BIT, rd, rs1, rs2)
    }
    fn sll(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::SLL, 0, rd, rs1, rs2)
    }
    fn slt(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::SLT, 0, rd, rs1, rs2)
    }
    fn sltu(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::SLTU, 0, rd, rs1, rs2)
    }
    fn xor(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::XOR, 0, rd, rs1, rs2)
    }
    fn srl(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::SR, 0, rd, rs1, rs2)
    }
    fn sra(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::SR, ALT_OP_BIT, rd, rs1, rs2)
    }
    fn or(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::OR, 0, rd, rs1, rs2)
    }
    fn and(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP, funct3::AND, 0, rd, rs1, rs2)
    }
    fn addw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP_32, funct3::ADD, 0, rd, rs1, rs2)
    }
    fn subw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP_32, funct3::ADD, ALT_OP_BIT, rd, rs1, rs2)
    }
    fn sllw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP_32, funct3::SLL, 0, rd, rs1, rs2)
    }
    fn srlw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP_32, funct3::SR, 0, rd, rs1, rs2)
    }
    fn sraw(rd: u32, rs1: u32, rs2: u32) -> u32 {
        r_type(opcode::OP_32, funct3::SR, ALT_OP_BIT, rd, rs1, rs2)
    }

    /// Encode a B-type branch (`imm` is a byte offset, bit 0 ignored).
    fn b_type(funct3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
        let imm = imm as u32;
        ((imm >> 12) & 1) << 31
            | ((imm >> 5) & 0x3f) << 25
            | (rs2 << 20)
            | (rs1 << 15)
            | (funct3 << 12)
            | ((imm >> 1) & 0xf) << 8
            | ((imm >> 11) & 1) << 7
            | opcode::BRANCH
    }
    fn beq(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BEQ, rs1, rs2, imm)
    }
    fn bne(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BNE, rs1, rs2, imm)
    }
    fn blt(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BLT, rs1, rs2, imm)
    }
    fn bge(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BGE, rs1, rs2, imm)
    }
    fn bltu(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BLTU, rs1, rs2, imm)
    }
    fn bgeu(rs1: u32, rs2: u32, imm: i32) -> u32 {
        b_type(funct3::branch::BGEU, rs1, rs2, imm)
    }

    /// Encode a J-type `jal rd, imm` (`imm` is a byte offset, bit 0 ignored).
    fn jal(rd: u32, imm: i32) -> u32 {
        let imm = imm as u32;
        ((imm >> 20) & 1) << 31
            | ((imm >> 1) & 0x3ff) << 21
            | ((imm >> 11) & 1) << 20
            | ((imm >> 12) & 0xff) << 12
            | (rd << 7)
            | opcode::JAL
    }
    fn jalr(rd: u32, rs1: u32, imm: i32) -> u32 {
        i_type(opcode::JALR, 0, rd, rs1, imm)
    }

    fn lb(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LB, rd, base, imm)
    }
    fn lh(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LH, rd, base, imm)
    }
    fn lw(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LW, rd, base, imm)
    }
    fn ld(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LD, rd, base, imm)
    }
    fn lbu(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LBU, rd, base, imm)
    }
    fn lhu(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LHU, rd, base, imm)
    }
    fn lwu(rd: u32, base: u32, imm: i32) -> u32 {
        i_type(opcode::LOAD, funct3::load::LWU, rd, base, imm)
    }

    /// Encode an S-type store (`src` is rs2, `base` is rs1).
    fn s_type(funct3: u32, base: u32, src: u32, imm: i32) -> u32 {
        let imm = imm as u32;
        ((imm >> 5) & 0x7f) << 25
            | (src << 20)
            | (base << 15)
            | (funct3 << 12)
            | (imm & 0x1f) << 7
            | opcode::STORE
    }
    fn sb(src: u32, base: u32, imm: i32) -> u32 {
        s_type(funct3::store::SB, base, src, imm)
    }
    fn sh(src: u32, base: u32, imm: i32) -> u32 {
        s_type(funct3::store::SH, base, src, imm)
    }
    fn sw(src: u32, base: u32, imm: i32) -> u32 {
        s_type(funct3::store::SW, base, src, imm)
    }
    fn sd(src: u32, base: u32, imm: i32) -> u32 {
        s_type(funct3::store::SD, base, src, imm)
    }

    /// A `Cpu` with `program` loaded at the RAM base and pc pointing at it.
    fn cpu_with(program: &[u32]) -> Cpu {
        let mut mem = Memory::new(0x1000);
        for (i, &word) in program.iter().enumerate() {
            mem.write_u32(RAM_BASE + (i as u64) * 4, word).unwrap();
        }
        Cpu::new(mem)
    }

    #[test]
    fn addi_sets_register_and_advances() {
        let mut cpu = cpu_with(&[addi(1, 0, 42)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 42);
        assert_eq!(cpu.pc(), RAM_BASE + 4);
        assert_eq!(cpu.instret(), 1);
    }

    #[test]
    fn addi_sign_extends_the_immediate() {
        let mut cpu = cpu_with(&[addi(1, 0, -1)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), u64::MAX);
    }

    #[test]
    fn x0_is_hard_wired_zero() {
        let mut cpu = cpu_with(&[addi(0, 0, 42)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(0), 0);
    }

    #[test]
    fn lui_loads_and_sign_extends_the_upper_immediate() {
        let mut cpu = cpu_with(&[lui(1, 0x12345), lui(2, 0xfffff)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 0x1234_5000);
        assert_eq!(cpu.pc(), RAM_BASE + 4);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 0xffff_ffff_ffff_f000);
    }

    #[test]
    fn auipc_adds_the_immediate_to_the_physical_pc() {
        let mut cpu = cpu_with(&[auipc(1, 0x1)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), RAM_BASE + 0x1000);
    }

    #[test]
    fn op_imm_compare_logic_and_shift_family() {
        let program = &[
            addi(1, 0, 12),    // x1  = 12
            slti(2, 1, 20),    // x2  = (12 <s 20)  = 1
            slti(3, 1, 5),     // x3  = (12 <s 5)   = 0
            sltiu(4, 1, -1),   // x4  = (12 <u MAX) = 1
            xori(5, 1, 0xff),  // x5  = 12 ^ 0xff   = 243
            ori(6, 1, 1),      // x6  = 12 | 1      = 13
            andi(7, 1, 6),     // x7  = 12 & 6      = 4
            slli(8, 1, 4),     // x8  = 12 << 4     = 192
            srli(9, 1, 2),     // x9  = 12 >> 2     = 3
            addi(10, 0, -16),  // x10 = -16
            srai(11, 10, 2),   // x11 = -16 >>a 2   = -4
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(2), 1);
        assert_eq!(cpu.reg(3), 0);
        assert_eq!(cpu.reg(4), 1);
        assert_eq!(cpu.reg(5), 243);
        assert_eq!(cpu.reg(6), 13);
        assert_eq!(cpu.reg(7), 4);
        assert_eq!(cpu.reg(8), 192);
        assert_eq!(cpu.reg(9), 3);
        assert_eq!(cpu.reg(11), (-4_i64) as u64);
    }

    #[test]
    fn op_register_register_family() {
        let program = &[
            addi(1, 0, 12),   // x1  = 12
            addi(2, 0, 5),    // x2  = 5
            addi(12, 0, 2),   // x12 = 2  (shift amount source)
            addi(13, 0, -16), // x13 = -16
            add(3, 1, 2),     // 17
            sub(4, 1, 2),     // 7
            sll(5, 1, 12),    // 12 << 2 = 48
            slt(6, 2, 1),     // (5 <s 12) = 1
            sltu(7, 1, 2),    // (12 <u 5) = 0
            xor(8, 1, 2),     // 12 ^ 5 = 9
            or(9, 1, 2),      // 12 | 5 = 13
            and(10, 1, 2),    // 12 & 5 = 4
            srl(11, 1, 12),   // 12 >> 2 = 3
            sra(14, 13, 12),  // -16 >>a 2 = -4
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(3), 17);
        assert_eq!(cpu.reg(4), 7);
        assert_eq!(cpu.reg(5), 48);
        assert_eq!(cpu.reg(6), 1);
        assert_eq!(cpu.reg(7), 0);
        assert_eq!(cpu.reg(8), 9);
        assert_eq!(cpu.reg(9), 13);
        assert_eq!(cpu.reg(10), 4);
        assert_eq!(cpu.reg(11), 3);
        assert_eq!(cpu.reg(14), (-4_i64) as u64);
    }

    #[test]
    fn word_ops_truncate_to_32_bits_and_sign_extend() {
        let program = &[
            addi(1, 0, 1),    // x1 = 1
            slli(2, 1, 31),   // x2 = 0x8000_0000
            slli(3, 1, 32),   // x3 = 0x1_0000_0000 (high bit beyond 32)
            addi(7, 0, 31),   // x7 = 31 (shift source)
            addiw(4, 3, 7),   // (0x1_0000_0000 + 7) low32 = 7
            slliw(13, 1, 31), // (1 << 31) sign-extended
            srliw(9, 2, 4),   // 0x8000_0000 >>l 4 = 0x0800_0000
            sraiw(8, 2, 4),   // 0x8000_0000 >>a 4, sign-extended
            addw(5, 2, 2),    // (0x8000_0000 + 0x8000_0000) low32 = 0
            subw(10, 3, 1),   // (0 - 1) low32 = -1, sign-extended
            sllw(6, 1, 7),    // (1 << 31) sign-extended
            srlw(11, 2, 7),   // 0x8000_0000 >>l 31 = 1
            sraw(12, 2, 7),   // 0x8000_0000 >>a 31 = -1, sign-extended
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(4), 7);
        assert_eq!(cpu.reg(13), 0xffff_ffff_8000_0000);
        assert_eq!(cpu.reg(9), 0x0800_0000);
        assert_eq!(cpu.reg(8), 0xffff_ffff_f800_0000);
        assert_eq!(cpu.reg(5), 0);
        assert_eq!(cpu.reg(10), u64::MAX);
        assert_eq!(cpu.reg(6), 0xffff_ffff_8000_0000);
        assert_eq!(cpu.reg(11), 1);
        assert_eq!(cpu.reg(12), u64::MAX);
    }

    /// Run `branch x1, x2, +8` with x1=a, x2=b; return whether it was taken.
    /// Layout: the branch skips a "not-taken marker" that sets x10=1.
    fn branch_taken(branch: fn(u32, u32, i32) -> u32, a: i32, b: i32) -> bool {
        let program = &[
            addi(1, 0, a),     // 0
            addi(2, 0, b),     // 4
            branch(1, 2, 8),   // 8:  taken -> 16, else -> 12
            addi(10, 0, 1),    // 12: not-taken marker
            addi(0, 0, 0),     // 16: nop (taken landing)
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..4 {
            cpu.step().unwrap();
        }
        cpu.reg(10) == 0
    }

    #[test]
    fn branches_take_the_right_path() {
        assert!(branch_taken(beq, 7, 7));
        assert!(!branch_taken(beq, 7, 8));
        assert!(branch_taken(bne, 7, 8));
        assert!(!branch_taken(bne, 7, 7));
        // signed: -1 < 1
        assert!(branch_taken(blt, -1, 1));
        assert!(!branch_taken(blt, 1, -1));
        assert!(branch_taken(bge, 1, 1));
        assert!(!branch_taken(bge, -1, 1));
        // unsigned: -1 is 0xffff...ff, so NOT < 1
        assert!(!branch_taken(bltu, -1, 1));
        assert!(branch_taken(bltu, 1, 2));
        assert!(branch_taken(bgeu, -1, 1));
        assert!(!branch_taken(bgeu, 1, 2));
    }

    #[test]
    fn jal_links_return_address_and_jumps() {
        let program = &[
            jal(1, 8),     // 0: x1 = pc+4; pc -> 8
            addi(2, 0, 1), // 4: skipped
            addi(3, 0, 1), // 8: executed
        ];
        let mut cpu = cpu_with(program);
        cpu.step().unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), RAM_BASE + 4);
        assert_eq!(cpu.reg(2), 0);
        assert_eq!(cpu.reg(3), 1);
    }

    #[test]
    fn jalr_links_and_jumps_to_register_plus_offset() {
        let program = &[
            auipc(5, 0),    // 0:  x5 = RAM_BASE
            jalr(1, 5, 16), // 4:  x1 = pc+4; pc -> RAM_BASE + 16
            addi(2, 0, 1),  // 8:  skipped
            addi(6, 0, 1),  // 12: skipped
            addi(3, 0, 1),  // 16: executed
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..3 {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(5), RAM_BASE);
        assert_eq!(cpu.reg(1), RAM_BASE + 8);
        assert_eq!(cpu.reg(2), 0);
        assert_eq!(cpu.reg(6), 0);
        assert_eq!(cpu.reg(3), 1);
    }

    #[test]
    fn stores_and_loads_round_trip_with_correct_extension() {
        let program = &[
            auipc(2, 0),       // 0:  x2 = RAM_BASE
            addi(2, 2, 0x200), // 4:  x2 = scratch area (RAM_BASE + 0x200)
            addi(1, 0, -1),    // 8:  x1 = 0xffff_ffff_ffff_ffff
            addi(6, 0, 1),     // 12: x6 = 1
            slli(6, 6, 31),    // 16: x6 = 0x8000_0000
            sd(1, 2, 0),       // 20: [x2+0]  = all ones (8 bytes)
            sb(1, 2, 8),       // 24: [x2+8]  = 0xff
            sw(6, 2, 16),      // 28: [x2+16] = 0x8000_0000
            sh(1, 2, 32),      // 32: [x2+32] = 0xffff
            ld(3, 2, 0),       // 36: x3  = u64::MAX
            lb(4, 2, 8),       // 40: x4  = sign(0xff)  = -1
            lbu(5, 2, 8),      // 44: x5  = 255
            lw(7, 2, 16),      // 48: x7  = sign(0x8000_0000)
            lwu(8, 2, 16),     // 52: x8  = 0x8000_0000
            lh(11, 2, 32),     // 56: x11 = sign(0xffff) = -1
            lhu(12, 2, 32),    // 60: x12 = 65535
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(3), u64::MAX);
        assert_eq!(cpu.reg(4), u64::MAX);
        assert_eq!(cpu.reg(5), 255);
        assert_eq!(cpu.reg(7), 0xffff_ffff_8000_0000);
        assert_eq!(cpu.reg(8), 0x8000_0000);
        assert_eq!(cpu.reg(11), u64::MAX);
        assert_eq!(cpu.reg(12), 65535);
    }

    #[test]
    fn m_extension_multiply() {
        assert_eq!(run_rrr(mul, 6, 7), 42);
        assert_eq!(run_rrr(mulh, 1 << 62, 4), 1);
        assert_eq!(run_rrr(mulhu, 1 << 62, 4), 1);
        // (-1) signed * 2 unsigned -> high word all ones
        assert_eq!(run_rrr(mulhsu, u64::MAX, 2), u64::MAX);
        // low 32 of (0x10000 * 0x8000) = 0x8000_0000, sign-extended
        assert_eq!(run_rrr(mulw, 0x10000, 0x8000), 0xffff_ffff_8000_0000);
    }

    #[test]
    fn m_extension_divide_and_remainder_with_edge_cases() {
        assert_eq!(run_rrr(div, 20, 6), 3);
        assert_eq!(run_rrr(div, (-20_i64) as u64, 6), (-3_i64) as u64);
        assert_eq!(run_rrr(div, 5, 0), u64::MAX); // div by zero -> -1
        assert_eq!(run_rrr(div, 1 << 63, (-1_i64) as u64), 1 << 63); // MIN / -1 -> MIN
        assert_eq!(run_rrr(divu, 20, 6), 3);
        assert_eq!(run_rrr(divu, 5, 0), u64::MAX); // div by zero -> all ones
        assert_eq!(run_rrr(rem, 20, 6), 2);
        assert_eq!(run_rrr(rem, (-20_i64) as u64, 6), (-2_i64) as u64);
        assert_eq!(run_rrr(rem, 5, 0), 5); // rem by zero -> dividend
        assert_eq!(run_rrr(rem, 1 << 63, (-1_i64) as u64), 0); // MIN % -1 -> 0
        assert_eq!(run_rrr(remu, 20, 6), 2);
        assert_eq!(run_rrr(remu, 5, 0), 5); // rem by zero -> dividend
    }

    #[test]
    fn m_extension_word_divide_and_remainder() {
        assert_eq!(run_rrr(divw, (-20_i64) as u64, 6), (-3_i64) as u64);
        assert_eq!(run_rrr(divw, 5, 0), u64::MAX); // -1 sign-extended
        // 32-bit MIN / -1 -> 32-bit MIN, sign-extended
        assert_eq!(run_rrr(divw, 1 << 31, (-1_i64) as u64), 0xffff_ffff_8000_0000);
        assert_eq!(run_rrr(divuw, 20, 6), 3);
        assert_eq!(run_rrr(divuw, 5, 0), u64::MAX); // 0xffff_ffff sign-extended
        assert_eq!(run_rrr(remw, (-20_i64) as u64, 6), (-2_i64) as u64);
        assert_eq!(run_rrr(remuw, 20, 6), 2);
    }

    #[test]
    fn unknown_instruction_reports_unimplemented() {
        let mut cpu = cpu_with(&[0xffff_ffff]);
        assert_eq!(
            cpu.step(),
            Err(StepError::Unimplemented {
                pc: RAM_BASE,
                instr: 0xffff_ffff,
            })
        );
    }
}
