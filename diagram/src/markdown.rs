//! Pull fenced ` ```mermaid ` blocks back out of a committed markdown doc, so
//! xtask can hand them to a mermaid renderer (`mmdc`) for local SVGs. Pure —
//! the file I/O and the render shell-out live in xtask.

/// Return the body of every fenced ` ```mermaid ` block in `md`, in order,
/// each with a trailing newline per line. Unterminated blocks are dropped.
pub fn extract_mermaid(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open = false;
    let mut buf = String::new();
    for line in md.lines() {
        let fence = line.trim_start();
        if !open {
            if fence.starts_with("```mermaid") {
                open = true;
                buf.clear();
            }
        } else if fence == "```" {
            blocks.push(std::mem::take(&mut buf));
            open = false;
        } else {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_mermaid_blocks_in_order() {
        let md = "# Title\n\nprose\n\n```mermaid\ngraph TD\n    a --> b\n```\n\ntail\n";
        assert_eq!(extract_mermaid(md), vec!["graph TD\n    a --> b\n".to_string()]);
    }

    #[test]
    fn ignores_non_mermaid_fences_and_returns_empty() {
        let md = "prose\n\n```rust\nfn main() {}\n```\n";
        assert!(extract_mermaid(md).is_empty());
    }
}
