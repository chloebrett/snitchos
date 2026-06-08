//! `cargo xtask audit <crate>` — mechanical evidence-gatherer for the
//! `crate-audit` skill. Replaces fragile bash (per-symbol greps, dead_code
//! builds blind to `pub`-in-`pub`-mod) with a deterministic table.
//!
//! This is **not** a static analyzer: line/word-boundary heuristics, no `syn`,
//! no name resolution. Counts can over-report callers (a name collision looks
//! like a use) but never under-report — so a flagged zero-caller `pub` is a
//! high-confidence *candidate*, which the skill verifies against design docs
//! (rule 6) before anyone deletes. See `plans/xtask-audit.md`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::source::test_line_mask;

/// Top-level dirs under the workspace root that aren't sibling crates.
const NON_CRATE_DIRS: [&str; 6] = ["vendor", "target", "stack", "docs", "plans", "posts"];

/// `cargo xtask audit <crate>`: print the per-symbol caller table, the
/// zero-caller candidates, debt markers, and unused deps for one crate.
pub fn run(crate_name: &str, json: bool, include_short: bool) -> ExitCode {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.parent().expect("xtask has no parent");
    let target = root.join(crate_name);
    if !target.join("src").is_dir() {
        eprintln!("audit: no crate `{crate_name}` (expected {}/src)", target.display());
        return ExitCode::FAILURE;
    }

    let target_files = read_rs_under(&target.join("src"), &target);
    let sibling_contents = sibling_crate_contents(root, crate_name);
    let reports = build_reports(&target_files, &sibling_contents, include_short);

    let markers: Vec<(String, usize, String)> = target_files
        .iter()
        .flat_map(|(f, c)| scan_markers(c).into_iter().map(move |(n, t)| (f.clone(), n, t)))
        .collect();

    let unused_deps = match run_machete(root, crate_name) {
        Ok(deps) => deps,
        Err(hint) => {
            eprintln!("{hint}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        print!("{}", render_json(crate_name, &reports, &markers, &unused_deps));
        return ExitCode::SUCCESS;
    }

    print_report(crate_name, &reports, &markers, &unused_deps);
    ExitCode::SUCCESS
}

/// Collect `(path_relative_to_crate, content)` for every `.rs` file under `dir`.
fn read_rs_under(dir: &Path, crate_root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut paths = Vec::new();
    collect_rs(dir, &mut paths);
    paths.sort();
    for path in paths {
        let rel = path.strip_prefix(crate_root).unwrap_or(&path).to_string_lossy().into_owned();
        if let Ok(content) = fs::read_to_string(&path) {
            files.push((rel, content));
        }
    }
    files
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                collect_rs(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Concatenated `.rs` source of every sibling crate (any top-level dir with a
/// `Cargo.toml` that isn't the target or a known non-crate dir).
fn sibling_crate_contents(root: &Path, crate_name: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_dir() || name == crate_name || NON_CRATE_DIRS.contains(&name.as_str()) {
            continue;
        }
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        for (_, content) in read_rs_under(&path, &path) {
            out.push(content);
        }
    }
    out
}

/// Run `cargo machete <crate>` and parse its unused-dependency findings. Returns
/// `Err(hint)` if cargo-machete isn't installed (no graceful degradation — this
/// is workspace tooling that assumes its tools are present).
fn run_machete(root: &Path, crate_name: &str) -> Result<Vec<(String, String)>, String> {
    let output = Command::new("cargo")
        .arg("machete")
        .arg(crate_name)
        .current_dir(root)
        .output()
        .map_err(|e| format!("audit: could not run cargo: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("no such command") {
        return Err("audit: cargo-machete not found — `cargo install cargo-machete`".to_string());
    }
    Ok(parse_machete(&String::from_utf8_lossy(&output.stdout)))
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Fn => "fn",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Const => "const",
        SymbolKind::Type => "type",
        SymbolKind::Mod => "mod",
    }
}

fn verdict_str(v: Verdict) -> &'static str {
    match v {
        Verdict::KeepPublic => "keep-public",
        Verdict::DemotePubCrate => "demote-pub(crate)",
        Verdict::ProdUnusedTestOnly => "test-only",
        Verdict::DeadCandidate => "dead-candidate",
    }
}

fn print_report(
    crate_name: &str,
    reports: &[SymbolReport],
    markers: &[(String, usize, String)],
    unused_deps: &[(String, String)],
) {
    let sep = "─".repeat(78);
    println!("crate-audit evidence — {crate_name}");
    println!("(counts are a lower bound on deadness — verify candidates vs design docs, rule 6)");
    println!("{sep}");
    println!("{:<28}  {:>6}  {:>4} {:>4} {:>4}  {}", "symbol", "kind", "ext", "int", "tst", "site");
    println!("{sep}");

    let mut rows: Vec<&SymbolReport> = reports.iter().collect();
    rows.sort_by(|a, b| {
        (a.ext + a.int + a.test)
            .cmp(&(b.ext + b.int + b.test))
            .then(a.sym.name.cmp(&b.sym.name))
    });
    for r in &rows {
        println!(
            "{:<28}  {:>6}  {:>4} {:>4} {:>4}  {}:{}",
            r.sym.name, kind_str(r.sym.kind), r.ext, r.int, r.test, r.file, r.sym.line,
        );
    }

    if reports.is_empty() {
        println!("(no bare-`pub` items found)");
    } else if reports.iter().all(|r| r.ext == 0 && r.int == 0) {
        println!();
        println!("⚠ every symbol shows ext=0 int=0 — the scan is almost certainly broken");
        println!("  (wrong crate dir / no siblings resolved), not the crate genuinely dead.");
    }

    let candidates: Vec<&&SymbolReport> = rows
        .iter()
        .filter(|r| matches!(r.verdict, Verdict::DeadCandidate | Verdict::ProdUnusedTestOnly))
        .collect();
    println!();
    println!("candidates (ext=0 — verify before acting):");
    if candidates.is_empty() {
        println!("  none");
    } else {
        for r in candidates {
            println!("  {} [{}] {}:{}", r.sym.name, verdict_str(r.verdict), r.file, r.sym.line);
        }
    }

    println!();
    println!("unused dependencies (cargo machete):");
    if unused_deps.is_empty() {
        println!("  none");
    } else {
        for (krate, dep) in unused_deps {
            println!("  {krate}: {dep}");
        }
    }

    println!();
    println!("markers ({}):", markers.len());
    for (file, line, text) in markers {
        println!("  {file}:{line}: {text}");
    }
}

/// Hand-rolled JSON so the `crate-audit` skill can consume the evidence without
/// scraping the table — keeps the zero-extra-dep promise (no `serde_json`).
fn render_json(
    crate_name: &str,
    reports: &[SymbolReport],
    markers: &[(String, usize, String)],
    unused_deps: &[(String, String)],
) -> String {
    let syms: Vec<String> = reports
        .iter()
        .map(|r| {
            format!(
                "{{\"name\":{},\"kind\":\"{}\",\"file\":{},\"line\":{},\"ext\":{},\"int\":{},\"test\":{},\"verdict\":\"{}\"}}",
                jstr(&r.sym.name),
                kind_str(r.sym.kind),
                jstr(&r.file),
                r.sym.line,
                r.ext,
                r.int,
                r.test,
                verdict_str(r.verdict),
            )
        })
        .collect();
    let marks: Vec<String> = markers
        .iter()
        .map(|(f, n, t)| format!("{{\"file\":{},\"line\":{},\"text\":{}}}", jstr(f), n, jstr(t)))
        .collect();
    let deps: Vec<String> = unused_deps
        .iter()
        .map(|(k, d)| format!("{{\"crate\":{},\"dep\":{}}}", jstr(k), jstr(d)))
        .collect();
    format!(
        "{{\"crate\":{},\"symbols\":[{}],\"markers\":[{}],\"unused_deps\":[{}]}}\n",
        jstr(crate_name),
        syms.join(","),
        marks.join(","),
        deps.join(","),
    )
}

/// Minimal JSON string encoder — quotes and escapes `"`, `\`, and control chars.
fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}


/// What kind of `pub` item a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Const,
    Type,
    Mod,
}

/// The tool's read on a `pub` symbol from its caller counts. Every variant is
/// a *candidate* for the human + skill to confirm — never a verdict. `ext` =
/// callers in sibling crates, `int` = non-test callers in this crate, `test` =
/// test-only callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// `ext > 0` — used by another crate; keep it `pub`.
    KeepPublic,
    /// `ext == 0, int > 0` — internal-only; could be `pub(crate)`.
    DemotePubCrate,
    /// `ext == 0, int == 0, test > 0` — only tests touch it. Verify it isn't
    /// reserved contract surface (rule 6) before demoting / `#[cfg(test)]`.
    ProdUnusedTestOnly,
    /// All zero — no caller anywhere. Highest-confidence deletion candidate;
    /// still check the design docs first.
    DeadCandidate,
}

/// Map caller counts to a [`Verdict`]. Pure; the report layer adds the
/// "candidate, not verdict" caveat.
pub fn classify(ext: usize, int: usize, test: usize) -> Verdict {
    match (ext, int, test) {
        (e, _, _) if e > 0 => Verdict::KeepPublic,
        (0, i, _) if i > 0 => Verdict::DemotePubCrate,
        (0, 0, t) if t > 0 => Verdict::ProdUnusedTestOnly,
        _ => Verdict::DeadCandidate,
    }
}

/// A `pub` (not `pub(crate)`) item declared in a crate's source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PubSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
}

/// One row of the audit: a `pub` symbol, where it's declared, its caller counts
/// across this crate (`int`/`test`) and sibling crates (`ext`), and the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolReport {
    pub sym: PubSymbol,
    pub file: String,
    pub ext: usize,
    pub int: usize,
    pub test: usize,
    pub verdict: Verdict,
}

/// Aggregate per-symbol caller counts across a crate's own files
/// (`target_files`: `(relative_path, content)`) and its sibling crates'
/// concatenated source (`sibling_contents`), then classify each. `allow_short`
/// includes ≤2-char idents (off by default — they're noise). Pure: all I/O is
/// done by the caller. Symbols are reported in declaration order, deduped by
/// (file, name) so a re-export line doesn't double-count.
pub fn build_reports(
    target_files: &[(String, String)],
    sibling_contents: &[String],
    allow_short: bool,
) -> Vec<SymbolReport> {
    let target_masks: Vec<(String, String, Vec<bool>)> = target_files
        .iter()
        .map(|(f, c)| (f.clone(), c.clone(), test_line_mask(c)))
        .collect();
    let sibling_masks: Vec<(String, Vec<bool>)> = sibling_contents
        .iter()
        .map(|c| (c.clone(), test_line_mask(c)))
        .collect();

    let mut reports = Vec::new();
    for (file, content, mask) in &target_masks {
        for sym in extract_pub_symbols(content, mask) {
            let (mut int, mut test) = (0usize, 0usize);
            for (_, c, m) in &target_masks {
                let (p, t) = count_callers(&sym.name, c, m, allow_short);
                int += p;
                test += t;
            }
            let ext: usize = sibling_masks
                .iter()
                .map(|(c, m)| {
                    let (p, t) = count_callers(&sym.name, c, m, allow_short);
                    p + t
                })
                .sum();
            let verdict = classify(ext, int, test);
            reports.push(SymbolReport { sym, file: file.clone(), ext, int, test, verdict });
        }
    }
    reports
}

/// Scan `content` for bare-`pub` item declarations, skipping any line the
/// `test_mask` marks as test code and any `pub(crate)`/`pub(super)` item.
/// Heuristic: the line must *start* (after indentation) with `pub` — so `pub`
/// inside a string/comment mid-line is ignored. `line` is 1-based.
pub fn extract_pub_symbols(content: &str, test_mask: &[bool]) -> Vec<PubSymbol> {
    content
        .lines()
        .enumerate()
        .filter(|(i, _)| !test_mask.get(*i).copied().unwrap_or(false))
        .filter_map(|(i, line)| parse_pub_line(line).map(|(kind, name)| PubSymbol {
            name,
            kind,
            line: i + 1,
        }))
        .collect()
}

/// Parse a single source line into `(kind, name)` if it declares a bare-`pub`
/// item. Returns `None` for non-pub lines, restricted visibility, or `pub use`.
fn parse_pub_line(line: &str) -> Option<(SymbolKind, String)> {
    let mut toks = line.trim_start().split_whitespace();
    if toks.next()? != "pub" {
        return None;
    }
    let rest: Vec<&str> = toks.collect();

    // `fn` wins over any preceding modifier (`const`/`async`/`unsafe`/`extern "C"`)
    // so `pub const fn` is a Fn, not a Const.
    if let Some(i) = rest.iter().position(|t| *t == "fn") {
        return rest.get(i + 1).and_then(|t| ident(t)).map(|n| (SymbolKind::Fn, n));
    }

    for (i, tok) in rest.iter().enumerate() {
        let kind = match *tok {
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "trait" => SymbolKind::Trait,
            "const" => SymbolKind::Const,
            "type" => SymbolKind::Type,
            "mod" => SymbolKind::Mod,
            _ => continue,
        };
        return rest.get(i + 1).and_then(|t| ident(t)).map(|n| (kind, n));
    }
    None
}

/// Count word-boundary uses of `symbol` in `content`, returning `(prod, test)`
/// where `test` is uses on lines the `test_mask` marks as test code. The
/// symbol's own declaration line is excluded. Idents of ≤2 chars return
/// `(0, 0)` — matching single/double-letter names (the `PtePerms` `R`/`W`/`X`
/// flags) as words floods the count with noise; pass nothing to opt out (a
/// short-ident audit isn't worth the false positives). This over-counts on
/// name collisions (a like-named local or trait method), never under-counts.
pub fn count_callers(
    symbol: &str,
    content: &str,
    test_mask: &[bool],
    allow_short: bool,
) -> (usize, usize) {
    if !allow_short && symbol.len() <= 2 {
        return (0, 0);
    }
    let mut prod = 0usize;
    let mut test = 0usize;
    for (i, line) in content.lines().enumerate() {
        if is_definition_line(line, symbol) {
            continue;
        }
        let n = count_word_occurrences(line, symbol);
        if test_mask.get(i).copied().unwrap_or(false) {
            test += n;
        } else {
            prod += n;
        }
    }
    (prod, test)
}

fn is_definition_line(line: &str, symbol: &str) -> bool {
    matches!(parse_pub_line(line), Some((_, name)) if name == symbol)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn count_word_occurrences(haystack: &str, word: &str) -> usize {
    let bytes = haystack.as_bytes();
    haystack
        .match_indices(word)
        .filter(|(pos, _)| {
            let before_ok = *pos == 0 || !is_ident_byte(bytes[*pos - 1]);
            let after = *pos + word.len();
            let after_ok = after >= bytes.len() || !is_ident_byte(bytes[after]);
            before_ok && after_ok
        })
        .count()
}

/// Debt/attention markers the report surfaces verbatim with their 1-based line.
const MARKERS: [&str; 8] =
    ["TODO", "FIXME", "HACK", "XXX", "#[allow", "#[expect", "dead_code", "stub"];

/// Find lines containing any [`MARKERS`] token. Returns `(line, trimmed_text)`.
/// Pure substring scan — a marker word inside a string literal counts too; the
/// report is for human eyes, so an occasional over-match is harmless.
pub fn scan_markers(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| MARKERS.iter().any(|m| line.contains(m)))
        .map(|(i, line)| (i + 1, line.trim().to_string()))
        .collect()
}

/// Parse `cargo machete` stdout into `(crate, dep)` pairs of unused
/// dependencies. The output groups deps under a `<crate> -- <manifest>:`
/// header, each dep on its own tab-indented line; the trailing explanatory
/// paragraph is neither a header nor indented, so it's ignored.
pub fn parse_machete(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in stdout.lines() {
        if let Some(krate) = machete_header_crate(line) {
            current = Some(krate);
        } else if line.starts_with([' ', '\t']) {
            let dep = line.trim();
            if let Some(krate) = &current {
                if !dep.is_empty() {
                    out.push((krate.clone(), dep.to_string()));
                }
            }
        } else if !line.is_empty() {
            current = None;
        }
    }
    out
}

/// `kernel-core -- kernel-core/Cargo.toml:` -> `Some("kernel-core")`.
fn machete_header_crate(line: &str) -> Option<String> {
    if line.starts_with(char::is_whitespace) || !line.ends_with(':') {
        return None;
    }
    let (krate, rest) = line.split_once(" -- ")?;
    rest.ends_with("Cargo.toml:").then(|| krate.trim().to_string())
}

/// Leading identifier of a token: `Wrap<T>` -> `Wrap`, `E:` -> `E`, `g{}` -> `g`.
/// `None` if the token doesn't start with an identifier char.
fn ident(tok: &str) -> Option<String> {
    let name: String = tok
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(src: &str) -> Vec<PubSymbol> {
        extract_pub_symbols(src, &test_line_mask(src))
    }

    #[test]
    fn extracts_each_kind() {
        let src = "\
pub fn a() {}
pub struct B {}
pub enum C {}
pub trait D {}
pub const E: usize = 1;
pub type F = u8;
pub mod g {}
";
        let syms = extract(src);
        let got: Vec<(&str, SymbolKind)> =
            syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("a", SymbolKind::Fn),
                ("B", SymbolKind::Struct),
                ("C", SymbolKind::Enum),
                ("D", SymbolKind::Trait),
                ("E", SymbolKind::Const),
                ("F", SymbolKind::Type),
                ("g", SymbolKind::Mod),
            ],
        );
    }

    #[test]
    fn records_one_based_line() {
        let src = "\n\npub fn here() {}\n";
        assert_eq!(extract(src)[0].line, 3);
    }

    #[test]
    fn skips_restricted_visibility() {
        let src = "\
pub(crate) fn a() {}
pub(super) struct B {}
pub(in crate::x) fn c() {}
fn d() {}
";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn skips_pub_items_inside_test_blocks() {
        let src = "\
pub fn real() {}

#[cfg(test)]
mod tests {
    pub fn helper() {}
    pub struct Fake {}
}
";
        let syms = extract(src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn const_fn_is_fn_not_const() {
        let src = "pub const fn masked(x: u64) -> usize { 0 }\n";
        let got = extract(src);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, SymbolKind::Fn);
        assert_eq!(got[0].name, "masked");
    }

    #[test]
    fn strips_generics_and_punctuation_from_name() {
        let src = "\
pub fn maps<T>(t: T) {}
pub struct Wrap<T> { inner: T }
";
        let syms = extract(src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["maps", "Wrap"]);
    }

    #[test]
    fn ignores_pub_inside_a_string_literal() {
        let src = "    let s = \"pub fn fake\";\n";
        assert!(extract(src).is_empty());
    }

    #[test]
    fn counts_word_boundary_uses_excluding_definition() {
        // `frames` appears: def line (excluded), one prod use, one as a
        // substring of `frames_free` (must NOT match — word boundary).
        let src = "\
pub fn frames() {}
fn caller() { frames(); }
fn other() { let n = frames_free(); }
";
        assert_eq!(count_callers("frames", src, &test_line_mask(src), false), (1, 0));
    }

    #[test]
    fn splits_prod_and_test_callers() {
        let src = "\
fn user() { thing(); }

#[cfg(test)]
mod tests {
    fn t() { thing(); thing(); }
}
";
        assert_eq!(count_callers("thing", src, &test_line_mask(src), false), (1, 2));
    }

    #[test]
    fn short_idents_are_skipped_by_default() {
        // `R` is a one-char perm flag — matching it as a word would count
        // every bare `R` in the file. Short idents return (0, 0).
        let src = "fn f() { let x = R | W; let y = R; }\n";
        assert_eq!(count_callers("R", src, &test_line_mask(src), false), (0, 0));
    }

    #[test]
    fn parse_machete_extracts_crate_and_dep() {
        // Verbatim `cargo machete kernel-core` output (cargo-machete 0.9.2).
        let out = "\
Analyzing dependencies of crates in kernel-core...
cargo-machete found the following unused dependencies in kernel-core:
kernel-core -- kernel-core/Cargo.toml:
\tspin

If you believe cargo-machete has detected an unused dependency incorrectly,
you can add the dependency to the list of dependencies to ignore in the
`[package.metadata.cargo-machete]` section of the appropriate Cargo.toml.
For example:

[package.metadata.cargo-machete]
ignored = [\"prost\"]

Done!
";
        assert_eq!(
            parse_machete(out),
            vec![("kernel-core".to_string(), "spin".to_string())],
        );
    }

    #[test]
    fn parse_machete_empty_when_clean() {
        let out = "\
Analyzing dependencies of crates in collector...
cargo-machete didn't find any unused dependencies in collector. Good job!
Done!
";
        assert!(parse_machete(out).is_empty());
    }

    #[test]
    fn build_reports_classifies_from_aggregated_counts() {
        let target = vec![(
            "frame.rs".to_string(),
            "\
pub fn used_ext() {}
pub fn used_int() {}
pub fn dead() {}
fn caller() { used_int(); }
"
            .to_string(),
        )];
        let siblings = vec!["fn k() { used_ext(); used_ext(); }".to_string()];

        let reports = build_reports(&target, &siblings, false);
        let by = |name: &str| reports.iter().find(|r| r.sym.name == name).unwrap().clone();

        assert_eq!(by("used_ext").ext, 2);
        assert_eq!(by("used_ext").verdict, Verdict::KeepPublic);
        assert_eq!((by("used_int").ext, by("used_int").int), (0, 1));
        assert_eq!(by("used_int").verdict, Verdict::DemotePubCrate);
        assert_eq!(by("dead").verdict, Verdict::DeadCandidate);
        assert_eq!(by("used_ext").file, "frame.rs");
    }

    #[test]
    fn scan_markers_finds_attrs_and_todos() {
        let src = "\
fn a() {}
    #[allow(clippy::should_implement_trait)]
fn b() {} // TODO: revisit
// FIXME later
let ok = 1;
#[expect(dead_code)]
";
        let marks = scan_markers(src);
        let hits: Vec<(usize, &str)> =
            marks.iter().map(|(n, t)| (*n, t.as_str())).collect();
        assert_eq!(
            hits,
            vec![
                (2, "#[allow(clippy::should_implement_trait)]"),
                (3, "fn b() {} // TODO: revisit"),
                (4, "// FIXME later"),
                (6, "#[expect(dead_code)]"),
            ],
        );
    }

    #[test]
    fn classify_covers_each_verdict() {
        assert_eq!(classify(3, 0, 0), Verdict::KeepPublic);
        assert_eq!(classify(0, 2, 5), Verdict::DemotePubCrate);
        assert_eq!(classify(0, 0, 4), Verdict::ProdUnusedTestOnly);
        assert_eq!(classify(0, 0, 0), Verdict::DeadCandidate);
    }

    #[test]
    fn handles_unsafe_and_extern_fn_modifiers() {
        let src = "\
pub unsafe trait Marker {}
pub extern \"C\" fn handler() {}
";
        let syms = extract(src);
        let got: Vec<(&str, SymbolKind)> =
            syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(
            got,
            vec![("Marker", SymbolKind::Trait), ("handler", SymbolKind::Fn)],
        );
    }
}
