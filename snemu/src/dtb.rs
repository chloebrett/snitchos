//! Firmware-role DTB patching: inject `/chosen/bootargs` into the flattened
//! device tree, the way QEMU's `-append` does. The kernel reads its runtime
//! workload from `dtb.chosen().bootargs()` (an `itest-workloads` build), so this
//! is how snemu selects a workload — `set_bootargs(dtb, "workload=demo")`.
//!
//! Flattened Device Tree (v17), all integers **big-endian**:
//! header · memory-reservation block · structure block (token stream) · strings
//! block. We add one `FDT_PROP` to the existing `/chosen` node and, if needed,
//! append its name to the strings block, then fix the header's sizes/offsets.

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
// FDT_END (9) — end of the structure block — falls into the walk's catch-all.

// Header field offsets.
const OFF_TOTALSIZE: usize = 0x04;
const OFF_DT_STRUCT: usize = 0x08;
const OFF_DT_STRINGS: usize = 0x0c;
const OFF_SIZE_STRINGS: usize = 0x20;
const OFF_SIZE_STRUCT: usize = 0x24;

fn be32(bytes: &[u8], off: usize) -> Option<u32> {
    let slice = bytes.get(off..off + 4)?;
    Some(u32::from_be_bytes(slice.try_into().unwrap()))
}

fn put_be32(bytes: &mut [u8], off: usize, value: u32) {
    bytes[off..off + 4].copy_from_slice(&value.to_be_bytes());
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Return a copy of `dtb` with `/chosen/bootargs = <bootargs>`. `None` if `dtb`
/// isn't a valid FDT or has no `/chosen` node (QEMU's `virt` always emits one).
/// Assumes `/chosen` has no existing `bootargs` (true for a `-append`-less dump).
#[must_use]
pub fn set_bootargs(dtb: &[u8], bootargs: &str) -> Option<Vec<u8>> {
    if be32(dtb, 0)? != FDT_MAGIC {
        return None;
    }
    let off_struct = be32(dtb, OFF_DT_STRUCT)? as usize;
    let off_strings = be32(dtb, OFF_DT_STRINGS)? as usize;
    let size_strings = be32(dtb, OFF_SIZE_STRINGS)? as usize;
    let size_struct = be32(dtb, OFF_SIZE_STRUCT)? as usize;

    let struct_block = dtb.get(off_struct..off_struct + size_struct)?;
    let strings_block = dtb.get(off_strings..off_strings + size_strings)?;

    // Find (or plan to append) the "bootargs" property name in the strings block.
    let (nameoff, new_strings) = if let Some(off) = find_string(strings_block, b"bootargs") {
        (off, strings_block.to_vec())
    } else {
        let off = strings_block.len();
        let mut s = strings_block.to_vec();
        s.extend_from_slice(b"bootargs\0");
        (off, s)
    };

    // The property token goes right after `/chosen`'s BEGIN_NODE (properties
    // precede child nodes), so it's the first property of /chosen.
    let insert_at = chosen_properties_offset(struct_block)?;

    // FDT_PROP: token, len, nameoff, value (null-terminated, padded to 4).
    let mut value = bootargs.as_bytes().to_vec();
    value.push(0);
    let mut prop = Vec::new();
    prop.extend_from_slice(&FDT_PROP.to_be_bytes());
    prop.extend_from_slice(&(value.len() as u32).to_be_bytes());
    prop.extend_from_slice(&(nameoff as u32).to_be_bytes());
    prop.extend_from_slice(&value);
    while prop.len() % 4 != 0 {
        prop.push(0);
    }

    let mut new_struct = Vec::with_capacity(struct_block.len() + prop.len());
    new_struct.extend_from_slice(&struct_block[..insert_at]);
    new_struct.extend_from_slice(&prop);
    new_struct.extend_from_slice(&struct_block[insert_at..]);

    // Reassemble: everything up to the struct block (header + mem-rsvmap) is
    // unchanged; the struct block grew, so the strings block shifts.
    let mut out = dtb[..off_struct].to_vec();
    out.extend_from_slice(&new_struct);
    let new_off_strings = out.len();
    out.extend_from_slice(&new_strings);

    let totalsize = out.len() as u32;
    put_be32(&mut out, OFF_TOTALSIZE, totalsize);
    put_be32(&mut out, OFF_DT_STRINGS, new_off_strings as u32);
    put_be32(&mut out, OFF_SIZE_STRINGS, new_strings.len() as u32);
    put_be32(&mut out, OFF_SIZE_STRUCT, new_struct.len() as u32);
    Some(out)
}

/// Byte offset (within the struct block) just past `/chosen`'s `BEGIN_NODE`
/// token and name — where its first property belongs.
fn chosen_properties_offset(struct_block: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(tok) = be32(struct_block, i) {
        match tok {
            FDT_BEGIN_NODE => {
                let name_start = i + 4;
                let rel_nul = struct_block[name_start..].iter().position(|&b| b == 0)?;
                let after_name = align4(name_start + rel_nul + 1);
                if &struct_block[name_start..name_start + rel_nul] == b"chosen" {
                    return Some(after_name);
                }
                i = after_name;
            }
            FDT_END_NODE | FDT_NOP => i += 4,
            FDT_PROP => {
                let len = be32(struct_block, i + 4)? as usize;
                i = align4(i + 12 + len);
            }
            // FDT_END (tree ended, no /chosen) or any unknown token: give up.
            _ => return None,
        }
    }
    None
}

/// Return a copy of `dtb` with the `/memory` node's `reg` **size** overwritten to
/// `size_bytes` (base address unchanged). In-place — same length, so no header
/// fix-ups. `None` if `dtb` isn't a valid FDT, has no `memory@…` node, or its `reg`
/// isn't the QEMU `virt` shape (2 address cells + 2 size cells → a 16-byte value).
///
/// The kernel's frame allocator sizes its pool from this (`frame::init_from_dtb`
/// walks `/memory`), so shrinking it makes an OOM workload exhaust RAM
/// proportionally faster — the same organic exhaustion on a smaller machine, which
/// is a more honest "small-RAM OOM" than pre-reserving frames.
#[must_use]
pub fn set_memory_size(dtb: &[u8], size_bytes: u64) -> Option<Vec<u8>> {
    if be32(dtb, 0)? != FDT_MAGIC {
        return None;
    }
    let off_struct = be32(dtb, OFF_DT_STRUCT)? as usize;
    let off_strings = be32(dtb, OFF_DT_STRINGS)? as usize;
    let size_strings = be32(dtb, OFF_SIZE_STRINGS)? as usize;
    let size_struct = be32(dtb, OFF_SIZE_STRUCT)? as usize;
    let struct_block = dtb.get(off_struct..off_struct + size_struct)?;
    let strings_block = dtb.get(off_strings..off_strings + size_strings)?;

    // reg value = [base: u64_be][size: u64_be]; overwrite the size (last 8 bytes).
    let reg_value_off = memory_reg_value_offset(struct_block, strings_block)?;
    let size_off = off_struct + reg_value_off + 8;
    let mut out = dtb.to_vec();
    out.get_mut(size_off..size_off + 8)?.copy_from_slice(&size_bytes.to_be_bytes());
    Some(out)
}

/// Byte offset (within `struct_block`) of the `memory@…` node's `reg` property
/// **value**, for the QEMU `virt` shape (a single 16-byte cell: base u64, size
/// u64). `None` if there's no such node/property.
fn memory_reg_value_offset(struct_block: &[u8], strings: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut depth = 0i32;
    let mut memory_depth: Option<i32> = None;
    while let Some(tok) = be32(struct_block, i) {
        match tok {
            FDT_BEGIN_NODE => {
                depth += 1;
                let name_start = i + 4;
                let rel_nul = struct_block[name_start..].iter().position(|&b| b == 0)?;
                let name = &struct_block[name_start..name_start + rel_nul];
                if name == b"memory" || name.starts_with(b"memory@") {
                    memory_depth = Some(depth);
                }
                i = align4(name_start + rel_nul + 1);
            }
            FDT_END_NODE => {
                if memory_depth == Some(depth) {
                    memory_depth = None; // left the memory node without finding reg
                }
                depth -= 1;
                i += 4;
            }
            FDT_NOP => i += 4,
            FDT_PROP => {
                let len = be32(struct_block, i + 4)? as usize;
                let nameoff = be32(struct_block, i + 8)? as usize;
                let value_off = i + 12;
                if memory_depth == Some(depth) && len == 16 {
                    let rel_nul = strings.get(nameoff..)?.iter().position(|&b| b == 0)?;
                    if &strings[nameoff..nameoff + rel_nul] == b"reg" {
                        return Some(value_off);
                    }
                }
                i = align4(i + 12 + len);
            }
            _ => return None, // FDT_END
        }
    }
    None
}

/// Offset of a null-terminated `needle` within the strings block, if present.
fn find_string(strings: &[u8], needle: &[u8]) -> Option<usize> {
    let mut off = 0;
    while off < strings.len() {
        let rel_nul = strings[off..].iter().position(|&b| b == 0)?;
        if &strings[off..off + rel_nul] == needle {
            return Some(off);
        }
        off += rel_nul + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `-smp 2` QEMU `virt` device tree snemu ships.
    const DTB: &[u8] = include_bytes!("../virt.dtb");

    #[test]
    fn injected_bootargs_parse_back_via_the_fdt_reader() {
        let patched = set_bootargs(DTB, "workload=demo").expect("patch");
        // Parse with the same reader the kernel uses.
        let fdt = fdt::Fdt::new(&patched).expect("valid fdt after patch");
        assert_eq!(fdt.chosen().bootargs(), Some("workload=demo"));
    }

    #[test]
    fn patch_preserves_the_rest_of_the_tree() {
        let patched = set_bootargs(DTB, "workload=smp").expect("patch");
        let fdt = fdt::Fdt::new(&patched).expect("valid fdt");
        // The bootargs took, and unrelated nodes still parse (tree intact).
        assert_eq!(fdt.chosen().bootargs(), Some("workload=smp"));
        assert!(fdt.memory().regions().count() >= 1);
        assert!(fdt.cpus().count() >= 2); // -smp 2
    }

    #[test]
    fn rejects_a_non_fdt_blob() {
        assert!(set_bootargs(&[0, 1, 2, 3, 4, 5, 6, 7], "x").is_none());
    }

    #[test]
    fn set_memory_size_shrinks_the_region_read_back_by_the_fdt_reader() {
        const NEW: u64 = 48 * 1024 * 1024;
        let base_before = fdt::Fdt::new(DTB)
            .unwrap()
            .memory()
            .regions()
            .next()
            .unwrap()
            .starting_address as u64;

        let patched = set_memory_size(DTB, NEW).expect("patch");
        // In-place edit — same length, no header shift.
        assert_eq!(patched.len(), DTB.len());

        let fdt = fdt::Fdt::new(&patched).expect("valid fdt after patch");
        let region = fdt.memory().regions().next().expect("memory region");
        assert_eq!(region.size, Some(NEW as usize), "size took");
        assert_eq!(region.starting_address as u64, base_before, "base unchanged");
        // The rest of the tree still parses.
        assert!(fdt.cpus().count() >= 2);
    }

    #[test]
    fn set_memory_size_rejects_a_non_fdt_blob() {
        assert!(set_memory_size(&[0, 1, 2, 3, 4, 5, 6, 7], 1).is_none());
    }
}
