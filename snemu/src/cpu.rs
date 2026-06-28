//! The hart: register file, program counter, instruction-count clock, and
//! the fetch/decode/execute `step`. The single API everything tests through.

use crate::mem::{BusError, Memory, RAM_BASE};

/// RISC-V base opcode field, `instr[6:0]`. Extended as families come online.
mod opcode {
    pub const LUI: u32 = 0x37;
    pub const AUIPC: u32 = 0x17;
    pub const OP_IMM: u32 = 0x13;
}

/// funct3 ALU-op selectors, `instr[14:12]` — shared by OP and OP-IMM.
mod funct3 {
    pub const ADD: u32 = 0x0;
    pub const SLL: u32 = 0x1;
    pub const SLT: u32 = 0x2;
    pub const SLTU: u32 = 0x3;
    pub const XOR: u32 = 0x4;
    pub const SR: u32 = 0x5;
    pub const OR: u32 = 0x6;
    pub const AND: u32 = 0x7;
}

/// funct7 bit 5 (`instr[30]`): selects sub-vs-add and arithmetic-vs-logical shift.
const ALT_OP_BIT: u32 = 0x4000_0000;

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

/// A decoded instruction word: thin field accessors over the raw bits.
/// Accessors are added as instruction families come online.
#[derive(Clone, Copy)]
struct Instr(u32);

impl Instr {
    fn opcode(self) -> u32 {
        self.0 & 0x7f
    }

    fn rd(self) -> usize {
        ((self.0 >> 7) & 0x1f) as usize
    }

    fn rs1(self) -> usize {
        ((self.0 >> 15) & 0x1f) as usize
    }

    fn funct3(self) -> u32 {
        (self.0 >> 12) & 0x7
    }

    /// Sign-extended I-type immediate (bits 31:20).
    fn i_imm(self) -> u64 {
        i64::from(self.0 as i32 >> 20) as u64
    }

    /// Sign-extended U-type immediate (bits 31:12, low 12 zero).
    fn u_imm(self) -> u64 {
        i64::from((self.0 & 0xffff_f000) as i32) as u64
    }

    /// 6-bit shift amount for RV64 immediate shifts (bits 25:20).
    fn shamt6(self) -> u32 {
        (self.0 >> 20) & 0x3f
    }

    fn is_alt_op(self) -> bool {
        self.0 & ALT_OP_BIT != 0
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
                self.pc = self.pc.wrapping_add(4);
                Ok(())
            }
            opcode::AUIPC => {
                self.set_reg(instr.rd(), self.pc.wrapping_add(instr.u_imm()));
                self.pc = self.pc.wrapping_add(4);
                Ok(())
            }
            opcode::OP_IMM => self.op_imm(instr),
            _ => Err(self.unimplemented(raw)),
        }
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
        self.pc = self.pc.wrapping_add(4);
        Ok(())
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
    use crate::mem::{Memory, RAM_BASE};

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

    fn shift_imm(funct3: u32, alt: u32, rd: u32, rs1: u32, shamt: u32) -> u32 {
        alt | (shamt << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode::OP_IMM
    }
    fn slli(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(funct3::SLL, 0, rd, rs1, shamt)
    }
    fn srli(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(funct3::SR, 0, rd, rs1, shamt)
    }
    fn srai(rd: u32, rs1: u32, shamt: u32) -> u32 {
        shift_imm(funct3::SR, ALT_OP_BIT, rd, rs1, shamt)
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
