//! Choosing what the guest boots.
//!
//! snemu plays a firmware role: it hands the guest a device tree, and the kernel
//! reads its runtime workload from `/chosen/bootargs`, exactly as it would from
//! QEMU's `-append` or U-Boot's `setenv bootargs`. So selecting a workload in the
//! browser is a DTB patch, not a rebuild — one kernel image contains every workload
//! and the page picks one at load time.
//!
//! Deliberately *no* registry of valid names here. `kernel_boot::bootargs` already
//! owns that mapping, and a second copy in the browser would be the duplicated
//! mapping this project has been bitten by before (`workload_features` exists
//! because one such copy was wrong at every call site but one). An unknown name is
//! the guest's business: it falls back to its default, which is the same thing that
//! happens on real hardware.

/// A workload selection from the string the JS boundary carries.
///
/// Empty means "the kernel's default", because `Option<&str>` does not cross
/// `wasm_bindgen` cleanly and the page needs one unambiguous way to say it.
///
/// This lives here rather than in the shell for a reason worth stating: it is a
/// *decision* — a convention about what an empty string means — and the shell is
/// supposed to hold none. Written inline there it read as
/// `(!workload.is_empty()).then_some(workload)`, which the thinness guard cannot see,
/// because it scans for `if`/`match` and this is a conditional wearing a method call.
/// The guard is a ratchet, not a proof; a decision that slips past it is still a
/// decision in the wrong place.
#[must_use]
pub fn selection(workload: &str) -> Option<&str> {
    (!workload.is_empty()).then_some(workload)
}

/// The bootargs string for a workload selection, or `None` to boot the default.
///
/// `None` means "no `/chosen/bootargs` at all", which is not the same as an empty
/// one: the kernel's default path is what it takes when the property is absent, and
/// this is how the browser reproduces a plain boot.
#[must_use]
pub fn bootargs_for(workload: Option<&str>) -> Option<String> {
    workload.map(|name| format!("workload={name}"))
}

/// The device tree to boot with, given a workload selection.
///
/// Returns the unpatched tree for `None`. A patch that fails to apply — a malformed
/// tree, or one with no `/chosen` — yields the unpatched tree rather than an error:
/// booting the default is a better outcome for a page than refusing to boot, and the
/// guest will say what it booted.
#[must_use]
pub fn dtb_for(base: &[u8], workload: Option<&str>) -> Vec<u8> {
    match bootargs_for(workload) {
        Some(args) => snemu::dtb::set_bootargs(base, &args).unwrap_or_else(|| base.to_vec()),
        None => base.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::{bootargs_for, dtb_for, selection};

    /// The page says "default" by sending nothing.
    #[test]
    fn an_empty_string_selects_the_kernels_default() {
        assert_eq!(selection(""), None);
    }

    #[test]
    fn a_name_selects_that_workload() {
        assert_eq!(selection("stitch-repl"), Some("stitch-repl"));
    }

    /// Whitespace is a name, not an absence. Trimming here would silently disagree
    /// with `kernel_boot::bootargs`, which is the only thing entitled to an opinion
    /// about what parses.
    #[test]
    fn whitespace_is_not_treated_as_absent() {
        assert_eq!(selection(" "), Some(" "));
    }

    #[test]
    fn a_selection_becomes_a_workload_bootarg() {
        assert_eq!(bootargs_for(Some("stitch-drivel")).as_deref(), Some("workload=stitch-drivel"));
    }

    /// The default boot has no bootargs property at all, which is distinct from an
    /// empty one — the kernel's default path is the one it takes when nothing is
    /// there.
    #[test]
    fn no_selection_means_no_bootargs() {
        assert_eq!(bootargs_for(None), None);
    }

    /// The patched tree must still be a device tree, and must actually carry the
    /// selection — parsed back with the same reader the kernel uses, so this is the
    /// guest's view rather than ours.
    #[test]
    fn the_patched_tree_parses_and_carries_the_selection() {
        let patched = dtb_for(snemu::dtb::VIRT, Some("stitch-kvetch"));
        let fdt = fdt::Fdt::new(&patched).expect("still a valid device tree");

        assert_eq!(fdt.chosen().bootargs(), Some("workload=stitch-kvetch"));
        assert!(fdt.cpus().count() >= 1, "and the rest of the tree survived");
    }

    #[test]
    fn no_selection_leaves_the_tree_untouched() {
        assert_eq!(dtb_for(snemu::dtb::VIRT, None), snemu::dtb::VIRT);
    }

    /// An unknown name is passed through rather than validated. The registry lives in
    /// `kernel_boot::bootargs`; duplicating it here would be a second copy to drift.
    #[test]
    fn an_unknown_workload_is_the_guests_business_not_ours() {
        let patched = dtb_for(snemu::dtb::VIRT, Some("no-such-workload"));
        let fdt = fdt::Fdt::new(&patched).expect("valid tree");

        assert_eq!(fdt.chosen().bootargs(), Some("workload=no-such-workload"));
    }

    /// A tree that cannot be patched still boots — the default, rather than nothing.
    #[test]
    fn an_unpatchable_tree_falls_back_to_booting_the_default() {
        let junk = b"not a device tree".to_vec();
        assert_eq!(dtb_for(&junk, Some("stitch-repl")), junk);
    }
}
