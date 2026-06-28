//! Instruction decoding: opcode / funct constants and field extraction over a
//! raw 32-bit instruction word. Pure — no reference to CPU state — so the
//! compressed-instruction expander can reuse it.

/// RISC-V base opcode field, `instr[6:0]`. Extended as families come online.
pub(crate) mod opcode {
    pub const LUI: u32 = 0x37;
    pub const AUIPC: u32 = 0x17;
    pub const OP_IMM: u32 = 0x13;
    pub const OP: u32 = 0x33;
    pub const OP_IMM_32: u32 = 0x1b;
    pub const OP_32: u32 = 0x3b;
    pub const BRANCH: u32 = 0x63;
    pub const JAL: u32 = 0x6f;
    pub const JALR: u32 = 0x67;
    pub const LOAD: u32 = 0x03;
    pub const STORE: u32 = 0x23;
}

/// funct3 ALU-op selectors, `instr[14:12]` — shared by OP and OP-IMM.
pub(crate) mod funct3 {
    pub const ADD: u32 = 0x0;
    pub const SLL: u32 = 0x1;
    pub const SLT: u32 = 0x2;
    pub const SLTU: u32 = 0x3;
    pub const XOR: u32 = 0x4;
    pub const SR: u32 = 0x5;
    pub const OR: u32 = 0x6;
    pub const AND: u32 = 0x7;

    /// funct3 condition selectors for the BRANCH opcode.
    pub mod branch {
        pub const BEQ: u32 = 0x0;
        pub const BNE: u32 = 0x1;
        pub const BLT: u32 = 0x4;
        pub const BGE: u32 = 0x5;
        pub const BLTU: u32 = 0x6;
        pub const BGEU: u32 = 0x7;
    }

    /// funct3 width selectors for the LOAD opcode.
    pub mod load {
        pub const LB: u32 = 0x0;
        pub const LH: u32 = 0x1;
        pub const LW: u32 = 0x2;
        pub const LD: u32 = 0x3;
        pub const LBU: u32 = 0x4;
        pub const LHU: u32 = 0x5;
        pub const LWU: u32 = 0x6;
    }

    /// funct3 width selectors for the STORE opcode.
    pub mod store {
        pub const SB: u32 = 0x0;
        pub const SH: u32 = 0x1;
        pub const SW: u32 = 0x2;
        pub const SD: u32 = 0x3;
    }
}

/// funct7 bit 5 (`instr[30]`): selects sub-vs-add and arithmetic-vs-logical shift.
pub(crate) const ALT_OP_BIT: u32 = 0x4000_0000;

/// Sign-extend the low `bits` of `value` to 64 bits.
fn sign_extend(value: u32, bits: u32) -> u64 {
    let shift = 32 - bits;
    i64::from((value << shift) as i32 >> shift) as u64
}

/// A decoded instruction word: thin field accessors over the raw bits.
/// Accessors are added as instruction families come online.
#[derive(Clone, Copy)]
pub(crate) struct Instr(pub(crate) u32);

impl Instr {
    pub(crate) fn opcode(self) -> u32 {
        self.0 & 0x7f
    }

    pub(crate) fn rd(self) -> usize {
        ((self.0 >> 7) & 0x1f) as usize
    }

    pub(crate) fn rs1(self) -> usize {
        ((self.0 >> 15) & 0x1f) as usize
    }

    pub(crate) fn rs2(self) -> usize {
        ((self.0 >> 20) & 0x1f) as usize
    }

    pub(crate) fn funct3(self) -> u32 {
        (self.0 >> 12) & 0x7
    }

    /// Sign-extended I-type immediate (bits 31:20).
    pub(crate) fn i_imm(self) -> u64 {
        i64::from(self.0 as i32 >> 20) as u64
    }

    /// Sign-extended U-type immediate (bits 31:12, low 12 zero).
    pub(crate) fn u_imm(self) -> u64 {
        i64::from((self.0 & 0xffff_f000) as i32) as u64
    }

    /// 6-bit shift amount for RV64 immediate shifts (bits 25:20).
    pub(crate) fn shamt6(self) -> u32 {
        (self.0 >> 20) & 0x3f
    }

    /// 5-bit shift amount for RV64 `.w` immediate shifts (bits 24:20).
    pub(crate) fn shamt5(self) -> u32 {
        (self.0 >> 20) & 0x1f
    }

    /// Sign-extended B-type branch offset (bit 0 always 0).
    pub(crate) fn b_imm(self) -> u64 {
        let i = self.0;
        let imm = ((i >> 31) & 1) << 12
            | ((i >> 7) & 1) << 11
            | ((i >> 25) & 0x3f) << 5
            | ((i >> 8) & 0xf) << 1;
        sign_extend(imm, 13)
    }

    /// Sign-extended J-type jump offset (bit 0 always 0).
    pub(crate) fn j_imm(self) -> u64 {
        let i = self.0;
        let imm = ((i >> 31) & 1) << 20
            | ((i >> 12) & 0xff) << 12
            | ((i >> 20) & 1) << 11
            | ((i >> 21) & 0x3ff) << 1;
        sign_extend(imm, 21)
    }

    /// Sign-extended S-type store offset.
    pub(crate) fn s_imm(self) -> u64 {
        let i = self.0;
        let imm = ((i >> 25) & 0x7f) << 5 | ((i >> 7) & 0x1f);
        sign_extend(imm, 12)
    }

    pub(crate) fn is_alt_op(self) -> bool {
        self.0 & ALT_OP_BIT != 0
    }
}
