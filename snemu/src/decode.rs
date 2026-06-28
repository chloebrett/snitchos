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
    pub const SYSTEM: u32 = 0x73;
    pub const MISC_MEM: u32 = 0x0f;
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

    /// funct3 selectors for the M extension (OP / OP-32 with funct7 = MULDIV).
    pub mod m {
        pub const MUL: u32 = 0x0;
        pub const MULH: u32 = 0x1;
        pub const MULHSU: u32 = 0x2;
        pub const MULHU: u32 = 0x3;
        pub const DIV: u32 = 0x4;
        pub const DIVU: u32 = 0x5;
        pub const REM: u32 = 0x6;
        pub const REMU: u32 = 0x7;
    }
}

/// funct7 selectors, `instr[31:25]`.
pub(crate) mod funct7 {
    /// Marks an OP / OP-32 instruction as belonging to the M extension.
    pub const MULDIV: u32 = 0x01;
}

/// funct3 selectors for the SYSTEM opcode.
pub(crate) mod system {
    pub const PRIV: u32 = 0x0; // ecall / ebreak / sret / wfi (by funct12)
    pub const CSRRW: u32 = 0x1;
    pub const CSRRS: u32 = 0x2;
    pub const CSRRC: u32 = 0x3;
    pub const CSRRWI: u32 = 0x5;
    pub const CSRRSI: u32 = 0x6;
    pub const CSRRCI: u32 = 0x7;
}

/// funct12 codes for SYSTEM privileged instructions (`instr[31:20]`, funct3=0).
pub(crate) mod priv12 {
    pub const ECALL: u32 = 0x000;
    pub const EBREAK: u32 = 0x001;
    pub const SRET: u32 = 0x102;
    pub const WFI: u32 = 0x105;
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

    pub(crate) fn funct7(self) -> u32 {
        (self.0 >> 25) & 0x7f
    }

    /// CSR address for `csr*` instructions (bits 31:20).
    pub(crate) fn csr(self) -> u16 {
        ((self.0 >> 20) & 0xfff) as u16
    }

    /// funct12 field for SYSTEM privileged instructions (bits 31:20).
    pub(crate) fn funct12(self) -> u32 {
        (self.0 >> 20) & 0xfff
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

/// A 16-bit half-word is a compressed instruction unless its low two bits are 11.
pub(crate) fn is_compressed(half: u16) -> bool {
    half & 0b11 != 0b11
}

/// Expand a 16-bit compressed instruction to its canonical 32-bit form, or
/// `None` if it isn't a compressed instruction snemu models yet (the meta-loop).
pub(crate) fn expand(half: u16) -> Option<u32> {
    let quadrant = half & 0b11;
    let funct3 = (half >> 13) & 0b111;
    match (quadrant, funct3) {
        (0b00, 0b000) => Some(expand_c_addi4spn(half)),
        (0b00, 0b011) => Some(expand_c_ld(half)),
        (0b00, 0b111) => Some(expand_c_sd(half)),
        (0b01, 0b000) => Some(expand_c_addi(half)),
        (0b01, 0b010) => Some(expand_c_li(half)),
        (0b01, 0b011) => expand_c_lui_addi16sp(half),
        (0b01, 0b100) => expand_c_misc_alu(half),
        (0b01, 0b101) => Some(expand_c_j(half)),
        (0b01, 0b110) => Some(expand_c_beqz(half)),
        (0b01, 0b111) => Some(expand_c_bnez(half)),
        (0b10, 0b010) => Some(expand_c_lwsp(half)),
        (0b10, 0b011) => Some(expand_c_ldsp(half)),
        (0b10, 0b100) => Some(expand_cr(half)),
        (0b10, 0b110) => Some(expand_c_swsp(half)),
        (0b10, 0b111) => Some(expand_c_sdsp(half)),
        _ => None,
    }
}

/// `c.addi rd, nzimm` -> `addi rd, rd, nzimm`.
fn expand_c_addi(half: u16) -> u32 {
    let rd = u32::from((half >> 7) & 0x1f);
    (ci_imm6(half) << 20) | (rd << 15) | (rd << 7) | opcode::OP_IMM
}

/// 12-bit I-type immediate field from a CI-format 6-bit immediate
/// (`imm[5]` = bit 12, `imm[4:0]` = bits 6:2), sign-extended.
fn ci_imm6(half: u16) -> u32 {
    let raw = (u32::from((half >> 12) & 1) << 5) | u32::from((half >> 2) & 0x1f);
    (sign_extend(raw, 6) as u32) & 0xfff
}

/// `c.li rd, imm` -> `addi rd, x0, imm`.
fn expand_c_li(half: u16) -> u32 {
    let rd = u32::from((half >> 7) & 0x1f);
    (ci_imm6(half) << 20) | (rd << 7) | opcode::OP_IMM // rs1 = x0
}

/// The CR cluster (quadrant 10, funct3 100): `c.mv` / `c.add` / `c.jr` /
/// `c.jalr` / `c.ebreak`, selected by bit 12 and whether rs2 is zero.
fn expand_cr(half: u16) -> u32 {
    let bit12 = (half >> 12) & 1;
    let rd = u32::from((half >> 7) & 0x1f); // also rs1
    let rs2 = u32::from((half >> 2) & 0x1f);
    match (bit12, rd, rs2) {
        (0, _, 0) => jalr_form(0, rd),                 // c.jr rs1    -> jalr x0, rs1, 0
        (0, _, _) => reg_alu(funct3::ADD, rd, 0, rs2), // c.mv rd,rs2 -> add rd, x0, rs2
        (_, 0, 0) => ebreak_form(),                    // c.ebreak
        (_, _, 0) => jalr_form(1, rd),                 // c.jalr rs1  -> jalr x1, rs1, 0
        (_, _, _) => reg_alu(funct3::ADD, rd, rd, rs2), // c.add rd,rs2 -> add rd, rd, rs2
    }
}

fn reg_alu(funct3: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode::OP
}

fn jalr_form(rd: u32, rs1: u32) -> u32 {
    (rs1 << 15) | (rd << 7) | opcode::JALR // funct3 0, imm 0
}

fn ebreak_form() -> u32 {
    (priv12::EBREAK << 20) | opcode::SYSTEM // funct3 0 (PRIV)
}

/// A compressed 3-bit register field (`rd'`/`rs'`) names `x8..x15`.
fn creg(field: u32) -> u32 {
    8 + (field & 0x7)
}

/// Encode `addi rd, rs1, imm` (low 12 bits of `imm` used).
fn addi_word(rd: u32, rs1: u32, imm: u32) -> u32 {
    ((imm & 0xfff) << 20) | (rs1 << 15) | (rd << 7) | opcode::OP_IMM
}

/// `c.addi4spn rd', uimm` -> `addi rd', x2, uimm`. CIW unsigned offset:
/// `uimm[5:4]`=inst[12:11], `uimm[9:6]`=inst[10:7], `uimm[2]`=inst[6], `uimm[3]`=inst[5].
fn expand_c_addi4spn(half: u16) -> u32 {
    let h = u32::from(half);
    let rd = creg(h >> 2);
    let uimm = (((h >> 11) & 0x3) << 4)
        | (((h >> 7) & 0xf) << 6)
        | (((h >> 6) & 0x1) << 2)
        | (((h >> 5) & 0x1) << 3);
    addi_word(rd, 2, uimm)
}

/// Quadrant 01, funct3 011: `c.addi16sp` when rd == 2, else `c.lui`
/// (the latter not yet modeled — surfaces via the meta-loop when hit).
fn expand_c_lui_addi16sp(half: u16) -> Option<u32> {
    let rd = (u32::from(half) >> 7) & 0x1f;
    (rd == 2).then(|| expand_c_addi16sp(half))
}

/// `c.addi16sp sp, nzimm` -> `addi x2, x2, nzimm`. Scaled signed offset:
/// `nzimm[9]`=inst[12], `[8:7]`=inst[4:3], `[6]`=inst[5], `[5]`=inst[2], `[4]`=inst[6].
fn expand_c_addi16sp(half: u16) -> u32 {
    let h = u32::from(half);
    let nzimm = (((h >> 12) & 1) << 9)
        | (((h >> 3) & 3) << 7)
        | (((h >> 5) & 1) << 6)
        | (((h >> 2) & 1) << 5)
        | (((h >> 6) & 1) << 4);
    addi_word(2, 2, sign_extend(nzimm, 10) as u32)
}

/// `c.j offset` -> `jal x0, offset`.
fn expand_c_j(half: u16) -> u32 {
    jal_word(0, cj_offset(half))
}

/// Sign-extended CJ-format jump offset (the spec's scrambled bit order).
fn cj_offset(half: u16) -> u32 {
    let h = u32::from(half);
    let imm = (((h >> 12) & 1) << 11)
        | (((h >> 11) & 1) << 4)
        | (((h >> 9) & 3) << 8)
        | (((h >> 8) & 1) << 10)
        | (((h >> 7) & 1) << 6)
        | (((h >> 6) & 1) << 7)
        | (((h >> 3) & 7) << 1)
        | (((h >> 2) & 1) << 5);
    sign_extend(imm, 12) as u32
}

/// `c.sdsp rs2, uimm(sp)` -> `sd rs2, uimm(x2)`. CSS unsigned offset:
/// `uimm[5:3]` = inst[12:10], `uimm[8:6]` = inst[9:7] (already byte-scaled).
fn expand_c_swsp(half: u16) -> u32 {
    let h = u32::from(half);
    let rs2 = (h >> 2) & 0x1f;
    let uimm = (((h >> 9) & 0xf) << 2) | (((h >> 7) & 0x3) << 6);
    store_word(funct3::store::SW, 2, rs2, uimm)
}

fn expand_c_sdsp(half: u16) -> u32 {
    let h = u32::from(half);
    let rs2 = (h >> 2) & 0x1f;
    let uimm = (((h >> 10) & 0x7) << 3) | (((h >> 7) & 0x7) << 6);
    store_word(funct3::store::SD, 2, rs2, uimm)
}

/// `c.ldsp rd, uimm(sp)` -> `ld rd, uimm(x2)`. CI unsigned offset:
/// `uimm[5]`=inst[12], `uimm[4:3]`=inst[6:5], `uimm[8:6]`=inst[4:2].
fn expand_c_lwsp(half: u16) -> u32 {
    let h = u32::from(half);
    let rd = (h >> 7) & 0x1f;
    let uimm = (((h >> 12) & 1) << 5) | (((h >> 4) & 0x7) << 2) | (((h >> 2) & 0x3) << 6);
    load_word(funct3::load::LW, rd, 2, uimm)
}

fn expand_c_ldsp(half: u16) -> u32 {
    let h = u32::from(half);
    let rd = (h >> 7) & 0x1f;
    let uimm = (((h >> 12) & 1) << 5) | (((h >> 5) & 3) << 3) | (((h >> 2) & 7) << 6);
    load_word(funct3::load::LD, rd, 2, uimm)
}

/// Encode an I-type load `funct3 rd, imm(base)`.
fn load_word(funct3: u32, rd: u32, base: u32, imm: u32) -> u32 {
    ((imm & 0xfff) << 20) | (base << 15) | (funct3 << 12) | (rd << 7) | opcode::LOAD
}

/// `c.ld rd', uimm(rs1')` -> `ld rd', uimm(rs1')`. CL unsigned offset:
/// `uimm[5:3]`=inst[12:10], `uimm[7:6]`=inst[6:5].
fn expand_c_ld(half: u16) -> u32 {
    let h = u32::from(half);
    let rs1 = creg(h >> 7);
    let rd = creg(h >> 2);
    let uimm = (((h >> 10) & 0x7) << 3) | (((h >> 5) & 0x3) << 6);
    load_word(funct3::load::LD, rd, rs1, uimm)
}

/// `c.sd rs2', uimm(rs1')` -> `sd rs2', uimm(rs1')`. CS unsigned offset:
/// `uimm[5:3]`=inst[12:10], `uimm[7:6]`=inst[6:5].
fn expand_c_sd(half: u16) -> u32 {
    let h = u32::from(half);
    let rs1 = creg(h >> 7);
    let rs2 = creg(h >> 2);
    let uimm = (((h >> 10) & 0x7) << 3) | (((h >> 5) & 0x3) << 6);
    store_word(funct3::store::SD, rs1, rs2, uimm)
}

/// Encode an S-type store `funct3 src, imm(base)`.
fn store_word(funct3: u32, base: u32, src: u32, imm: u32) -> u32 {
    (((imm >> 5) & 0x7f) << 25)
        | (src << 20)
        | (base << 15)
        | (funct3 << 12)
        | ((imm & 0x1f) << 7)
        | opcode::STORE
}

/// Quadrant 01, funct3 100: the misc-ALU cluster, by bits[11:10]. Only the
/// CA-format register ops (bits[11:10]=11) are modeled so far; the
/// `c.srli`/`c.srai`/`c.andi` cases surface via the meta-loop when hit.
fn expand_c_misc_alu(half: u16) -> Option<u32> {
    match (u32::from(half) >> 10) & 0x3 {
        0b00 => Some(expand_c_srli(half)),
        0b11 => expand_c_ca(half),
        _ => None, // c.srai (01), c.andi (10) not yet
    }
}

/// `c.srli rd', shamt` -> `srli rd', rd', shamt` (6-bit shamt for RV64).
fn expand_c_srli(half: u16) -> u32 {
    let h = u32::from(half);
    let rd = creg(h >> 7);
    let shamt = (((h >> 12) & 1) << 5) | ((h >> 2) & 0x1f);
    shift_imm_word(funct3::SR, 0, rd, shamt)
}

/// Encode an OP-IMM shift `funct3 rd, rd, shamt` (`alt` is 0 or `ALT_OP_BIT`).
fn shift_imm_word(funct3: u32, alt: u32, rd: u32, shamt: u32) -> u32 {
    alt | (shamt << 20) | (rd << 15) | (funct3 << 12) | (rd << 7) | opcode::OP_IMM
}

/// CA-format register-register ops (`c.sub`/`c.xor`/`c.or`/`c.and`/`c.subw`/
/// `c.addw`), by bit 12 and bits[6:5]. Only `c.and` is modeled so far.
fn expand_c_ca(half: u16) -> Option<u32> {
    let h = u32::from(half);
    let rd = creg(h >> 7); // rd'/rs1'
    let rs2 = creg(h >> 2);
    match ((h >> 12) & 1, (h >> 5) & 0x3) {
        (0, 0b00) => Some(reg_alu(funct3::ADD, rd, rd, rs2) | ALT_OP_BIT), // c.sub
        (0, 0b11) => Some(reg_alu(funct3::AND, rd, rd, rs2)),             // c.and
        _ => None,
    }
}

/// `c.beqz rs1', offset` -> `beq rs1', x0, offset`.
fn expand_c_beqz(half: u16) -> u32 {
    let rs1 = creg(u32::from(half) >> 7);
    branch_word(funct3::branch::BEQ, rs1, 0, cb_offset(half))
}

/// `c.bnez rs1', offset` -> `bne rs1', x0, offset`.
fn expand_c_bnez(half: u16) -> u32 {
    let rs1 = creg(u32::from(half) >> 7);
    branch_word(funct3::branch::BNE, rs1, 0, cb_offset(half))
}

/// Sign-extended CB-format branch offset: `offset[8]`=inst[12],
/// `[4:3]`=inst[11:10], `[7:6]`=inst[6:5], `[2:1]`=inst[4:3], `[5]`=inst[2].
fn cb_offset(half: u16) -> u32 {
    let h = u32::from(half);
    let imm = (((h >> 12) & 1) << 8)
        | (((h >> 10) & 3) << 3)
        | (((h >> 5) & 3) << 6)
        | (((h >> 3) & 3) << 1)
        | (((h >> 2) & 1) << 5);
    sign_extend(imm, 9) as u32
}

/// Encode a B-type branch `funct3 rs1, rs2, imm` from a sign-extended `imm`.
fn branch_word(funct3: u32, rs1: u32, rs2: u32, imm: u32) -> u32 {
    (((imm >> 12) & 1) << 31)
        | (((imm >> 5) & 0x3f) << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (((imm >> 1) & 0xf) << 8)
        | (((imm >> 11) & 1) << 7)
        | opcode::BRANCH
}

/// Encode `jal rd, imm` (J-type) from a sign-extended `imm`.
fn jal_word(rd: u32, imm: u32) -> u32 {
    (((imm >> 20) & 1) << 31)
        | (((imm >> 1) & 0x3ff) << 21)
        | (((imm >> 11) & 1) << 20)
        | (((imm >> 12) & 0xff) << 12)
        | (rd << 7)
        | opcode::JAL
}
