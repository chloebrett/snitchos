//! The hart: register file, program counter, instruction-count clock, and
//! the fetch/decode/execute `step`. The single API everything tests through.

use crate::mem::{BusError, Memory, RAM_BASE};

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

    fn execute(&mut self, instr: u32) -> Result<(), StepError> {
        let opcode = instr & 0x7f;
        let funct3 = (instr >> 12) & 0x7;
        match (opcode, funct3) {
            (0x13, 0x0) => {
                // addi rd, rs1, imm
                let rd = ((instr >> 7) & 0x1f) as usize;
                let rs1 = ((instr >> 15) & 0x1f) as usize;
                let imm = i64::from(instr as i32 >> 20) as u64;
                self.set_reg(rd, self.x[rs1].wrapping_add(imm));
                self.pc = self.pc.wrapping_add(4);
                Ok(())
            }
            _ => Err(StepError::Unimplemented {
                pc: self.pc,
                instr,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{Memory, RAM_BASE};

    /// Encode an I-type `addi rd, rs1, imm`.
    fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
        let imm = (imm as u32) & 0xfff;
        (imm << 20) | (rs1 << 15) | (rd << 7) | 0x13
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
