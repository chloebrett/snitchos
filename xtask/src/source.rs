//! Shared, dependency-free heuristics for reading Rust source as text.
//!
//! These are line/char level — no `syn`, no name resolution. They are good
//! enough to *flag candidates* for the `crate-audit` skill, never to decide
//! deletion. See `plans/legacy/xtask-audit.md`.

/// One bool per line of `content`: `true` if the line falls within a
/// `#[cfg(test)]` / `#[test]` attributed item (the attribute line, and every
/// line of the block it guards). Brace-depth tracking finds the block end;
/// string literals containing `{`/`}` may skew it slightly — negligible for
/// flagging. Lifted from `loc::count_file_lines` so both share one notion of
/// "this line is test code".
pub fn test_line_mask(content: &str) -> Vec<bool> {
    let mut mask = Vec::new();
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
            mask.push(true);
            depth += net;
            if depth <= 0 {
                in_test = false;
                depth = 0;
            }
        } else if awaiting_open {
            mask.push(true);
            if net > 0 {
                in_test = true;
                depth = net;
                awaiting_open = false;
            } else if net < 0 {
                awaiting_open = false;
            }
        } else if has_test_attr {
            awaiting_open = true;
            mask.push(true);
        } else {
            mask.push(false);
        }
    }

    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_prod_is_all_false() {
        let src = "fn main() {\n    let x = 1;\n}\n";
        assert_eq!(test_line_mask(src), vec![false, false, false]);
    }

    #[test]
    fn cfg_test_mod_block_is_all_true() {
        let src = "\
fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    fn it_adds() {}
}
";
        assert_eq!(
            test_line_mask(src),
            vec![false, false, true, true, true, true],
        );
    }

    #[test]
    fn standalone_test_fn_block_is_true() {
        let src = "\
fn helper() -> bool { true }

#[test]
fn standalone() {
    assert!(helper());
}
";
        assert_eq!(
            test_line_mask(src),
            vec![false, false, true, true, true, true],
        );
    }

    #[test]
    fn nested_braces_inside_test_block_stay_true() {
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
        assert_eq!(
            test_line_mask(src),
            vec![false, false, true, true, true, true, true, true, true, true],
        );
    }

    #[test]
    fn blank_content_is_empty_mask() {
        assert_eq!(test_line_mask(""), Vec::<bool>::new());
    }
}
