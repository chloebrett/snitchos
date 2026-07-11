//! Minimal ELF64 symbol-table parsing + PC→function resolution, for the guest
//! instret profiler (`snemu-profile`). The loader ([`crate::loader`]) reads
//! program headers to place segments; this reads the `.symtab` / `.strtab`
//! sections to name code addresses, so a per-PC instret histogram can roll up to
//! "which function is the guest spending its cycles in."
//!
//! Kept in snemu (next to the loader) because it's pure ELF parsing; the
//! aggregation + categorisation + reporting layer lives in xtask.

/// One function symbol: its runtime address, byte size (`0` if the producer
/// didn't record one), and demangled-or-raw name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub addr: u64,
    pub size: u64,
    pub name: String,
}

/// A set of function symbols, sorted by address, supporting "which function
/// contains this PC" lookups.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    /// Sorted ascending by `addr`.
    syms: Vec<Symbol>,
}

impl SymbolTable {
    /// Build from an unsorted symbol list.
    #[must_use]
    pub fn new(mut syms: Vec<Symbol>) -> Self {
        syms.sort_by(|a, b| a.addr.cmp(&b.addr).then_with(|| a.name.cmp(&b.name)));
        Self { syms }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.syms.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.syms.is_empty()
    }

    /// The function containing `pc`: the symbol with the largest `addr <= pc`
    /// (nearest-preceding, the standard `nm`-style attribution for
    /// contiguously-laid-out code). `None` if `pc` is below every symbol.
    #[must_use]
    pub fn resolve(&self, pc: u64) -> Option<&str> {
        // `partition_point` gives the first index whose addr > pc; the one before
        // it is the nearest preceding symbol.
        let idx = self.syms.partition_point(|s| s.addr <= pc);
        if idx == 0 {
            return None;
        }
        Some(&self.syms[idx - 1].name)
    }
}

/// Resolve the runtime address of a named function symbol from an ELF64 image's
/// `.symtab`, or `None` if absent/stripped. Used to find the entry PC of
/// `memset`/`memcpy` so the native-op helper (tier-0.5 of the JIT) can intercept
/// them. Hand-rolled to match the loader's ethos: walk the section headers to
/// `.symtab` + its linked string table, scan `STT_FUNC` entries for an exact name.
#[must_use]
pub fn function_addr(image: &[u8], name: &str) -> Option<u64> {
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(image.get(o..o + 2)?.try_into().ok()?)) };
    let u32_at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(image.get(o..o + 4)?.try_into().ok()?)) };
    let u64_at = |o: usize| -> Option<u64> { Some(u64::from_le_bytes(image.get(o..o + 8)?.try_into().ok()?)) };

    // ELF64 header → section header table (e_shoff @0x28, e_shentsize @0x3a, e_shnum @0x3c).
    let sh_off = usize::try_from(u64_at(0x28)?).ok()?;
    let sh_entsize = u16_at(0x3a)? as usize;
    let sh_num = u16_at(0x3c)? as usize;

    for i in 0..sh_num {
        let sh = sh_off + i * sh_entsize;
        if u32_at(sh + 4)? != 2 {
            continue; // sh_type != SHT_SYMTAB
        }
        let sym_off = usize::try_from(u64_at(sh + 0x18)?).ok()?; // sh_offset
        let sym_size = usize::try_from(u64_at(sh + 0x20)?).ok()?; // sh_size
        let sym_entsize = usize::try_from(u64_at(sh + 0x38)?).ok()?; // sh_entsize
        let strtab = sh_off + u32_at(sh + 0x28)? as usize * sh_entsize; // sh_link → strtab section
        let str_off = usize::try_from(u64_at(strtab + 0x18)?).ok()?;

        let count = if sym_entsize == 0 { 0 } else { sym_size / sym_entsize };
        for s in 0..count {
            let sym = sym_off + s * sym_entsize;
            // Elf64_Sym: st_name @0 (u32), st_info @4 (u8), st_value @8 (u64).
            if image.get(sym + 4)? & 0xf != 2 {
                continue; // not STT_FUNC
            }
            let st_name = u32_at(sym)? as usize;
            let name_bytes = image.get(str_off + st_name..)?;
            let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
            if &name_bytes[..end] == name.as_bytes() {
                return u64_at(sym + 8);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(addr: u64, size: u64, name: &str) -> Symbol {
        Symbol { addr, size, name: name.to_owned() }
    }

    #[test]
    fn resolve_picks_the_nearest_preceding_symbol() {
        let t = SymbolTable::new(vec![
            sym(0x2000, 0x100, "beta"),
            sym(0x1000, 0x200, "alpha"),
        ]);
        // Exactly at a symbol's start resolves to it.
        assert_eq!(t.resolve(0x1000), Some("alpha"));
        // Inside alpha.
        assert_eq!(t.resolve(0x1123), Some("alpha"));
        // At/after beta's start resolves to beta, not alpha.
        assert_eq!(t.resolve(0x2000), Some("beta"));
        assert_eq!(t.resolve(0x2040), Some("beta"));
    }

    #[test]
    fn resolve_below_all_symbols_is_none() {
        let t = SymbolTable::new(vec![sym(0x1000, 0x10, "alpha")]);
        assert_eq!(t.resolve(0x0fff), None);
    }

    #[test]
    fn resolve_on_empty_table_is_none() {
        let t = SymbolTable::default();
        assert_eq!(t.resolve(0x1000), None);
    }
}
