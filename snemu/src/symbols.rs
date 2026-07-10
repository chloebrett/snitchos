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
