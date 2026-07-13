//! The hart: register file, program counter, instruction-count clock, and
//! the fetch/decode/execute `step`. The single API everything tests through.

use std::sync::Arc;

use crate::block::{self, Block, BlockCache, Compiled};
use crate::bus::Bus;
use crate::csr::{Csr, CsrError, addr, sstatus};
use crate::decode::{Instr, amo_op, expand, funct3, funct7, is_compressed, opcode, priv12, system};
use crate::decode_cache::{DecodeCache, Decoded};
use crate::mem::{BusError, Memory, RAM_BASE};
use crate::mmu::{self, Access};

/// Instruction lengths in bytes.
const ILEN_FULL: u64 = 4;
const ILEN_COMPRESSED: u64 = 2;

/// The privilege mode the hart is executing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Privilege {
    User,
    Supervisor,
}

/// Whether a hart is executing or parked. Secondary harts boot `Stopped` and are
/// woken by an SBI `hart_start`; the boot hart starts `Running`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HartState {
    Running,
    Stopped,
    /// Halted on `wfi`, waiting for an interrupt to become pending (its timer
    /// reaching `stimecmp`, or an IPI). Retires no instructions until woken —
    /// this is what lets a driver fast-forward the clock over idle time instead
    /// of grinding through the idle loop one instruction at a time.
    Idle,
}

/// Set (`on`) or clear (`!on`) the bits of `mask` in `value`.
fn with_bit(value: u64, mask: u64, on: bool) -> u64 {
    if on { value | mask } else { value & !mask }
}

/// Instret the interpreter would retire for a `memset`/`memcpy` of `len` bytes —
/// dominated by the word-store loop (~`len/8` iterations), plus a per-call fixed
/// cost and a byte tail. The native-op helper charges this to the clock so a run
/// with helpers on totals the same guest instret as the pure interpreter, keeping
/// the deterministic timing (and thus the frame stream) identical. Constants are
/// disassembly-informed and validated by the `--calibrate-memops` probe: over an
/// `init` boot `real/charged = 1.011` (was 1.121 with the old BASE=8), and the
/// `snemu-itest` on↔off *guest* instret matches to within ~1% on 94/110 scenarios.
pub(crate) fn memop_charge(len: u64) -> u64 {
    // The word loop is `sd; addi; bltu` = 3/word. The per-call fixed cost — prologue
    // + head-align setup + the 7-insn 8-byte splat + tail setup + ret ≈ 24 — is what
    // the old BASE=8 lowballed; tail bytes run the 4-insn byte loop, not 1.
    const BASE: u64 = 24;
    const PER_WORD: u64 = 3;
    const PER_TAIL_BYTE: u64 = 4;
    BASE + (len / 8) * PER_WORD + (len % 8) * PER_TAIL_BYTE
}

/// Trap cause codes (`scause`, exceptions; interrupt bit clear).
mod cause {
    pub const BREAKPOINT: u64 = 3;
    pub const ECALL_FROM_U: u64 = 8;
    pub const INSTRUCTION_PAGE_FAULT: u64 = 12;
    pub const LOAD_PAGE_FAULT: u64 = 13;
    pub const STORE_PAGE_FAULT: u64 = 15;
    // S-mode ecall (code 9) never reaches the kernel: snemu services it as an
    // SBI firmware call, so there's no `ECALL_FROM_S` trap.
    /// The top `scause` bit marks an interrupt (vs. an exception).
    pub const INTERRUPT: u64 = 1 << 63;
    /// Supervisor software interrupt code (with [`INTERRUPT`] set).
    pub const SUPERVISOR_SOFTWARE: u64 = 1;
    /// Supervisor timer interrupt code (with [`INTERRUPT`] set).
    pub const SUPERVISOR_TIMER: u64 = 5;
}

/// `sie.STIE` / `sie.SSIE` — supervisor timer / software interrupt enables.
const SIE_STIE: u64 = 1 << 5;
const SIE_SSIE: u64 = 1 << 1;
/// `sip.SSIP` — supervisor software interrupt pending (set by an IPI, cleared
/// by the kernel's `csrc sip`).
const SIP_SSIP: u64 = 1 << 1;

/// SBI calls the kernel makes from S-mode (snemu plays firmware).
mod sbi {
    /// Send-IPI extension id (`"sPI"`), function 0 = `sbi_send_ipi`.
    pub const EID_IPI: u64 = 0x0073_5049;
    pub const FID_SEND_IPI: u64 = 0;
    /// Hart State Management extension id (`"HSM"`), function 0 = `sbi_hart_start`.
    pub const EID_HSM: u64 = 0x0048_534D;
    pub const FID_HART_START: u64 = 0;
    pub const SUCCESS: i64 = 0;
    pub const ERR_NOT_SUPPORTED: i64 = -2;
    pub const ERR_INVALID_PARAM: i64 = -3;
    pub const ERR_ALREADY_AVAILABLE: i64 = -6;
}

/// An SBI firmware call captured from an S-mode `ecall` — serviced by the driver
/// (`Machine`/`Cpu`) against the whole hart set, since `send_ipi`/`hart_start`
/// touch harts other than the caller.
#[derive(Clone)]
pub(crate) struct SbiRequest {
    eid: u64,
    fid: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
}

/// What a `step` asks the driver to do after it returns — cross-hart work a hart
/// can't do while it only holds `&mut self`.
pub(crate) enum HartEffect {
    None,
    Sbi(SbiRequest),
    /// The hart is parked on `wfi` and retired nothing this step. The driver
    /// uses this to fast-forward the shared clock to the earliest wake deadline
    /// once every hart is idle (or stopped).
    Idle,
    /// A block JIT block retired `n` instructions this step (typically > 1). The
    /// driver advances the shared clock by `n` — the analogue of `None`'s single
    /// tick, generalised to a whole block.
    Block(u64),
}

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

/// Combine the old memory value with `rhs` per an AMO `funct5`. Single hart, so
/// the read-modify-write is atomic for free. `None` for LR/SC (not arithmetic).
fn amo_combine_u64(funct5: u32, old: u64, rhs: u64) -> Option<u64> {
    Some(match funct5 {
        amo_op::SWAP => rhs,
        amo_op::ADD => old.wrapping_add(rhs),
        amo_op::XOR => old ^ rhs,
        amo_op::OR => old | rhs,
        amo_op::AND => old & rhs,
        amo_op::MIN => (old as i64).min(rhs as i64) as u64,
        amo_op::MAX => (old as i64).max(rhs as i64) as u64,
        amo_op::MINU => old.min(rhs),
        amo_op::MAXU => old.max(rhs),
        _ => return None,
    })
}

/// The 32-bit `.w` form: arithmetic wraps within 32 bits, signed compares use i32.
fn amo_combine_u32(funct5: u32, old: u32, rhs: u32) -> Option<u32> {
    Some(match funct5 {
        amo_op::SWAP => rhs,
        amo_op::ADD => old.wrapping_add(rhs),
        amo_op::XOR => old ^ rhs,
        amo_op::OR => old | rhs,
        amo_op::AND => old & rhs,
        amo_op::MIN => (old as i32).min(rhs as i32) as u32,
        amo_op::MAX => (old as i32).max(rhs as i32) as u32,
        amo_op::MINU => old.min(rhs),
        amo_op::MAXU => old.max(rhs),
        _ => return None,
    })
}

/// Why a `step` could not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepError {
    /// Instruction fetch or a memory access fell outside RAM.
    Bus(BusError),
    /// The decoder doesn't know this instruction yet (the meta-loop signal).
    Unimplemented { pc: u64, instr: u32 },
    /// A `csr*` instruction named a CSR snemu doesn't model yet (meta-loop).
    UnknownCsr { pc: u64, addr: u16 },
    /// Sv39 translation failed for `va` (unmapped or permission-denied). A real
    /// guest page-fault trap is future work; for now this halts the run.
    PageFault { va: u64 },
}

/// How a `csr*` instruction combines the source operand with the old value.
#[derive(Clone, Copy)]
enum CsrOp {
    Write,
    Set,
    Clear,
}

fn csr_step_error(pc: u64, e: CsrError) -> StepError {
    match e {
        CsrError::Unknown(addr) => StepError::UnknownCsr { pc, addr },
    }
}

impl From<BusError> for StepError {
    fn from(e: BusError) -> Self {
        StepError::Bus(e)
    }
}

/// A single RISC-V hart: register file, pc, CSRs, and privilege. The memory and
/// devices it runs against live in a shared [`Bus`] threaded through `step`, so
/// several harts can share one address space (see `Machine`).
#[derive(Clone)]
pub(crate) struct Hart {
    x: [u64; 32],
    pc: u64,
    instret: u64,
    /// The shared machine clock as of this step — the `rdtime` / `stimecmp`
    /// source. Set by the driver (the `Cpu` wrapper or the `Machine`) before each
    /// `step`, so every hart reads one common monotonic clock, not its own
    /// per-hart retired count.
    cycle: u64,
    /// Length in bytes of the instruction currently executing (2 or 4); set by
    /// `step` and used for pc advance and link addresses.
    cur_ilen: u64,
    privilege: Privilege,
    csr: Csr,
    /// Address reserved by the most recent `lr`, if still valid. `sc` succeeds
    /// only while it holds; any store to that address breaks it.
    reservation: Option<u64>,
    /// Running or parked (secondary harts start parked until `hart_start`).
    state: HartState,
    /// An SBI request captured from an S-mode `ecall` this step, drained by
    /// `step` into a [`HartEffect`] for the driver to service.
    pending_sbi: Option<SbiRequest>,
    /// Tier-1 decode cache (M5), or `None` when disabled — the default, which
    /// runs the pure interpreter (the correctness oracle). Toggled per hart via
    /// [`set_decode_cache`](Self::set_decode_cache).
    decode_cache: Option<DecodeCache>,
    /// Tier-2 block JIT (M6) `PC → block` cache, or `None` when disabled (the
    /// default oracle path). Toggled per hart via [`set_block_jit`](Self::set_block_jit).
    block_cache: Option<BlockCache>,
    /// Whether the block executor caches the register file in a host local (M6
    /// increment 4). On by default; `false` runs each op through the hart directly —
    /// the A/B baseline proving the cache changes only speed, not architectural state.
    reg_cache: bool,
    /// Whether `wfi` parks the hart (Idle) so the driver can fast-forward the
    /// clock over idle time, vs. acting as a bare nop that advances. On by
    /// default; toggled per hart via [`set_idle_skip`](Self::set_idle_skip) so a
    /// run with it on can be proven identical to one with it off (the decode-cache
    /// philosophy — the interpreter stays the oracle).
    idle_skip: bool,
    /// Whether the block JIT uses **Backend B** (native AArch64 codegen) instead of
    /// **Backend A** (the reified-`Op` interpreter) to execute compiled blocks. Off by
    /// default — A is the correct-by-construction oracle and the only backend that
    /// runs in the browser. Independent of `block_cache`: B still needs A's frontend
    /// (discovery/cache), and falls back to A for any block it can't emit natively, so
    /// a run with B on must stay byte-identical to one with it off. Only consulted on
    /// hosts where the `jit` module compiles (aarch64/macos today).
    native_jit: bool,
    /// Backend B's compiled-native-code cache (host-only). Rebuilt lazily; excluded
    /// from the snapshot (clones cold) and flushed with the block cache. Present only
    /// where the `jit` module compiles.
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    native_cache: crate::jit::NativeCache,
    /// Diagnostic: supervisor timer interrupts actually delivered to this hart.
    timer_fires: u64,
}

/// A single-hart machine: one [`Hart`] plus the [`Bus`] it owns. The convenience
/// wrapper the loader, `main`, and the unit tests drive; multi-hart runs use a
/// `Machine` that shares one `Bus` across several `Hart`s instead.
pub struct Cpu {
    hart: Hart,
    bus: Bus,
}

impl Cpu {
    /// A fresh single-hart machine over `mem`, positioned at the RAM base.
    #[must_use]
    pub fn new(mem: Memory) -> Self {
        Self {
            hart: Hart::new(),
            bus: Bus::new(mem),
        }
    }

    /// Fetch/decode/execute one instruction against this machine's bus. The
    /// single hart's clock is just its own retired count; an SBI call is serviced
    /// against the lone hart (a self-IPI targets it; `hart_start` finds no peer).
    pub fn step(&mut self) -> Result<(), StepError> {
        self.hart.set_cycle(self.hart.instret);
        match self.hart.step(&mut self.bus)? {
            HartEffect::Sbi(req) => {
                service_sbi(std::slice::from_mut(&mut self.hart), 0, &req);
            }
            // The lone hart parked on wfi: jump its clock (its retired-instruction
            // count) to the timer deadline so the next step delivers the interrupt,
            // instead of returning without progress forever.
            HartEffect::Idle => {
                if let Some(deadline) = self.hart.wake_deadline() {
                    self.hart.instret = self.hart.instret.max(deadline);
                }
            }
            // A block already advanced the hart's own instret; the wrapper's clock
            // reads it directly, so there's nothing extra to do.
            HartEffect::None | HartEffect::Block(_) => {}
        }
        Ok(())
    }

    #[must_use]
    pub fn privilege(&self) -> Privilege {
        self.hart.privilege
    }

    #[must_use]
    pub fn uart_output(&self) -> &[u8] {
        self.bus.uart_output()
    }

    /// Bytes the virtio-console has transmitted (the decoded telemetry stream).
    #[must_use]
    pub fn virtio_tx_output(&self) -> &[u8] {
        self.bus.virtio_tx_output()
    }

    /// The current `satp` value (for diagnostics).
    #[must_use]
    pub fn satp(&self) -> u64 {
        self.hart.satp()
    }

    #[must_use]
    pub fn reg(&self, i: usize) -> u64 {
        self.hart.reg(i)
    }

    pub fn set_reg(&mut self, i: usize, value: u64) {
        self.hart.set_reg(i, value);
    }

    #[must_use]
    pub fn pc(&self) -> u64 {
        self.hart.pc
    }

    pub fn set_pc(&mut self, addr: u64) {
        self.hart.pc = addr;
    }

    #[must_use]
    pub fn instret(&self) -> u64 {
        self.hart.instret
    }

    /// Enable or disable this hart's Tier-1 decode cache (M5).
    pub fn set_decode_cache(&mut self, on: bool) {
        self.hart.set_decode_cache(on);
    }

    /// Enable/disable the Tier-2 block JIT on the lone hart.
    pub fn set_block_jit(&mut self, on: bool) {
        self.hart.set_block_jit(on);
    }

    /// Select Backend B (native codegen) vs Backend A (the `Op` interpreter) for the
    /// block JIT on the lone hart. Off by default; A is the oracle.
    pub fn set_native_jit(&mut self, on: bool) {
        self.hart.set_native_jit(on);
    }

    /// Enable/disable block-executor register caching on the lone hart.
    pub fn set_register_cache(&mut self, on: bool) {
        self.hart.set_register_cache(on);
    }

    /// Enable or disable `wfi` idle-skip (on by default). Off restores the bare
    /// nop-`wfi` behaviour — the baseline for proving idle-skip changes only speed.
    pub fn set_idle_skip(&mut self, on: bool) {
        self.hart.set_idle_skip(on);
    }

    /// Decode-cache hits so far (0 when the cache is disabled). Used by the
    /// equivalence test to confirm the fast path engaged.
    #[cfg(test)]
    #[must_use]
    pub fn decode_cache_hits(&self) -> u64 {
        self.hart.decode_cache.as_ref().map_or(0, DecodeCache::hits)
    }
}

impl Hart {
    /// A fresh hart, started in S-mode (the privilege the kernel boots in;
    /// firmware/snemu has already dropped out of M-mode at reset).
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            x: [0; 32],
            pc: RAM_BASE,
            instret: 0,
            cycle: 0,
            cur_ilen: ILEN_FULL,
            privilege: Privilege::Supervisor,
            csr: Csr::new(),
            reservation: None,
            state: HartState::Running,
            pending_sbi: None,
            decode_cache: None,
            block_cache: None,
            reg_cache: true,
            idle_skip: true,
            native_jit: false,
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            native_cache: crate::jit::NativeCache::new(),
            timer_fires: 0,
        }
    }

    /// Supervisor timer interrupts delivered to this hart (diagnostic).
    pub(crate) fn timer_fires(&self) -> u64 {
        self.timer_fires
    }

    /// Enable or disable this hart's Tier-1 decode cache. Enabling starts from a
    /// cold cache; disabling drops it (back to the pure interpreter). The flag is
    /// what lets snemu run the interpreter as the oracle and prove the cache
    /// changes nothing but speed.
    pub(crate) fn set_decode_cache(&mut self, on: bool) {
        self.decode_cache = on.then(DecodeCache::default);
    }

    /// Enable or disable this hart's Tier-2 block JIT (M6). Like the decode cache,
    /// it's a pure speedup proven by the on↔off A/B (`set_block_jit(false)` is the
    /// interpreter oracle). Starts from a cold cache; disabling drops it.
    pub(crate) fn set_block_jit(&mut self, on: bool) {
        self.block_cache = on.then(BlockCache::default);
    }

    /// Enable or disable **Backend B** (native codegen) for this hart's block JIT.
    /// A pure speedup over Backend A, proven by the on↔off A/B; `false` is the
    /// interpreter-over-IR oracle. Has effect only when the block JIT (frontend) is
    /// also on and the host supports native emission.
    pub(crate) fn set_native_jit(&mut self, on: bool) {
        self.native_jit = on;
    }

    /// Whether Backend B (native codegen) is selected — the block executor reads this
    /// to choose native execution vs. the reified-`Op` walk.
    pub(crate) fn native_jit_enabled(&self) -> bool {
        self.native_jit
    }

    /// Enable/disable register caching in the block executor (M6 increment 4). On by
    /// default; off runs each op through the hart — the A/B baseline for the cache.
    pub(crate) fn set_register_cache(&mut self, on: bool) {
        self.reg_cache = on;
    }

    pub(crate) fn reg_cache_enabled(&self) -> bool {
        self.reg_cache
    }

    /// Block JIT hits so far — used by tests to prove the fast path engaged.
    #[cfg(test)]
    pub(crate) fn block_jit_hits(&self) -> u64 {
        self.block_cache.as_ref().map_or(0, BlockCache::hits)
    }

    /// Block JIT fast path: if a compiled hot block starts at the current PC, run it
    /// and return the instructions it retired. Otherwise count the PC toward
    /// hotness, compile it once hot, and — for a non-empty block — run it. Returns
    /// `None` when there's no block to run (still cold, or the PC starts with an
    /// instruction the compiler can't lower), so the caller interprets one
    /// instruction. Interrupts were checked by `step` before this, so a block runs
    /// interrupt-free (the per-block hoist; a timer is at most one block late).
    fn try_run_block(&mut self, bus: &mut Bus) -> Result<Option<u64>, StepError> {
        let pc = self.pc;
        // Already compiled? A cheap `Arc` clone releases the cache borrow, freeing
        // `&mut self` for the executor.
        if let Some(block) = self.block_cache.as_mut().and_then(|c| c.get(pc)) {
            if block.is_empty() {
                return Ok(None); // "nothing compilable starts here" marker → interpret
            }
            return Ok(Some(self.run_block(&block, bus)?));
        }
        // Cold: count it; compile only once it crosses the hotness threshold.
        if !self.block_cache.as_mut().expect("block jit is on").record_hot(pc) {
            return Ok(None);
        }
        let block = Arc::new(self.compile_block(bus));
        self.block_cache.as_mut().expect("block jit is on").insert(pc, Arc::clone(&block));
        if block.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.run_block(&block, bus)?))
    }

    /// Execute a compiled block via Backend B (native codegen) when it's selected and
    /// the block is natively compilable, else Backend A (the reified-`Op` walk). Both
    /// are architecturally identical (the on↔off A/B oracle); B is a pure speedup.
    fn run_block(&mut self, block: &Block, bus: &mut Bus) -> Result<u64, StepError> {
        if self.native_jit_enabled()
            && let Some(retired) = self.run_block_native(block)
        {
            return Ok(retired);
        }
        block.exec(self, bus)
    }

    /// Backend B: run `block` natively against the register file, applying the result
    /// and resume PC. `None` when the block isn't natively compilable (a memory op or
    /// an ALU family the emitter doesn't cover) — the caller runs Backend A. Native
    /// blocks have no memory ops, so `bus` isn't needed. Host-only.
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn run_block_native(&mut self, block: &Block) -> Option<u64> {
        let pc = self.pc;
        let mut regs = self.x;
        let exit = self.native_cache.run(pc, block.ops(), block.exit_pc(), &mut regs)?;
        self.set_registers(regs);
        self.pc = exit.pc;
        Some(exit.retired)
    }

    /// Backend B is unavailable off aarch64/macos — always fall back to Backend A.
    #[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
    fn run_block_native(&mut self, _block: &Block) -> Option<u64> {
        None
    }

    /// Enable or disable `wfi` idle-skip. With it off, `wfi` is a bare
    /// nop-that-advances (the pre-fidelity behaviour) and the driver never
    /// fast-forwards — the A/B baseline that proves idle-skip changes only speed,
    /// not telemetry.
    pub(crate) fn set_idle_skip(&mut self, on: bool) {
        self.idle_skip = on;
    }

    /// Park this hart (a secondary before its `hart_start`).
    pub(crate) fn park(&mut self) {
        self.state = HartState::Stopped;
    }

    /// Wake this parked hart at `pc` with `a0 = hartid`, `a1 = opaque` — the SBI
    /// `hart_start` contract. A parked-from-birth secondary is otherwise in reset
    /// state (MMU off, S-mode), so this is all the setup a fresh start needs.
    pub(crate) fn start(&mut self, pc: u64, hartid: u64, opaque: u64) {
        self.pc = pc;
        self.set_reg(10, hartid);
        self.set_reg(11, opaque);
        self.state = HartState::Running;
    }

    /// Raise this hart's supervisor software-interrupt pending bit (`sip.SSIP`) —
    /// the effect of an IPI targeting it.
    pub(crate) fn raise_software_interrupt(&mut self) {
        let sip = self.csr_read(addr::SIP) | SIP_SSIP;
        self.csr_write(addr::SIP, sip);
    }

    #[must_use]
    pub(crate) fn is_running(&self) -> bool {
        self.state == HartState::Running
    }

    #[must_use]
    pub(crate) fn is_idle(&self) -> bool {
        self.state == HartState::Idle
    }

    #[must_use]
    pub(crate) fn is_stopped(&self) -> bool {
        self.state == HartState::Stopped
    }

    /// Test helper: arm this hart's supervisor timer at `deadline` with the
    /// interrupt fully enabled, and park it idle — the state a hart is in after
    /// `wfi` in the idle loop. Lets a `Machine` test exercise the fast-forward
    /// without hand-assembling the CSR-setup + `wfi` guest sequence.
    #[cfg(test)]
    pub(crate) fn arm_idle_timer(&mut self, deadline: u64) {
        self.csr.write(addr::STIMECMP, deadline).unwrap();
        self.csr.write(addr::SIE, SIE_STIE).unwrap();
        self.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();
        self.state = HartState::Idle;
    }

    /// Set the shared machine clock this hart observes for its next `step`.
    pub(crate) fn set_cycle(&mut self, cycle: u64) {
        self.cycle = cycle;
    }

    /// The current `satp` value (for diagnostics).
    #[must_use]
    pub(crate) fn satp(&self) -> u64 {
        self.csr.read(addr::SATP).unwrap_or(0)
    }

    #[must_use]
    pub(crate) fn reg(&self, i: usize) -> u64 {
        self.x[i]
    }

    /// Fold this hart's **architectural** state into `h` — the register file, PC,
    /// privilege, CSRs, the LR/SC reservation, and running/parked state: everything
    /// that determines future execution. The performance toggles (decode/block
    /// caches, `reg_cache`, `idle_skip`, `native_ops`) are deliberately excluded —
    /// they change speed, not architectural state, so two runs that differ only in
    /// them must still hash equal (the A/B fidelity discipline). `instret`/`cycle`/
    /// `timer_fires` are diagnostics, and `pending_sbi` is drained within each step
    /// (always `None` between steps), so none of them are folded in.
    pub(crate) fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.x.hash(h);
        self.pc.hash(h);
        self.privilege.hash(h);
        self.csr.hash_state(h);
        self.reservation.hash(h);
        self.state.hash(h);
    }

    pub(crate) fn set_reg(&mut self, i: usize, value: u64) {
        if i != 0 {
            self.x[i] = value;
        }
    }

    /// Snapshot the register file — the block executor caches it in a host local
    /// and operates on that across the whole block (M6 increment 4).
    pub(crate) fn registers(&self) -> [u64; 32] {
        self.x
    }

    /// Restore the register file from a block executor's cache, keeping `x0` zero.
    pub(crate) fn set_registers(&mut self, mut regs: [u64; 32]) {
        regs[0] = 0;
        self.x = regs;
    }

    #[must_use]
    pub(crate) fn pc(&self) -> u64 {
        self.pc
    }

    pub(crate) fn set_pc(&mut self, pc: u64) {
        self.pc = pc;
    }

    /// Translate a guest virtual address through `satp` (Sv39 or bare). On a page
    /// fault, deliver an S-mode trap (scause = the fault cause by access kind,
    /// stval = the faulting VA) and return `None` so the caller aborts the
    /// instruction — real hardware traps to the kernel's handler, it doesn't halt.
    fn translate_or_trap(&mut self, va: u64, access: Access, bus: &Bus) -> Option<u64> {
        let satp = self.csr.read(addr::SATP).expect("satp is modeled");
        let user = self.privilege == Privilege::User;
        let sum = self.csr_read(addr::SSTATUS) & sstatus::SUM != 0;
        match mmu::translate(satp, va, access, bus.ram(), user, sum) {
            Ok(pa) => Some(pa),
            Err(_) => {
                let cause = match access {
                    Access::Fetch => cause::INSTRUCTION_PAGE_FAULT,
                    Access::Load => cause::LOAD_PAGE_FAULT,
                    Access::Store => cause::STORE_PAGE_FAULT,
                };
                self.take_trap(cause, va);
                None
            }
        }
    }

    /// Native-op helper (tier-0.5 of the JIT): if `pc` is the entry of `memset` or
    /// `memcpy`, execute the whole op against guest RAM in one shot and return the
    /// instret the interpreted loop would have retired (charged to the clock so the
    /// deterministic timing — and thus the frame stream — is unchanged). Returns
    /// `None` to decline (not a memop entry, or a page it would fault on — let the
    /// interpreter run it and trap correctly). ABI: `memset(a0=dst, a1=val, a2=len)`,
    /// `memcpy(a0=dst, a1=src, a2=len)`, both returning `dst` in `a0`, `ra`=return.
    pub(crate) fn try_native_memop(
        &mut self,
        bus: &mut Bus,
        memset_pc: Option<u64>,
        memcpy_pc: Option<u64>,
    ) -> Option<u64> {
        let pc = self.pc();
        let is_set = Some(pc) == memset_pc;
        if !is_set && Some(pc) != memcpy_pc {
            return None;
        }
        let dst = self.reg(10);
        let src = self.reg(11); // memcpy source (unused for memset)
        let len = self.reg(12);

        // Translate every chunk up front (Store for dst, Load for src) so a fault
        // aborts *before* any write — no partial state to unwind. Chunk by the
        // smaller page remainder of dst/src so each chunk stays within one page each.
        let satp = self.csr.read(addr::SATP).expect("satp is modeled");
        let user = self.privilege == Privilege::User;
        let sum = self.csr_read(addr::SSTATUS) & sstatus::SUM != 0;
        let mut chunks: Vec<(u64, u64, usize)> = Vec::new(); // (dst_pa, src_pa, len)
        let mut off = 0u64;
        while off < len {
            let dva = dst + off;
            let drem = 0x1000 - (dva & 0xfff);
            let mut clen = drem.min(len - off);
            let dpa = mmu::translate(satp, dva, Access::Store, bus.ram(), user, sum).ok()?;
            let spa = if is_set {
                0
            } else {
                let sva = src + off;
                clen = clen.min(0x1000 - (sva & 0xfff));
                mmu::translate(satp, sva, Access::Load, bus.ram(), user, sum).ok()?
            };
            chunks.push((dpa, spa, clen as usize));
            off += clen;
        }

        // Execute: fill (memset) or copy (memcpy) each translated chunk.
        if is_set {
            let val = self.reg(11) as u8;
            let buf = [val; 0x1000];
            for (dpa, _, clen) in &chunks {
                bus.write_ram(*dpa, &buf[..*clen]).ok()?;
            }
        } else {
            for (dpa, spa, clen) in &chunks {
                let mut buf = [0u8; 0x1000];
                for k in 0..*clen {
                    buf[k] = bus.ram().read_u8(spa + k as u64).ok()?;
                }
                bus.write_ram(*dpa, &buf[..*clen]).ok()?;
            }
        }

        self.set_reg(10, dst); // return value = dst
        self.set_pc(self.reg(1)); // return to `ra`
        Some(memop_charge(len))
    }

    /// Compile the basic block starting at the current PC into IR (backend A of the
    /// M6 block JIT). Walks instructions — fetching without executing or trapping —
    /// lowering each via `block::compile_op`, until a branch (terminator), an
    /// instruction it can't lower (a block boundary), a fetch fault, or the length
    /// cap. Executing the returned block is byte-identical to interpreting the same
    /// instructions (the oracle property, proven by the on↔off A/B). The cap bounds
    /// how late a timer can be delivered (at most one block).
    pub(crate) fn compile_block(&self, bus: &Bus) -> Block {
        const MAX_OPS: usize = 64;
        let mut ops = Vec::new();
        let mut pc = self.pc;
        for _ in 0..MAX_OPS {
            let Some(decoded) = self.fetch_for_compile(pc, bus) else { break };
            match block::compile_op(decoded.raw, decoded.ilen, pc) {
                Compiled::Continue(op) => {
                    ops.push(op);
                    pc = pc.wrapping_add(decoded.ilen);
                }
                Compiled::Terminate(op) => {
                    ops.push(op);
                    pc = pc.wrapping_add(decoded.ilen);
                    break;
                }
                Compiled::Unsupported => break,
            }
        }
        Block::new(ops, pc)
    }

    /// Fetch + decode the instruction at `pc` without executing or trapping — the
    /// block compiler walks a block this way. `None` on a fetch fault or an illegal
    /// compressed encoding: the compiler ends the block there and the interpreter
    /// re-fetches and traps correctly at run time. Side-effect-free (`&self`).
    fn fetch_for_compile(&self, pc: u64, bus: &Bus) -> Option<Decoded> {
        let satp = self.csr.read(addr::SATP).ok()?;
        let user = self.privilege == Privilege::User;
        let sum = self.csr_read(addr::SSTATUS) & sstatus::SUM != 0;
        let pa = mmu::translate(satp, pc, Access::Fetch, bus.ram(), user, sum).ok()?;
        let half = bus.read_u16(pa).ok()?;
        if is_compressed(half) {
            Some(Decoded { raw: expand(half)?, ilen: ILEN_COMPRESSED })
        } else {
            Some(Decoded { raw: bus.read_u32(pa).ok()?, ilen: ILEN_FULL })
        }
    }

    /// Fetch, decode, and execute one instruction (16- or 32-bit) against `bus`.
    /// Returns any cross-hart work (an SBI request) for the driver to service.
    pub(crate) fn step(&mut self, bus: &mut Bus) -> Result<HartEffect, StepError> {
        // A parked (wfi) hart stays parked, retiring nothing, until an interrupt
        // becomes pending against the current clock — then it wakes and falls
        // through to deliver that interrupt as a trap.
        if self.state == HartState::Idle {
            if self.pending_interrupt().is_none() {
                return Ok(HartEffect::Idle);
            }
            self.state = HartState::Running;
        }
        // Deliver a pending interrupt before fetching: `sepc` then points at the
        // un-run instruction, so `sret` resumes exactly where we left off.
        if let Some(cause) = self.pending_interrupt() {
            if cause == cause::INTERRUPT | cause::SUPERVISOR_TIMER {
                self.timer_fires += 1;
            }
            self.take_trap(cause, 0);
            return Ok(HartEffect::None);
        }
        // Block JIT (M6): if a compiled hot block starts here, run it in one shot —
        // amortising fetch/decode/dispatch and the per-instruction interrupt probe
        // over the whole block (interrupts were just checked above). Falls through to
        // per-instruction interpretation when there's no block to run.
        if self.block_cache.is_some()
            && let Some(n) = self.try_run_block(bus)?
        {
            self.instret += n;
            return Ok(HartEffect::Block(n));
        }
        // Fast path: a cache hit skips the whole fetch pipeline (translate, byte
        // read, compressed expand) and goes straight to dispatch. Only the
        // decoded form is reused — `execute` still reads live `pc`/registers, so
        // behaviour is identical to the slow path (the equivalence the flag
        // guards). No satp read here: the cache is flushed on any translation
        // change (satp write / sfence.vma), so a live entry is valid by
        // construction — the hot path is a single array probe.
        if let Some(decoded) = self.decode_cache.as_mut().and_then(|c| c.get(self.pc)) {
            self.cur_ilen = decoded.ilen;
            self.execute(decoded.raw, bus)?;
            self.instret += 1;
            return Ok(self.pending_sbi.take().map_or(HartEffect::None, HartEffect::Sbi));
        }
        // Slow path: the full fetch pipeline, then cache the result.
        let Some(pc_pa) = self.translate_or_trap(self.pc, Access::Fetch, bus) else {
            return Ok(HartEffect::None); // fetch faulted → trapped to the handler
        };
        let half = bus.read_u16(pc_pa)?;
        let raw = if is_compressed(half) {
            self.cur_ilen = ILEN_COMPRESSED;
            expand(half).ok_or_else(|| self.unimplemented(u32::from(half)))?
        } else {
            self.cur_ilen = ILEN_FULL;
            bus.read_u32(pc_pa)?
        };
        if let Some(cache) = self.decode_cache.as_mut() {
            cache.insert(self.pc, Decoded { raw, ilen: self.cur_ilen });
        }
        self.execute(raw, bus)?;
        self.instret += 1;
        Ok(self.pending_sbi.take().map_or(HartEffect::None, HartEffect::Sbi))
    }

    fn execute(&mut self, raw: u32, bus: &mut Bus) -> Result<(), StepError> {
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
            opcode::LOAD => self.load(instr, bus),
            opcode::STORE => self.store(instr, bus),
            opcode::AMO => self.amo(instr, bus),
            opcode::SYSTEM => self.system(instr),
            opcode::MISC_MEM => {
                // fence / fence.i: no caches or store buffers to order.
                self.advance();
                Ok(())
            }
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
            self.pc.wrapping_add(self.cur_ilen)
        };
        Ok(())
    }

    /// JAL: link `pc+4` into rd, jump to `pc + offset`.
    fn jal(&mut self, instr: Instr) {
        self.set_reg(instr.rd(), self.pc.wrapping_add(self.cur_ilen));
        self.pc = self.pc.wrapping_add(instr.j_imm());
    }

    /// JALR: link `pc+4` into rd, jump to `(rs1 + offset) & !1`.
    fn jalr(&mut self, instr: Instr) {
        let target = self.x[instr.rs1()].wrapping_add(instr.i_imm()) & !1;
        self.set_reg(instr.rd(), self.pc.wrapping_add(self.cur_ilen));
        self.pc = target;
    }

    /// LOAD: read memory at `rs1 + imm`, sign/zero-extend into rd.
    fn load(&mut self, instr: Instr, bus: &Bus) -> Result<(), StepError> {
        let base = self.x[instr.rs1()];
        let Some(value) = self.load_value(bus, instr.funct3(), base, instr.i_imm() as i64)? else {
            return Ok(()); // faulted → trapped, don't advance
        };
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// STORE: write rs2 (truncated to the access width) to `rs1 + imm`.
    fn store(&mut self, instr: Instr, bus: &mut Bus) -> Result<(), StepError> {
        let base = self.x[instr.rs1()];
        let value = self.x[instr.rs2()];
        if self.store_value(bus, instr.funct3(), base, instr.s_imm() as i64, value)? {
            return Ok(()); // faulted → trapped, don't advance
        }
        self.advance();
        Ok(())
    }

    /// Execute a LOAD from base address `base` + `imm`, returning the loaded value
    /// (sign/zero-extended per `funct3`), or `None` if it page-faulted (a trap was
    /// taken against the current `pc`, which the caller set). Takes/returns values,
    /// not register indices, so the block executor can keep the register file in
    /// host locals across the op; `pc` is not advanced (the caller owns it).
    pub(crate) fn load_value(
        &mut self,
        bus: &Bus,
        funct3: u32,
        base: u64,
        imm: i64,
    ) -> Result<Option<u64>, StepError> {
        let va = base.wrapping_add(imm as u64);
        let Some(addr) = self.translate_or_trap(va, Access::Load, bus) else {
            return Ok(None);
        };
        Ok(Some(match funct3 {
            funct3::load::LB => i64::from(bus.read_u8(addr)? as i8) as u64,
            funct3::load::LH => i64::from(bus.read_u16(addr)? as i16) as u64,
            funct3::load::LW => i64::from(bus.read_u32(addr)? as i32) as u64,
            funct3::load::LD => bus.read_u64(addr)?,
            funct3::load::LBU => u64::from(bus.read_u8(addr)?),
            funct3::load::LHU => u64::from(bus.read_u16(addr)?),
            funct3::load::LWU => u64::from(bus.read_u32(addr)?),
            other => return Err(self.unimplemented(other)),
        }))
    }

    /// Execute a STORE of `value` to base `base` + `imm`. Returns `true` if it
    /// page-faulted (trap taken, caller must stop). Value-based like `load_value`.
    pub(crate) fn store_value(
        &mut self,
        bus: &mut Bus,
        funct3: u32,
        base: u64,
        imm: i64,
        value: u64,
    ) -> Result<bool, StepError> {
        let va = base.wrapping_add(imm as u64);
        let Some(addr) = self.translate_or_trap(va, Access::Store, bus) else {
            return Ok(true);
        };
        if self.reservation == Some(addr) {
            self.reservation = None; // a write to the reserved cell breaks lr/sc
        }
        match funct3 {
            funct3::store::SB => bus.write_u8(addr, value as u8)?,
            funct3::store::SH => bus.write_u16(addr, value as u16)?,
            funct3::store::SW => bus.write_u32(addr, value as u32)?,
            funct3::store::SD => bus.write_u64(addr, value)?,
            other => return Err(self.unimplemented(other)),
        }
        Ok(false)
    }

    /// AMO: atomic read-modify-write. Reads the addressed word/doubleword,
    /// combines it with rs2, stores the result, and returns the old value in rd.
    /// Single hart, so the sequence is atomic with no reservation tracking; the
    /// `aq`/`rl` ordering bits are no-ops. (LR/SC surface via the meta-loop.)
    fn amo(&mut self, instr: Instr, bus: &mut Bus) -> Result<(), StepError> {
        // AMOs touch a page that must be both readable and writable; the kernel's
        // data mappings are R+W, so checking the store permission suffices.
        let Some(addr) = self.translate_or_trap(self.x[instr.rs1()], Access::Store, bus) else {
            return Ok(()); // AMO faulted → trapped
        };
        let rs2 = self.x[instr.rs2()];
        match instr.funct5() {
            amo_op::LR => return self.load_reserved(instr, addr, bus),
            amo_op::SC => return self.store_conditional(instr, addr, rs2, bus),
            _ => {}
        }
        let old = match instr.funct3() {
            funct3::amo::W => {
                let old = bus.read_u32(addr)?;
                let new =
                    amo_combine_u32(instr.funct5(), old, rs2 as u32).ok_or(self.unimplemented(instr.0))?;
                bus.write_u32(addr, new)?;
                sext32(old)
            }
            funct3::amo::D => {
                let old = bus.read_u64(addr)?;
                let new =
                    amo_combine_u64(instr.funct5(), old, rs2).ok_or(self.unimplemented(instr.0))?;
                bus.write_u64(addr, new)?;
                old
            }
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.set_reg(instr.rd(), old);
        self.advance();
        Ok(())
    }

    /// `lr.w`/`lr.d`: load the addressed value into rd and reserve the address.
    fn load_reserved(&mut self, instr: Instr, addr: u64, bus: &Bus) -> Result<(), StepError> {
        let value = match instr.funct3() {
            funct3::amo::W => sext32(bus.read_u32(addr)?),
            funct3::amo::D => bus.read_u64(addr)?,
            _ => return Err(self.unimplemented(instr.0)),
        };
        self.reservation = Some(addr);
        self.set_reg(instr.rd(), value);
        self.advance();
        Ok(())
    }

    /// `sc.w`/`sc.d`: store rs2 iff the reservation still names this address,
    /// writing 0 (success) or 1 (failure) to rd. The reservation clears either way.
    fn store_conditional(
        &mut self,
        instr: Instr,
        addr: u64,
        rs2: u64,
        bus: &mut Bus,
    ) -> Result<(), StepError> {
        let reserved = self.reservation.take() == Some(addr);
        if reserved {
            match instr.funct3() {
                funct3::amo::W => bus.write_u32(addr, rs2 as u32)?,
                funct3::amo::D => bus.write_u64(addr, rs2)?,
                _ => return Err(self.unimplemented(instr.0)),
            }
        }
        self.set_reg(instr.rd(), u64::from(!reserved));
        self.advance();
        Ok(())
    }

    /// SYSTEM: CSR instructions and privileged ops.
    fn system(&mut self, instr: Instr) -> Result<(), StepError> {
        let reg_source = self.x[instr.rs1()];
        let imm_source = instr.rs1() as u64; // rs1 field is a 5-bit zero-extended uimm
        match instr.funct3() {
            system::PRIV => self.priv_op(instr),
            system::CSRRW => self.csr_access(instr, reg_source, CsrOp::Write),
            system::CSRRS => self.csr_access(instr, reg_source, CsrOp::Set),
            system::CSRRC => self.csr_access(instr, reg_source, CsrOp::Clear),
            system::CSRRWI => self.csr_access(instr, imm_source, CsrOp::Write),
            system::CSRRSI => self.csr_access(instr, imm_source, CsrOp::Set),
            system::CSRRCI => self.csr_access(instr, imm_source, CsrOp::Clear),
            _ => Err(self.unimplemented(instr.0)),
        }
    }

    /// Read-modify-write a CSR: old value into rd, combine the source per `op`.
    /// `Set`/`Clear` skip the write when the source is zero (no spurious write).
    fn csr_access(&mut self, instr: Instr, source: u64, op: CsrOp) -> Result<(), StepError> {
        let pc = self.pc;
        let csr = instr.csr();
        if csr == addr::TIME {
            // The `time` counter is read-only and computed, not stored: it's the
            // shared machine clock, deterministic across harts.
            self.set_reg(instr.rd(), self.cycle);
            self.advance();
            return Ok(());
        }
        let old = self.csr.read(csr).map_err(|e| csr_step_error(pc, e))?;
        let (new, do_write) = match op {
            CsrOp::Write => (source, true),
            CsrOp::Set => (old | source, source != 0),
            CsrOp::Clear => (old & !source, source != 0),
        };
        if do_write {
            self.csr.write(csr, new).map_err(|e| csr_step_error(pc, e))?;
            // Writing `satp` switches the address space, so every cached
            // (translated) instruction is now stale. This is the coherence hook
            // that lets the fast path skip re-reading satp per instruction.
            if csr == addr::SATP {
                if let Some(cache) = self.decode_cache.as_mut() {
                    cache.flush();
                }
                if let Some(cache) = self.block_cache.as_mut() {
                    cache.flush();
                }
                #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
                self.native_cache.flush();
            }
        }
        self.set_reg(instr.rd(), old);
        self.advance();
        Ok(())
    }

    /// Privileged SYSTEM ops (funct3 = 0), dispatched by funct12.
    fn priv_op(&mut self, instr: Instr) -> Result<(), StepError> {
        if instr.funct7() == funct7::SFENCE_VMA {
            // No hardware TLB to flush — translation walks every access. But the
            // decode cache IS a translated-instruction cache, so the guest's
            // invalidation must drop it (this is the coherence hook that lets the
            // fast path skip re-translation safely).
            if let Some(cache) = self.decode_cache.as_mut() {
                cache.flush();
            }
            if let Some(cache) = self.block_cache.as_mut() {
                cache.flush();
            }
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            self.native_cache.flush();
            self.advance();
            return Ok(());
        }
        match instr.funct12() {
            priv12::ECALL => {
                // U-mode ecall is a syscall — trap to the kernel. S-mode ecall is
                // an SBI call — captured here and serviced by the driver (which
                // holds every hart), since send_ipi/hart_start cross harts.
                match self.privilege {
                    Privilege::User => self.take_trap(cause::ECALL_FROM_U, 0),
                    Privilege::Supervisor => self.capture_sbi_call(),
                }
                Ok(())
            }
            priv12::EBREAK => {
                self.take_trap(cause::BREAKPOINT, self.pc);
                Ok(())
            }
            priv12::SRET => {
                self.sret();
                Ok(())
            }
            priv12::WFI => {
                // wfi retires and advances past itself; if nothing is pending, the
                // hart parks (Idle) at the next instruction until an interrupt
                // arrives — modelling a real hart's halt so the driver can skip the
                // idle wait instead of emulating every idle-loop instruction.
                self.advance();
                if self.idle_skip && self.pending_interrupt().is_none() {
                    self.state = HartState::Idle;
                }
                Ok(())
            }
            _ => Err(self.unimplemented(instr.0)),
        }
    }

    /// Move the program counter to the next sequential instruction.
    fn advance(&mut self) {
        self.pc = self.pc.wrapping_add(self.cur_ilen);
    }

    /// Read a CSR that the trap machinery is guaranteed to model.
    fn csr_read(&self, addr: u16) -> u64 {
        // The S-mode trap CSRs are always in the supported set, so this read
        // cannot fail.
        self.csr.read(addr).expect("modeled trap CSR")
    }

    fn csr_write(&mut self, addr: u16, value: u64) {
        self.csr.write(addr, value).expect("modeled trap CSR");
    }

    /// The highest-priority deliverable supervisor interrupt, if any. RISC-V
    /// orders software above timer; both sit below external (which snemu has no
    /// source for yet).
    fn pending_interrupt(&self) -> Option<u64> {
        if self.software_interrupt_pending() {
            return Some(cause::INTERRUPT | cause::SUPERVISOR_SOFTWARE);
        }
        if self.timer_interrupt_pending() {
            return Some(cause::INTERRUPT | cause::SUPERVISOR_TIMER);
        }
        None
    }

    /// Whether a supervisor software interrupt (an IPI) is pending and currently
    /// deliverable: `sip.SSIP` raised, `sie.SSIE` set, and the privilege gate met.
    fn software_interrupt_pending(&self) -> bool {
        if self.csr_read(addr::SIP) & SIP_SSIP == 0 {
            return false;
        }
        if self.csr_read(addr::SIE) & SIE_SSIE == 0 {
            return false;
        }
        match self.privilege {
            Privilege::User => true,
            Privilege::Supervisor => self.csr_read(addr::SSTATUS) & sstatus::SIE != 0,
        }
    }

    /// Capture an S-mode `ecall`'s SBI arguments (`a7`=EID, `a6`=FID, `a0..a2`)
    /// and advance past it. The driver services the request after `step` returns
    /// and writes `a0`/`a1` back; S-mode execution then continues (no trap).
    fn capture_sbi_call(&mut self) {
        self.pending_sbi = Some(SbiRequest {
            eid: self.x[17],
            fid: self.x[16],
            arg0: self.x[10],
            arg1: self.x[11],
            arg2: self.x[12],
        });
        self.advance();
    }

    /// The clock value at which this hart's armed timer would wake it — `stimecmp`
    /// iff the timer is deliverable once the clock reaches it (`sie.STIE` set and
    /// the `sstatus.SIE`/privilege gate met), else `None`. Same gate as
    /// [`timer_interrupt_pending`](Self::timer_interrupt_pending) minus the
    /// `cycle >= stimecmp` check — it's the *future* deadline, so the driver can
    /// fast-forward an idle clock straight to it. `None` means only an IPI can
    /// wake this hart (impossible while every hart idles, so nothing to jump to).
    pub(crate) fn wake_deadline(&self) -> Option<u64> {
        if self.csr_read(addr::SIE) & SIE_STIE == 0 {
            return None;
        }
        let armed = match self.privilege {
            Privilege::User => true,
            Privilege::Supervisor => self.csr_read(addr::SSTATUS) & sstatus::SIE != 0,
        };
        if !armed {
            return None;
        }
        // An unset / explicitly-disarmed `stimecmp` (`u64::MAX`) never fires —
        // treat it as no deadline so the driver doesn't jump the clock to infinity.
        self.csr.read(addr::STIMECMP).ok().filter(|&t| t != u64::MAX)
    }

    /// Whether a supervisor timer interrupt is pending and currently deliverable.
    /// Sstc raises it once `time` (the retired-instruction clock) reaches
    /// `stimecmp`; it's taken only when `sie.STIE` is set and either we're in
    /// U-mode (lower privilege never masks an S-interrupt) or in S-mode with the
    /// global `sstatus.SIE` enabled.
    fn timer_interrupt_pending(&self) -> bool {
        let stimecmp = self.csr.read(addr::STIMECMP).unwrap_or(u64::MAX);
        if self.cycle < stimecmp {
            return false;
        }
        if self.csr_read(addr::SIE) & SIE_STIE == 0 {
            return false;
        }
        match self.privilege {
            Privilege::User => true,
            Privilege::Supervisor => self.csr_read(addr::SSTATUS) & sstatus::SIE != 0,
        }
    }

    /// Enter the S-mode trap handler: record the cause, save and mask the
    /// interrupt-enable state, record the interrupted privilege, and jump to
    /// `stvec` (direct mode).
    fn take_trap(&mut self, cause: u64, tval: u64) {
        let sie = self.csr_read(addr::SSTATUS) & sstatus::SIE != 0;
        let from_supervisor = self.privilege == Privilege::Supervisor;
        let mut status = self.csr_read(addr::SSTATUS);
        status = with_bit(status, sstatus::SPIE, sie); // SPIE <- SIE
        status = with_bit(status, sstatus::SIE, false); // SIE <- 0
        status = with_bit(status, sstatus::SPP, from_supervisor); // SPP <- prev mode
        self.csr_write(addr::SSTATUS, status);

        self.csr_write(addr::SEPC, self.pc);
        self.csr_write(addr::SCAUSE, cause);
        self.csr_write(addr::STVAL, tval);
        self.privilege = Privilege::Supervisor;
        self.pc = self.csr_read(addr::STVEC) & !0b11; // direct mode; ignore mode bits
    }

    /// Return from an S-mode trap: restore the interrupt-enable and privilege
    /// from the `SPIE`/`SPP` fields and resume at `sepc`.
    fn sret(&mut self) {
        let spie = self.csr_read(addr::SSTATUS) & sstatus::SPIE != 0;
        let to_supervisor = self.csr_read(addr::SSTATUS) & sstatus::SPP != 0;
        let mut status = self.csr_read(addr::SSTATUS);
        status = with_bit(status, sstatus::SIE, spie); // SIE <- SPIE
        status = with_bit(status, sstatus::SPIE, true); // SPIE <- 1
        status = with_bit(status, sstatus::SPP, false); // SPP <- U
        self.csr_write(addr::SSTATUS, status);

        self.privilege = if to_supervisor {
            Privilege::Supervisor
        } else {
            Privilege::User
        };
        self.pc = self.csr_read(addr::SEPC);
    }

    fn unimplemented(&self, instr: u32) -> StepError {
        StepError::Unimplemented {
            pc: self.pc,
            instr,
        }
    }
}

/// Service an SBI request from hart `caller` against the whole hart set (snemu
/// plays firmware). `send_ipi` and `hart_start` reach harts other than the
/// caller, so this runs at the driver level, not inside `Hart::step`. The result
/// (`a0` = error, `a1` = value) is written back into the caller.
pub(crate) fn service_sbi(harts: &mut [Hart], caller: usize, req: &SbiRequest) {
    let (error, value) = match (req.eid, req.fid) {
        (sbi::EID_IPI, sbi::FID_SEND_IPI) => {
            send_ipi(harts, req.arg0, req.arg1);
            (sbi::SUCCESS, 0)
        }
        (sbi::EID_HSM, sbi::FID_HART_START) => hart_start(harts, req.arg0, req.arg1, req.arg2),
        _ => (sbi::ERR_NOT_SUPPORTED, 0),
    };
    harts[caller].set_reg(10, error as u64);
    harts[caller].set_reg(11, value);
}

/// Raise `sip.SSIP` on every hart the mask selects. Hart `i` has mhartid `i`
/// here, and bit `k` of `hart_mask` targets hart `hart_mask_base + k`.
fn send_ipi(harts: &mut [Hart], hart_mask: u64, hart_mask_base: u64) {
    for id in 0..harts.len() as u64 {
        if id >= hart_mask_base && (hart_mask >> (id - hart_mask_base)) & 1 != 0 {
            harts[id as usize].raise_software_interrupt();
        }
    }
}

/// Wake the target hart at `start_addr` (physical, MMU off) with `a0 = hartid`,
/// `a1 = opaque`. Errors if the hart id is unknown or already running.
fn hart_start(harts: &mut [Hart], hartid: u64, start_addr: u64, opaque: u64) -> (i64, u64) {
    match harts.get_mut(hartid as usize) {
        None => (sbi::ERR_INVALID_PARAM, 0),
        Some(h) if h.is_running() => (sbi::ERR_ALREADY_AVAILABLE, 0),
        Some(h) => {
            h.start(start_addr, hartid, opaque);
            (sbi::SUCCESS, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::{addr, sstatus};
    use crate::decode::{ALT_OP_BIT, funct3, funct7, opcode, priv12, system};
    use crate::mem::{Memory, RAM_BASE};
    use crate::mmu::pte;

    fn priv_instr(funct12: u32) -> u32 {
        (funct12 << 20) | (system::PRIV << 12) | opcode::SYSTEM
    }
    fn ecall() -> u32 {
        priv_instr(priv12::ECALL)
    }
    fn ebreak() -> u32 {
        priv_instr(priv12::EBREAK)
    }
    fn sret() -> u32 {
        priv_instr(priv12::SRET)
    }
    fn wfi() -> u32 {
        priv_instr(priv12::WFI)
    }

    fn fence() -> u32 {
        opcode::MISC_MEM // funct3 = 0
    }
    fn fence_i() -> u32 {
        (1 << 12) | opcode::MISC_MEM // funct3 = 1
    }

    /// Encode a `c.addi rd, imm` (CI format, quadrant 01, funct3 000).
    fn c_addi(rd: u32, imm: i32) -> u16 {
        let imm = imm as u32;
        let w = (((imm >> 5) & 1) << 12) | (rd << 7) | ((imm & 0x1f) << 2) | 0b01;
        w as u16
    }
    fn c_li(rd: u32, imm: i32) -> u16 {
        let imm = imm as u32;
        let w = (0b010 << 13) | (((imm >> 5) & 1) << 12) | (rd << 7) | ((imm & 0x1f) << 2) | 0b01;
        w as u16
    }
    /// Encode a CR-format instruction (funct4 in bits 15:12, quadrant 10).
    fn cr(funct4: u32, rd: u32, rs2: u32) -> u16 {
        ((funct4 << 12) | (rd << 7) | (rs2 << 2) | 0b10) as u16
    }
    fn c_mv(rd: u32, rs2: u32) -> u16 {
        cr(0b1000, rd, rs2)
    }
    fn c_add(rd: u32, rs2: u32) -> u16 {
        cr(0b1001, rd, rs2)
    }
    fn c_jr(rs1: u32) -> u16 {
        cr(0b1000, rs1, 0)
    }
    fn c_jalr(rs1: u32) -> u16 {
        cr(0b1001, rs1, 0)
    }

    fn csr_reg(funct3: u32, rd: u32, rs1: u32, csr: u16) -> u32 {
        (u32::from(csr) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode::SYSTEM
    }
    fn csr_imm(funct3: u32, rd: u32, uimm: u32, csr: u16) -> u32 {
        (u32::from(csr) << 20) | (uimm << 15) | (funct3 << 12) | (rd << 7) | opcode::SYSTEM
    }
    fn csrrw(rd: u32, rs1: u32, csr: u16) -> u32 {
        csr_reg(system::CSRRW, rd, rs1, csr)
    }
    fn csrrs(rd: u32, rs1: u32, csr: u16) -> u32 {
        csr_reg(system::CSRRS, rd, rs1, csr)
    }
    fn csrrc(rd: u32, rs1: u32, csr: u16) -> u32 {
        csr_reg(system::CSRRC, rd, rs1, csr)
    }
    fn csrrwi(rd: u32, uimm: u32, csr: u16) -> u32 {
        csr_imm(system::CSRRWI, rd, uimm, csr)
    }
    fn csrrsi(rd: u32, uimm: u32, csr: u16) -> u32 {
        csr_imm(system::CSRRSI, rd, uimm, csr)
    }
    fn csrrci(rd: u32, uimm: u32, csr: u16) -> u32 {
        csr_imm(system::CSRRCI, rd, uimm, csr)
    }

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
    fn a_compiled_block_matches_the_interpreter() {
        // The block JIT's oracle property, proven per block: compiling a straight run
        // to IR and executing it must land the exact architectural state — every
        // register and pc — that interpreting the same instructions one-by-one does.
        let program = &[
            0x0050_0093, // addi x1, x0, 5
            0x0030_8113, // addi x2, x1, 3
            0x0020_81b3, // add  x3, x1, x2
            0x0020_9463, // bne  x1, x2, +8   (taken: 5 != 8)
        ];

        // Interpreter oracle: step the four instructions one at a time.
        let mut interp = cpu_with(program);
        for _ in 0..4 {
            interp.step().unwrap();
        }

        // Block JIT: compile the block from the entry PC and execute it in one shot.
        let mut jit = cpu_with(program);
        let block = jit.hart.compile_block(&jit.bus);
        block.exec(&mut jit.hart, &mut jit.bus).unwrap();

        assert_eq!(jit.hart.x, interp.hart.x, "all registers match the interpreter");
        assert_eq!(jit.hart.pc(), interp.hart.pc(), "pc matches the interpreter");
        assert_eq!(jit.hart.pc(), RAM_BASE + 0xc + 8, "the bne was taken to pc+8");
    }

    /// Backend B's oracle at the Cpu level: running a block through the **native**
    /// executor must land the exact register/pc state the interpreter does. The block
    /// (addi/addi/add/bne) is all-emittable, so native genuinely engages (asserted).
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn native_backend_matches_the_interpreter() {
        let program = &[
            0x0050_0093, // addi x1, x0, 5
            0x0030_8113, // addi x2, x1, 3
            0x0020_81b3, // add  x3, x1, x2
            0x0020_9463, // bne  x1, x2, +8   (taken)
        ];
        let mut interp = cpu_with(program);
        for _ in 0..4 {
            interp.step().unwrap();
        }

        let mut jit = cpu_with(program);
        jit.hart.set_native_jit(true);
        let block = jit.hart.compile_block(&jit.bus);
        let mut arena = crate::jit::CodeArena::new();
        assert!(
            crate::jit::NativeBlock::compile_into(block.ops(), block.exit_pc(), &mut arena).is_some(),
            "this block is natively compilable — native, not Backend A, runs it",
        );
        let retired = jit.hart.run_block(&block, &mut jit.bus).unwrap();

        assert_eq!(retired, 4, "every op retired");
        assert_eq!(jit.hart.x, interp.hart.x, "native registers match the interpreter");
        assert_eq!(jit.hart.pc(), interp.hart.pc(), "native pc matches the interpreter");
    }

    /// Compile a straight run to IR, execute it once, and assert every register and
    /// pc match interpreting the same `program` instruction-by-instruction.
    fn assert_block_matches_interp(program: &[u32], setup: impl Fn(&mut Cpu)) {
        let mut interp = cpu_with(program);
        setup(&mut interp);
        for _ in 0..program.len() {
            interp.step().unwrap();
        }
        let mut jit = cpu_with(program);
        setup(&mut jit);
        let block = jit.hart.compile_block(&jit.bus);
        block.exec(&mut jit.hart, &mut jit.bus).unwrap();
        assert_eq!(jit.hart.x, interp.hart.x, "registers diverged from the interpreter");
        assert_eq!(jit.hart.pc(), interp.hart.pc(), "pc diverged from the interpreter");
    }

    #[test]
    fn block_jit_covers_the_alu_families() {
        // A medley of every integer ALU family — reg-reg, reg-imm, and their 32-bit
        // `.w` forms, plus LUI/AUIPC — ending in a branch. Compiling the block must
        // reproduce the interpreter exactly (the per-op oracle, all families at once).
        let program = &[
            add(3, 1, 2),
            sub(4, 1, 2),
            sll(5, 1, 2),
            slt(6, 1, 2),
            sltu(7, 1, 2),
            xor(8, 1, 2),
            srl(9, 1, 2),
            sra(10, 1, 2),
            or(11, 1, 2),
            and(12, 1, 2),
            i_type(opcode::OP_IMM, funct3::ADD, 13, 1, -3), // addi
            slli(14, 1, 4),
            srli(15, 1, 4),
            srai(16, 1, 4),
            andi(17, 1, 0xF),
            slti(18, 1, 100),
            sltiu(19, 1, 100),
            xori(20, 1, 0x55),
            addiw(21, 1, 7),
            slliw(22, 1, 3),
            sraiw(23, 1, 2),
            addw(24, 1, 2),
            subw(25, 1, 2),
            sllw(26, 1, 2),
            sraw(27, 1, 2),
            srlw(28, 1, 2),
            u_type(opcode::LUI, 29, 0xABCDE),
            u_type(opcode::AUIPC, 30, 0x10),
            bne(1, 2, 8),
        ];
        assert_block_matches_interp(program, |cpu| {
            cpu.set_reg(1, 0x1_2345_6789);
            cpu.set_reg(2, 5);
        });
    }

    #[test]
    fn block_jit_reports_the_faulting_instructions_pc_mid_block() {
        // A block whose *second* op (a store) page-faults. The trap's `sepc` must
        // point at the store, not the block entry — the interpreter sets pc per
        // instruction, so the JIT (which advances pc once per block) must set it on
        // the faulting op or it resumes at the wrong place.
        let mut mem = Memory::new(0x10000);
        let code = RAM_BASE + 0x3000;
        mem.write_u32(code, i_type(opcode::OP_IMM, funct3::ADD, 5, 0, 1)).unwrap(); // op1: addi, no fault
        mem.write_u32(code + 4, sw(7, 6, 0)).unwrap(); // op2: store to x6 (unmapped) → faults
        // Sv39: one 1 GiB leaf maps VA 4..5 GiB (VPN[2]=4) onto physical RAM, RWX.
        let root = RAM_BASE + 0x8000;
        let leaf = ((0x8000_0000_u64 >> 12) << 10) | pte::V | pte::R | pte::W | pte::X;
        mem.write_u64(root + 4 * 8, leaf).unwrap();

        let mut cpu = Cpu::new(mem);
        cpu.hart.csr.write(addr::SATP, (8 << 60) | (root >> 12)).unwrap();
        cpu.hart.csr.write(addr::STVEC, 0x1_0000_3000).unwrap();
        cpu.set_pc(0x1_0000_3000); // VPN[2]=4, offset 0x3000
        cpu.set_reg(6, 0x1_4000_0000); // store target in VPN[2]=5 — unmapped → faults

        // Compile + run the block directly (bypassing the hotness gate — this block
        // faults on its one and only run, so it never gets hot on its own).
        let block = cpu.hart.compile_block(&cpu.bus);
        block.exec(&mut cpu.hart, &mut cpu.bus).unwrap();

        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), 15, "store page fault");
        assert_eq!(
            cpu.hart.csr.read(addr::SEPC).unwrap(),
            0x1_0000_3004,
            "sepc points at the faulting store, not the block entry"
        );
        assert_eq!(cpu.hart.csr.read(addr::STVAL).unwrap(), 0x1_4000_0000, "faulting VA");
    }

    #[test]
    fn block_jit_covers_jumps() {
        // JAL: link pc+4 into rd and jump to a compile-time target (the block ends).
        assert_block_matches_interp(
            &[
                i_type(opcode::OP_IMM, funct3::ADD, 5, 0, 7), // addi x5, x0, 7
                jal(1, 16),                                   // jal x1, +16
            ],
            |_| {},
        );
        // JALR: link pc+4, jump to the runtime target (x2 + imm) & !1.
        assert_block_matches_interp(&[jalr(1, 2, 4)], |cpu| {
            cpu.set_reg(2, RAM_BASE + 0x40);
        });
    }

    #[test]
    fn block_jit_covers_loads_and_stores() {
        // Build a RAM pointer, store a value through it, load it back, and branch on
        // the result — all inside one block. Memory ops run mid-block (they can fault
        // and bail, but here they succeed) and the block must match the interpreter.
        let program = &[
            u_type(opcode::AUIPC, 1, 0), // auipc x1, 0  -> x1 = RAM_BASE (a valid RAM ptr)
            i_type(opcode::OP_IMM, funct3::ADD, 1, 1, 0x400), // addi x1, x1, 0x400
            sw(2, 1, 0),  // sw x2, 0(x1)
            lw(3, 1, 0),  // lw x3, 0(x1)
            ld(4, 1, 0),  // ld x4, 0(x1)  (a second width)
            bne(3, 2, 8), // not taken: x3 == x2
        ];
        assert_block_matches_interp(program, |cpu| {
            cpu.set_reg(2, 0x1234_5678);
        });
    }

    #[test]
    fn block_jit_covers_all_branch_conditions() {
        // Every branch condition, taken and not-taken: the compiled terminator must
        // resolve pc to the same place the interpreter does.
        let conds: &[fn(u32, u32, i32) -> u32] = &[beq, bne, blt, bge, bltu, bgeu];
        let cases: &[(u64, u64)] = &[(5, 5), (5, 8), (8, 5), (u64::MAX, 1)];
        for &enc in conds {
            for &(a, b) in cases {
                assert_block_matches_interp(&[enc(1, 2, 8)], |cpu| {
                    cpu.set_reg(1, a);
                    cpu.set_reg(2, b);
                });
            }
        }
    }

    #[test]
    fn the_block_jit_changes_nothing_but_speed() {
        // A backward-branch loop forms a hot block that re-executes. Running to the
        // loop's exit (a fixed guest state, not a fixed step count — a JIT step
        // retires a whole block) with the JIT off and on must yield byte-identical
        // architectural state: instret, pc, every register. The oracle property.
        let program = &[
            0x0010_8093, // addi x1, x1, 1
            0xfe20_9ee3, // bne  x1, x2, -4   (loop back to the addi while x1 != x2)
        ];
        let exit_pc = RAM_BASE + 8; // fall-through past the branch when x1 == x2
        let run = |jit: bool| {
            let mut cpu = cpu_with(program);
            cpu.set_reg(2, 10); // loop ten times
            cpu.set_block_jit(jit);
            for _ in 0..1000 {
                if cpu.pc() == exit_pc {
                    break;
                }
                cpu.step().unwrap();
            }
            (cpu.instret(), cpu.pc(), cpu.reg(1), cpu.reg(2))
        };
        let off = run(false);
        let on = run(true);
        assert_eq!(on, off, "block JIT ON must equal the interpreter OFF");
        assert_eq!(on.1, exit_pc, "the loop actually exited");
        assert_eq!(on.2, 10, "x1 counted up to x2");

        // ...and the fast path engaged — the hot block re-executed from the cache.
        let mut cpu = cpu_with(program);
        cpu.set_reg(2, 10);
        cpu.set_block_jit(true);
        for _ in 0..1000 {
            if cpu.pc() == exit_pc {
                break;
            }
            cpu.step().unwrap();
        }
        assert!(cpu.hart.block_jit_hits() > 0, "the loop's block should hit the cache");
    }

    #[test]
    fn the_decode_cache_changes_nothing_but_speed() {
        // A tiny loop that re-executes the same PCs, so the cache takes hits:
        // `addi x1,x1,1` once, then `jal x0,0` spinning on itself. Running it with
        // the cache OFF and ON must yield byte-identical architectural state —
        // instret, pc, and the register — proving the cache is a pure speedup.
        let program = &[0x0010_8093, 0x0000_006f]; // addi x1,x1,1 ; jal x0,0
        let run = |cache: bool| {
            let mut cpu = cpu_with(program);
            cpu.set_decode_cache(cache);
            for _ in 0..8 {
                cpu.step().unwrap();
            }
            (cpu.instret(), cpu.pc(), cpu.reg(1))
        };
        let off = run(false);
        let on = run(true);
        assert_eq!(on, off, "cache ON must equal cache OFF");
        // And the fast path actually engaged (the jal re-executed).
        let mut cpu = cpu_with(program);
        cpu.set_decode_cache(true);
        for _ in 0..8 {
            cpu.step().unwrap();
        }
        assert!(cpu.decode_cache_hits() > 0, "the loop should hit the cache");
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
    fn csr_instructions_read_modify_write() {
        let s = addr::SSCRATCH;
        let program = &[
            addi(1, 0, 0x12),    // x1 = 0x12
            csrrw(2, 1, s),      // x2 = old(0); sscratch = 0x12
            csrrs(3, 0, s),      // x3 = 0x12 (read; rs1=x0 -> no write)
            addi(4, 0, 0x01),    // x4 = 1
            csrrs(5, 4, s),      // x5 = 0x12 (old); sscratch = 0x13
            addi(6, 0, 0x02),    // x6 = 2
            csrrc(7, 6, s),      // x7 = 0x13 (old); sscratch = 0x11
            csrrwi(8, 0x1f, s),  // x8 = 0x11 (old); sscratch = 0x1f
            csrrsi(9, 0, s),     // x9 = 0x1f (read; uimm=0 -> no write)
            csrrci(10, 0x0f, s), // x10 = 0x1f (old); sscratch = 0x10
            csrrsi(11, 0x04, s), // x11 = 0x10 (old); sscratch = 0x14
            csrrs(12, 0, s),     // x12 = 0x14 (final read)
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(2), 0);
        assert_eq!(cpu.reg(3), 0x12);
        assert_eq!(cpu.reg(5), 0x12);
        assert_eq!(cpu.reg(7), 0x13);
        assert_eq!(cpu.reg(8), 0x11);
        assert_eq!(cpu.reg(9), 0x1f);
        assert_eq!(cpu.reg(10), 0x1f);
        assert_eq!(cpu.reg(11), 0x10);
        assert_eq!(cpu.reg(12), 0x14);
    }

    #[test]
    fn csr_access_to_unmodeled_register_reports_unknown() {
        let mut cpu = cpu_with(&[csrrw(1, 0, 0xbc0)]); // 0xbc0 not modeled
        assert_eq!(
            cpu.step(),
            Err(StepError::UnknownCsr {
                pc: RAM_BASE,
                addr: 0xbc0,
            })
        );
    }

    #[test]
    fn s_mode_ecall_is_serviced_as_sbi_not_trapped() {
        // snemu plays the firmware: an S-mode ecall is an SBI call, not a trap to
        // the kernel's own handler. An unknown EID returns SBI_ERR_NOT_SUPPORTED
        // (-2) in a0 and execution continues past the ecall.
        let mut cpu = cpu_with(&[ecall(), addi(1, 0, 7)]);
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x200).unwrap();
        cpu.set_reg(17, 0xdead); // a7 = unrecognized EID
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4); // advanced; did NOT trap to stvec
        assert_eq!(cpu.reg(10) as i64, -2); // a0 = SBI_ERR_NOT_SUPPORTED
    }

    #[test]
    fn sbi_send_ipi_raises_a_software_interrupt_for_this_hart() {
        let mut cpu = cpu_with(&[ecall()]);
        cpu.set_reg(17, 0x735049); // a7 = EID "sPI" (send_ipi extension)
        cpu.set_reg(16, 0); // a6 = FID 0
        cpu.set_reg(10, 1); // a0 = hart_mask, bit 0 -> hart 0 (us)
        cpu.set_reg(11, 0); // a1 = hart_mask_base
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0); // a0 = SBI_SUCCESS
        assert_ne!(cpu.hart.csr.read(addr::SIP).unwrap() & (1 << 1), 0); // SSIP raised
    }

    #[test]
    fn send_ipi_targets_the_selected_hart_not_the_others() {
        // The cross-hart IPI: hart 0 sends to hart 1 (bit 1 of the mask). Only
        // hart 1's SSIP is raised.
        let mut harts = vec![Hart::new(), Hart::new()];
        send_ipi(&mut harts, 1 << 1, 0);
        assert_eq!(harts[0].csr_read(addr::SIP) & SIP_SSIP, 0);
        assert_ne!(harts[1].csr_read(addr::SIP) & SIP_SSIP, 0);
    }

    #[test]
    fn pending_software_interrupt_traps_to_the_handler() {
        let mut cpu = cpu_with(&[addi(1, 0, 7)]);
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x200).unwrap();
        cpu.hart.csr.write(addr::SIP, 1 << 1).unwrap(); // SSIP pending
        cpu.hart.csr.write(addr::SIE, 1 << 1).unwrap(); // SSIE enabled
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200);
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), (1 << 63) | 1); // software int
    }

    #[test]
    fn ebreak_traps_with_the_breakpoint_cause() {
        let mut cpu = cpu_with(&[ebreak()]);
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x200).unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200);
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), 3); // breakpoint
    }

    /// sie.STIE — supervisor timer interrupt enable (bit 5).
    const STIE: u64 = 1 << 5;

    #[test]
    fn timer_interrupt_fires_when_time_reaches_stimecmp() {
        // jal x0, 0 — a self-loop, so without the timer the cpu would spin here.
        let mut cpu = cpu_with(&[jal(0, 0)]);
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x200).unwrap();
        cpu.hart.csr.write(addr::STIMECMP, 0).unwrap(); // deadline 0; time >= 0 at once
        cpu.hart.csr.write(addr::SIE, STIE).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200); // trapped to stvec
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), (1 << 63) | 5); // timer interrupt
        assert_eq!(cpu.hart.csr.read(addr::SEPC).unwrap(), RAM_BASE); // resume the un-run instr
    }

    #[test]
    fn timer_interrupt_is_masked_when_sstatus_sie_clear() {
        let mut cpu = cpu_with(&[addi(1, 0, 7)]);
        cpu.hart.csr.write(addr::STIMECMP, 0).unwrap();
        cpu.hart.csr.write(addr::SIE, STIE).unwrap();
        // sstatus.SIE left clear: in S-mode the interrupt stays pending, not taken.
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 7); // the instruction ran instead of trapping
        assert_eq!(cpu.pc(), RAM_BASE + 4);
    }

    #[test]
    fn timer_interrupt_needs_the_per_source_enable() {
        let mut cpu = cpu_with(&[addi(1, 0, 7)]);
        cpu.hart.csr.write(addr::STIMECMP, 0).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();
        // sie.STIE left clear: the global enable alone doesn't deliver it.
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 7);
    }

    #[test]
    fn timer_interrupt_waits_for_the_deadline() {
        let mut cpu = cpu_with(&[addi(1, 0, 7), addi(2, 0, 9)]);
        cpu.hart.csr.write(addr::STIMECMP, 5).unwrap(); // five ticks out
        cpu.hart.csr.write(addr::SIE, STIE).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();
        cpu.step().unwrap(); // instret 0 < 5: runs the instruction, no trap
        assert_eq!(cpu.reg(1), 7);
        assert_eq!(cpu.pc(), RAM_BASE + 4);
    }

    #[test]
    fn sret_instruction_returns_to_sepc() {
        let mut cpu = cpu_with(&[sret()]);
        cpu.hart.csr.write(addr::SEPC, RAM_BASE + 0x40).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SPIE).unwrap(); // SPP=U, SPIE=1
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x40);
        assert_eq!(cpu.privilege(), Privilege::User);
    }

    #[test]
    fn wfi_is_a_nop_that_advances() {
        let mut cpu = cpu_with(&[wfi()]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4);
    }

    #[test]
    fn wfi_with_idle_skip_disabled_is_a_bare_nop_that_never_parks() {
        // The A/B baseline: with idle-skip off, wfi advances but leaves the hart
        // Running (the pre-fidelity behaviour), so no fast-forward ever happens.
        let mut cpu = cpu_with(&[wfi()]);
        cpu.set_idle_skip(false);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4);
        assert!(cpu.hart.is_running(), "idle-skip off: wfi does not park the hart");
    }

    #[test]
    fn wfi_parks_the_hart_when_no_interrupt_is_pending() {
        // Real hardware halts on wfi until an interrupt is pending. With no timer
        // armed, the hart parks (Idle) rather than spinning the idle loop — the
        // fidelity fix that lets the machine fast-forward idle time.
        let mut cpu = cpu_with(&[wfi()]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4, "wfi still retires and advances PC");
        assert!(!cpu.hart.is_running(), "wfi with nothing pending parks the hart");
    }

    #[test]
    fn an_idle_hart_fast_forwards_the_clock_to_its_timer_deadline() {
        // wfi parks the hart at the following instruction; with a timer armed 1000
        // ticks out, the machine must jump the clock straight to the deadline and
        // deliver the timer — not grind 1000 idle steps to get there.
        let mut cpu = cpu_with(&[wfi(), jal(0, 0)]);
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x200).unwrap();
        cpu.hart.csr.write(addr::STIMECMP, 1000).unwrap();
        cpu.hart.csr.write(addr::SIE, STIE).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap();

        cpu.step().unwrap(); // wfi: time 0 < 1000, nothing pending → park
        assert!(!cpu.hart.is_running());
        cpu.step().unwrap(); // parked, idle → jump clock to the deadline
        cpu.step().unwrap(); // deliver the timer trap

        // Delivered a timer 1000 ticks out in a *fixed* 3 step() calls — proof the
        // driver jumped the clock rather than grinding 1000 idle steps to reach it
        // (nop-wfi would still be looping at RAM_BASE+4 here).
        assert_eq!(cpu.pc(), RAM_BASE + 0x200, "trapped to stvec");
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), (1 << 63) | 5);
        assert_eq!(cpu.instret(), 1000, "clock fast-forwarded to the deadline");
    }

    #[test]
    fn fence_instructions_are_noops() {
        let mut cpu = cpu_with(&[fence(), fence_i()]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 8);
    }

    #[test]
    fn take_trap_enters_the_supervisor_handler() {
        const HANDLER: u64 = RAM_BASE + 0x100;
        const TRAP_PC: u64 = RAM_BASE + 0x40;
        const ILLEGAL_INSTRUCTION: u64 = 2;
        let mut cpu = Cpu::new(Memory::new(0x1000));
        cpu.hart.csr.write(addr::STVEC, HANDLER).unwrap();
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SIE).unwrap(); // interrupts enabled
        cpu.set_pc(TRAP_PC);

        cpu.hart.take_trap(ILLEGAL_INSTRUCTION, 0xbad);

        assert_eq!(cpu.pc(), HANDLER);
        assert_eq!(cpu.hart.csr.read(addr::SEPC).unwrap(), TRAP_PC);
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), ILLEGAL_INSTRUCTION);
        assert_eq!(cpu.hart.csr.read(addr::STVAL).unwrap(), 0xbad);
        let s = cpu.hart.csr.read(addr::SSTATUS).unwrap();
        assert_eq!(s & sstatus::SIE, 0, "SIE cleared on trap");
        assert_ne!(s & sstatus::SPIE, 0, "SPIE holds prior SIE");
        assert_ne!(s & sstatus::SPP, 0, "SPP records the interrupted S-mode");
        assert_eq!(cpu.privilege(), Privilege::Supervisor);
    }

    #[test]
    fn sret_restores_state_and_returns() {
        const RETURN_PC: u64 = RAM_BASE + 0x80;
        let mut cpu = Cpu::new(Memory::new(0x1000));
        cpu.hart.csr.write(addr::SEPC, RETURN_PC).unwrap();
        // Mid-trap state: SPIE=1, SPP=0 (trapped from U-mode), SIE=0.
        cpu.hart.csr.write(addr::SSTATUS, sstatus::SPIE).unwrap();

        cpu.hart.sret();

        assert_eq!(cpu.pc(), RETURN_PC);
        assert_eq!(cpu.privilege(), Privilege::User); // SPP was U
        let s = cpu.hart.csr.read(addr::SSTATUS).unwrap();
        assert_ne!(s & sstatus::SIE, 0, "SIE restored from SPIE");
        assert_ne!(s & sstatus::SPIE, 0, "SPIE set to 1");
        assert_eq!(s & sstatus::SPP, 0, "SPP cleared to U");
    }

    #[test]
    fn compressed_addi_executes_and_advances_by_two() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, c_addi(1, 5)).unwrap(); // c.addi x1, 5
        let mut cpu = Cpu::new(mem);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 5); // x1 = x1 + 5
        assert_eq!(cpu.pc(), RAM_BASE + 2); // compressed -> advance by 2
    }

    #[test]
    fn compressed_li_and_cr_arithmetic() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, c_li(1, -3)).unwrap(); // x1 = -3
        mem.write_u16(RAM_BASE + 2, c_mv(2, 1)).unwrap(); // x2 = x1
        mem.write_u16(RAM_BASE + 4, c_add(2, 1)).unwrap(); // x2 += x1
        let mut cpu = Cpu::new(mem);
        for _ in 0..3 {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(1), (-3_i64) as u64);
        assert_eq!(cpu.reg(2), (-6_i64) as u64);
        assert_eq!(cpu.pc(), RAM_BASE + 6); // three compressed instructions
    }

    #[test]
    fn compressed_jr_does_not_link() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, c_jr(5)).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(5, RAM_BASE + 0x40);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x40);
        assert_eq!(cpu.reg(1), 0);
    }

    #[test]
    fn compressed_jalr_links_with_compressed_length() {
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, c_jalr(5)).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(5, RAM_BASE + 0x40);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x40);
        assert_eq!(cpu.reg(1), RAM_BASE + 2); // link = pc + 2, not + 4
    }

    #[test]
    fn compressed_j_jumps_forward_and_backward() {
        // c.j +6 == 0xa019
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0xa019).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 6);

        // c.j -10 == 0xbfdd (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE + 0x40, 0xbfdd).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_pc(RAM_BASE + 0x40);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x40 - 10);
    }

    #[test]
    fn compressed_sdsp_stores_sp_relative() {
        // c.sdsp x11, 272(sp) == 0xea2e (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0xea2e).unwrap();
        mem.write_u32(RAM_BASE + 2, ld(5, 2, 272)).unwrap(); // ld x5, 272(x2)
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, RAM_BASE + 0x100); // sp
        cpu.set_reg(11, 0xdead_beef_cafe_babe);
        cpu.step().unwrap(); // c.sdsp
        cpu.step().unwrap(); // ld
        assert_eq!(cpu.reg(5), 0xdead_beef_cafe_babe);
    }

    #[test]
    fn compressed_addi4spn_computes_sp_offset() {
        // c.addi4spn x10, sp, 344 == 0xaa8 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0xaa8).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, 0x4000); // sp
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0x4000 + 344);
        assert_eq!(cpu.pc(), RAM_BASE + 2);
    }

    #[test]
    fn compressed_addi16sp_adjusts_sp() {
        // c.addi16sp sp, -176 == 0x7171 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x7171).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, 0x4000); // sp
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 0x4000 - 176);
    }

    #[test]
    fn compressed_ldsp_loads_sp_relative() {
        // c.ldsp x10, 16(sp) == 0x6542 (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0x6542).unwrap();
        mem.write_u64(RAM_BASE + 0x100 + 16, 0x1122_3344_5566_7788)
            .unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, RAM_BASE + 0x100); // sp
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0x1122_3344_5566_7788);
    }

    #[test]
    fn compressed_bnez_branches_when_nonzero() {
        // c.bnez x10, +206 == 0xe579 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0xe579).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 1);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 206); // taken

        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0xe579).unwrap();
        let mut cpu = Cpu::new(mem); // x10 == 0
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 2); // not taken
    }

    #[test]
    fn compressed_sd_stores_register_relative() {
        // c.sd x10, 0(x11) == 0xe188 (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0xe188).unwrap();
        mem.write_u32(RAM_BASE + 2, ld(5, 11, 0)).unwrap(); // ld x5, 0(x11)
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, RAM_BASE + 0x200); // base
        cpu.set_reg(10, 0xfeed_face_0000_1234);
        cpu.step().unwrap(); // c.sd
        cpu.step().unwrap(); // ld
        assert_eq!(cpu.reg(5), 0xfeed_face_0000_1234);
    }

    #[test]
    fn compressed_ld_loads_register_relative() {
        // c.ld x10, 0(x10) == 0x6108 (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0x6108).unwrap();
        mem.write_u64(RAM_BASE + 0x200, 0x0102_0304_0506_0708)
            .unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, RAM_BASE + 0x200);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0x0102_0304_0506_0708);
    }

    #[test]
    fn compressed_beqz_branches_when_zero() {
        // c.beqz x10, +18 == 0xc909 (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE + 0x200, 0xc909).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_pc(RAM_BASE + 0x200); // x10 == 0
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200 + 18); // taken

        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE + 0x200, 0xc909).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_pc(RAM_BASE + 0x200);
        cpu.set_reg(10, 1);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200 + 2); // not taken
    }

    #[test]
    fn compressed_and_combines_registers() {
        // c.and x10, x12 == 0x8d71 (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8d71).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0xff0f);
        cpu.set_reg(12, 0x0ff0);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xff0f & 0x0ff0);
    }

    #[test]
    fn compressed_sub_subtracts_registers() {
        // c.sub x11, x10 == 0x8d89 (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8d89).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, 100);
        cpu.set_reg(10, 30);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(11), 70);
    }

    #[test]
    fn compressed_srli_shifts_right_logical() {
        // c.srli x11, 2 == 0x8189 (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8189).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, 0xff);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(11), 0xff >> 2);
    }

    #[test]
    fn compressed_swsp_stores_word_sp_relative() {
        // c.swsp x10, 44(sp) == 0xd62a (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0xd62a).unwrap();
        mem.write_u32(RAM_BASE + 2, lw(5, 2, 44)).unwrap(); // lw x5, 44(x2)
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, RAM_BASE + 0x100); // sp
        cpu.set_reg(10, 0x0bcd_1234);
        cpu.step().unwrap(); // c.swsp
        cpu.step().unwrap(); // lw
        assert_eq!(cpu.reg(5), 0x0bcd_1234);
    }

    #[test]
    fn compressed_lwsp_loads_word_sp_relative() {
        // c.lwsp x10, 44(sp) == 0x5532 (captured from the minimal-boot kernel)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0x5532).unwrap();
        mem.write_u32(RAM_BASE + 0x100 + 44, 0x0011_2233).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(2, RAM_BASE + 0x100); // sp
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0x0011_2233);
    }

    #[test]
    fn executes_through_sv39_translation() {
        let mut mem = Memory::new(0x10000);
        // Instruction lives at physical RAM_BASE + 0x3000.
        mem.write_u32(RAM_BASE + 0x3000, addi(1, 0, 42)).unwrap();
        // Root page table at RAM_BASE + 0x8000; a 1 GiB leaf for VPN[2]=4 maps
        // the whole 4..5 GiB VA range onto physical 0x8000_0000.
        let root = RAM_BASE + 0x8000;
        let leaf = ((0x8000_0000_u64 >> 12) << 10) | pte::V | pte::R | pte::W | pte::X;
        mem.write_u64(root + 4 * 8, leaf).unwrap();

        let mut cpu = Cpu::new(mem);
        cpu.hart.csr.write(addr::SATP, (8 << 60) | (root >> 12)).unwrap();
        cpu.set_pc(0x1_0000_0000 | 0x3000); // VPN[2]=4, offset 0x3000

        cpu.step().unwrap();
        assert_eq!(cpu.reg(1), 42);
    }

    #[test]
    fn compressed_or_combines_registers() {
        // c.or x11, x12 == 0x8dd1 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8dd1).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, 0xf0);
        cpu.set_reg(12, 0x0f);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(11), 0xff);
    }

    #[test]
    fn compressed_slli_shifts_left() {
        // c.slli x10, 8 == 0x0522 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x0522).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0xab);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xab << 8);
    }

    #[test]
    fn compressed_andi_masks_register() {
        // c.andi x10, 1 == 0x8905 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8905).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0xff);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xff & 1);
    }

    #[test]
    fn compressed_lui_loads_upper_immediate() {
        // c.lui x14, 0x10 == 0x6741 (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x6741).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(14), 0x10000);
    }

    #[test]
    fn compressed_sw_stores_word_register_relative() {
        // c.sw x10, 0(x11) == 0xc188 (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0xc188).unwrap();
        mem.write_u32(RAM_BASE + 2, lw(5, 11, 0)).unwrap(); // lw x5, 0(x11)
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, RAM_BASE + 0x200);
        cpu.set_reg(10, 0x0bad_f00d);
        cpu.step().unwrap(); // c.sw
        cpu.step().unwrap(); // lw
        assert_eq!(cpu.reg(5), 0x0bad_f00d);
    }

    #[test]
    fn compressed_lw_loads_word_register_relative() {
        // c.lw x14, 0(x14) == 0x4318 (captured from the kernel boot)
        let mut mem = Memory::new(0x2000);
        mem.write_u16(RAM_BASE, 0x4318).unwrap();
        mem.write_u32(RAM_BASE + 0x200, 0x0102_0304).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(14, RAM_BASE + 0x200);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(14), 0x0102_0304);
    }

    #[test]
    fn compressed_addiw_sign_extends_word() {
        // c.addiw x10, 0 == 0x2501 (captured from the kernel boot; sext.w idiom)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x2501).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0x1_8000_0000);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xffff_ffff_8000_0000);
    }

    #[test]
    fn compressed_xor_combines_registers() {
        // c.xor x10, x11 == 0x8d2d (captured from the kernel boot)
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x8d2d).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0xff00);
        cpu.set_reg(11, 0x0ff0);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xff00 ^ 0x0ff0);
    }

    #[test]
    fn compressed_srai_arithmetic_shifts_register() {
        // c.srai x12, 32 == 0x9601 (captured from the kernel boot).
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x9601).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(12, 0xffff_ffff_0000_0000);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(12), 0xffff_ffff_ffff_ffff); // sign-propagating >> 32
    }

    #[test]
    fn compressed_addw_adds_words_and_sign_extends() {
        // c.addw x10, x11 == 0x9d2d (captured from the demo workload boot).
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x9d2d).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0x7fff_ffff);
        cpu.set_reg(11, 1);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), 0xffff_ffff_8000_0000); // sext32(0x7fffffff + 1)
    }

    #[test]
    fn compressed_subw_subtracts_words_and_sign_extends() {
        // c.subw x10, x11 == 0x9d0d (captured from the kernel boot).
        let mut mem = Memory::new(0x1000);
        mem.write_u16(RAM_BASE, 0x9d0d).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(10, 0);
        cpu.set_reg(11, 1);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(10), u64::MAX); // sext32(0 - 1) = -1
    }

    #[test]
    fn a_fetch_page_fault_traps_instead_of_halting() {
        // Sv39 on, root page table pointing at a zeroed page → every translation
        // (including the instruction fetch) faults. It must trap, not halt.
        let mut mem = Memory::new(0x4000);
        mem.write_u32(RAM_BASE, addi(1, 0, 1)).unwrap(); // never runs
        let mut cpu = Cpu::new(mem);
        cpu.set_pc(RAM_BASE);
        let root = RAM_BASE + 0x2000; // page-aligned, zeroed
        cpu.hart.csr.write(addr::SATP, (8u64 << 60) | (root >> 12)).unwrap();
        cpu.hart.csr.write(addr::STVEC, RAM_BASE + 0x300).unwrap();
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x300); // trapped to stvec, not halted
        assert_eq!(cpu.hart.csr.read(addr::SCAUSE).unwrap(), 12); // instruction page fault
        assert_eq!(cpu.hart.csr.read(addr::STVAL).unwrap(), RAM_BASE); // faulting VA
    }

    #[test]
    fn sfence_vma_is_a_nop() {
        // sfence.vma x0, x0 == 0x12000073 (no TLB in snemu)
        let mut cpu = cpu_with(&[0x1200_0073]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4);
    }

    #[test]
    fn store_to_uart_produces_console_output() {
        let program = &[
            lui(2, 0x10000),               // x2 = 0x1000_0000 (UART base)
            addi(1, 0, i32::from(b'X')),   // x1 = 'X'
            sb(1, 2, 0),                   // store 'X' to the UART THR
        ];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.uart_output(), b"X");
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

    /// Encode an AMO (opcode 0x2f): `funct5`, width (`2`=`.w`, `3`=`.d`),
    /// rd/rs1/rs2. aq/rl left zero — ordering is a no-op on a single hart.
    fn amo(funct5: u32, width: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
        (funct5 << 27) | (rs2 << 20) | (rs1 << 15) | (width << 12) | (rd << 7) | 0x2f
    }

    /// Run a single doubleword AMO against a seeded memory cell. Returns
    /// `(rd, memory)` after the op: rd=3 holds the old value, x4 reloads the cell.
    fn run_amo_d(funct5: u32, init: u64, rs2: u64) -> (u64, u64) {
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, amo(funct5, 3, 3, 1, 2)).unwrap();
        mem.write_u32(RAM_BASE + 4, ld(4, 1, 0)).unwrap();
        mem.write_u64(RAM_BASE + 0x200, init).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(1, RAM_BASE + 0x200);
        cpu.set_reg(2, rs2);
        cpu.step().unwrap(); // amo
        cpu.step().unwrap(); // ld back
        (cpu.reg(3), cpu.reg(4))
    }

    /// Run a single word AMO against a seeded 32-bit cell. Returns
    /// `(rd, memory)`: rd=3 is the old value (sign-extended), x4 reloads the cell.
    fn run_amo_w(funct5: u32, init: u32, rs2: u64) -> (u64, u32) {
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, amo(funct5, 2, 3, 1, 2)).unwrap();
        mem.write_u32(RAM_BASE + 4, lwu(4, 1, 0)).unwrap();
        mem.write_u32(RAM_BASE + 0x200, init).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(1, RAM_BASE + 0x200);
        cpu.set_reg(2, rs2);
        cpu.step().unwrap(); // amo
        cpu.step().unwrap(); // lwu back
        (cpu.reg(3), cpu.reg(4) as u32)
    }

    // funct5 selectors for the AMO family.
    const AMO_LR: u32 = 0x02;
    const AMO_SC: u32 = 0x03;
    const AMO_ADD: u32 = 0x00;
    const AMO_SWAP: u32 = 0x01;
    const AMO_XOR: u32 = 0x04;
    const AMO_OR: u32 = 0x08;
    const AMO_AND: u32 = 0x0c;
    const AMO_MIN: u32 = 0x10;
    const AMO_MAX: u32 = 0x14;
    const AMO_MINU: u32 = 0x18;
    const AMO_MAXU: u32 = 0x1c;

    #[test]
    fn a_extension_amoor_d_captured() {
        // amoor.d x10, x10, (x11) == 0x40a5b52f (captured from the kernel boot).
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, 0x40a5_b52f).unwrap();
        mem.write_u32(RAM_BASE + 4, ld(5, 11, 0)).unwrap();
        mem.write_u64(RAM_BASE + 0x200, 0x00ff).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(11, RAM_BASE + 0x200);
        cpu.set_reg(10, 0xff00);
        cpu.step().unwrap(); // amoor.d
        cpu.step().unwrap(); // ld x5, 0(x11)
        assert_eq!(cpu.reg(10), 0x00ff); // rd <- old value
        assert_eq!(cpu.reg(5), 0xffff); // memory <- old | rs2
    }

    #[test]
    fn a_extension_amo_doubleword_family() {
        assert_eq!(run_amo_d(AMO_SWAP, 0x1111, 0x2222), (0x1111, 0x2222));
        assert_eq!(run_amo_d(AMO_ADD, 5, 7), (5, 12));
        assert_eq!(run_amo_d(AMO_XOR, 0xff, 0x0f), (0xff, 0xf0));
        assert_eq!(run_amo_d(AMO_OR, 0xf0, 0x0f), (0xf0, 0xff));
        assert_eq!(run_amo_d(AMO_AND, 0xf0, 0x3c), (0xf0, 0x30));
        // signed min/max treat the operands as i64.
        let neg5 = (-5_i64) as u64;
        assert_eq!(run_amo_d(AMO_MIN, neg5, 3), (neg5, neg5));
        assert_eq!(run_amo_d(AMO_MAX, neg5, 3), (neg5, 3));
        // unsigned min/max treat neg5 as a huge magnitude.
        assert_eq!(run_amo_d(AMO_MINU, neg5, 3), (neg5, 3));
        assert_eq!(run_amo_d(AMO_MAXU, neg5, 3), (neg5, neg5));
    }

    /// `rdtime rd` == `csrrs rd, time, x0` (read the read-only `time` counter).
    fn rdtime(rd: u32) -> u32 {
        csrrs(rd, 0, addr::TIME)
    }

    #[test]
    fn rdtime_reads_a_monotonic_counter_from_instret() {
        let program = &[rdtime(1), addi(0, 0, 0), rdtime(2)];
        let mut cpu = cpu_with(program);
        for _ in 0..program.len() {
            cpu.step().unwrap();
        }
        // First read sees zero completed instructions; the second sees two.
        assert_eq!(cpu.reg(1), 0);
        assert_eq!(cpu.reg(2), 2);
        assert!(cpu.reg(2) > cpu.reg(1));
    }

    #[test]
    fn a_extension_lr_sc_word_round_trips() {
        // lr.w x12, (x15) == 0x1407a62f (captured from the kernel boot).
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, 0x1407_a62f).unwrap(); // lr.w x12, (x15)
        mem.write_u32(RAM_BASE + 4, amo(AMO_SC, 2, 13, 15, 14)).unwrap(); // sc.w x13, x14, (x15)
        mem.write_u32(RAM_BASE + 8, lwu(11, 15, 0)).unwrap(); // reload the cell
        mem.write_u32(RAM_BASE + 0x200, 0x1234).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(15, RAM_BASE + 0x200);
        cpu.set_reg(14, 0xbeef);
        for _ in 0..3 {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(12), 0x1234); // lr returned the old value
        assert_eq!(cpu.reg(13), 0); // sc reported success
        assert_eq!(cpu.reg(11), 0xbeef); // store landed
    }

    #[test]
    fn a_extension_sc_without_reservation_fails() {
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, amo(AMO_SC, 3, 13, 15, 14)).unwrap(); // sc.d, no prior lr
        mem.write_u32(RAM_BASE + 4, ld(11, 15, 0)).unwrap(); // reload the cell
        mem.write_u64(RAM_BASE + 0x200, 0x1234).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(15, RAM_BASE + 0x200);
        cpu.set_reg(14, 0xbeef);
        cpu.step().unwrap(); // sc.d
        cpu.step().unwrap(); // ld back
        assert_eq!(cpu.reg(13), 1); // sc reported failure
        assert_eq!(cpu.reg(11), 0x1234); // memory untouched
    }

    #[test]
    fn a_extension_store_breaks_the_reservation() {
        // lr.d, then a plain store to the reserved cell, then sc.d -> sc must fail.
        let mut mem = Memory::new(0x2000);
        mem.write_u32(RAM_BASE, amo(AMO_LR, 3, 12, 15, 0)).unwrap(); // lr.d x12, (x15)
        mem.write_u32(RAM_BASE + 4, sd(14, 15, 0)).unwrap(); // sd x14, 0(x15)
        mem.write_u32(RAM_BASE + 8, amo(AMO_SC, 3, 13, 15, 14)).unwrap(); // sc.d x13, x14, (x15)
        mem.write_u64(RAM_BASE + 0x200, 0x1234).unwrap();
        let mut cpu = Cpu::new(mem);
        cpu.set_reg(15, RAM_BASE + 0x200);
        cpu.set_reg(14, 0xbeef);
        for _ in 0..3 {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(13), 1); // reservation broken by the intervening store
    }

    #[test]
    fn a_extension_amo_word_sign_extends_old_value() {
        // amoadd.w on 0x8000_0000: rd gets the sign-extended old value, the
        // store wraps within 32 bits.
        let (old, mem) = run_amo_w(AMO_ADD, 0x8000_0000, 1);
        assert_eq!(old, 0xffff_ffff_8000_0000);
        assert_eq!(mem, 0x8000_0001);
    }
}
