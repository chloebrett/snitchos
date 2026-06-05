use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn run() -> ExitCode {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("xtask has no parent");

    let rs_files = collect_rs_files(workspace_root);

    let mut by_crate: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut vendor_total = 0usize;

    for path in &rs_files {
        let rel = path.strip_prefix(workspace_root).expect("path under workspace root");
        let crate_name = rel
            .components()
            .next()
            .expect("at least one component")
            .as_os_str()
            .to_string_lossy()
            .into_owned();

        let content = fs::read_to_string(path).unwrap_or_default();
        let (prod, test) = count_file_lines(&content);

        if crate_name == "vendor" {
            vendor_total += prod + test;
        } else {
            let entry = by_crate.entry(crate_name).or_insert((0, 0));
            entry.0 += prod;
            entry.1 += test;
        }
    }

    print_table(&by_crate, vendor_total);
    ExitCode::SUCCESS
}

fn collect_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rs_files_inner(root, root, &mut out);
    out.sort();
    out
}

fn collect_rs_files_inner(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if name_str == "target" {
                continue;
            }
            collect_rs_files_inner(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Returns `(prod_lines, test_lines)` for a Rust source file.
///
/// Lines are classified as test if they fall within a `#[cfg(test)]` or
/// `#[test]` attributed item.  Brace-depth tracking is used to find the
/// end of the block; string literals containing `{`/`}` may slightly skew
/// the count, but the error is negligible in practice.
fn count_file_lines(content: &str) -> (usize, usize) {
    let mut prod = 0usize;
    let mut test = 0usize;
    let mut in_test = false;
    let mut depth = 0i32;
    let mut awaiting_open = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        let has_test_attr = trimmed.starts_with("#[cfg(test)]")
            || trimmed.starts_with("#[test]")
            || trimmed.starts_with("#[cfg(test,");

        let net: i32 = line
            .chars()
            .map(|c| match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            })
            .sum();

        if in_test {
            test += 1;
            depth += net;
            if depth <= 0 {
                in_test = false;
                depth = 0;
            }
        } else if awaiting_open {
            test += 1;
            if net > 0 {
                in_test = true;
                depth = net;
                awaiting_open = false;
            } else if net < 0 {
                awaiting_open = false;
            }
        } else if has_test_attr {
            awaiting_open = true;
            test += 1;
        } else {
            prod += 1;
        }
    }

    (prod, test)
}

fn print_table(by_crate: &BTreeMap<String, (usize, usize)>, vendor_total: usize) {
    let sep = "─".repeat(48);

    println!("SnitchOS workspace — lines of code");
    println!("{sep}");
    println!("{:<14}  {:>7}  {:>7}  {:>7}", "crate", "prod", "test", "total");
    println!("{sep}");

    let mut total_prod = 0usize;
    let mut total_test = 0usize;

    for (name, (prod, test)) in by_crate {
        let total = prod + test;
        let test_str = if *test == 0 {
            "—".to_string()
        } else {
            fmt_n(*test)
        };
        println!(
            "{:<14}  {:>7}  {:>7}  {:>7}",
            name,
            fmt_n(*prod),
            test_str,
            fmt_n(total),
        );
        total_prod += prod;
        total_test += test;
    }

    println!("{sep}");
    println!(
        "{:<14}  {:>7}  {:>7}  {:>7}",
        "total",
        fmt_n(total_prod),
        fmt_n(total_test),
        fmt_n(total_prod + total_test),
    );

    if vendor_total > 0 {
        println!();
        println!(
            "vendor (excluded)            {:>7}   linked_list_allocator fork",
            fmt_n(vendor_total)
        );
    }
}

fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut result = Vec::with_capacity(s.len() + s.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        result.push(b);
        let remaining = bytes.len() - 1 - i;
        if remaining > 0 && remaining % 3 == 0 {
            result.push(b',');
        }
    }
    String::from_utf8(result).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_prod_file() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        assert_eq!(count_file_lines(src), (3, 0));
    }

    #[test]
    fn cfg_test_mod_block() {
        let src = "\
fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_adds() {
        assert_eq!(add(1, 2), 3);
    }
}
";
        let (prod, test) = count_file_lines(src);
        assert_eq!(prod, 2, "prod");
        assert_eq!(test, 9, "test");
    }

    #[test]
    fn standalone_test_fn() {
        let src = "\
fn helper() -> bool { true }

#[test]
fn standalone() {
    assert!(helper());
}
";
        let (prod, test) = count_file_lines(src);
        assert_eq!(prod, 2, "prod");
        assert_eq!(test, 4, "test");
    }

    #[test]
    fn blank_file() {
        assert_eq!(count_file_lines(""), (0, 0));
    }

    #[test]
    fn test_block_with_nested_braces() {
        let src = "\
fn real() {}

#[cfg(test)]
mod tests {
    fn inner() {
        if true {
            let _ = 1;
        }
    }
}
";
        let (prod, test) = count_file_lines(src);
        assert_eq!(prod, 2, "prod");
        assert_eq!(test, 8, "test");
    }

    #[test]
    fn fmt_n_formats_thousands() {
        assert_eq!(fmt_n(0), "0");
        assert_eq!(fmt_n(999), "999");
        assert_eq!(fmt_n(1000), "1,000");
        assert_eq!(fmt_n(10765), "10,765");
        assert_eq!(fmt_n(1_234_567), "1,234,567");
    }
}
