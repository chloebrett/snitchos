//! Per-scenario **instret baseline** for the snemu engine — a deterministic
//! perf-regression gate.
//!
//! This is *not* the flake baseline (`itest/baseline.rs` + `itest-harness`).
//! That one tracks QEMU failure *rates* — Wilson scores over runs — and stays,
//! both for the `--engine qemu` escape hatch and as an open-sourcing candidate.
//! Under the deterministic snemu engine a failure rate is always 0 or 1, so
//! there's nothing to estimate. What *is* worth watching there is **guest
//! instructions retired per scenario**: a deterministic number (an exact re-run
//! reproduces every value) that *grows* when the kernel gets slower on that path.
//!
//! So this baseline records `scenario → instret`, renders it as a Prometheus
//! textfile (for `node_exporter --collector.textfile`), and compares a fresh run
//! against a recorded one to flag regressions past a tolerance.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The Prometheus metric name for a scenario's guest-instret figure.
const METRIC: &str = "snitchos_itest_scenario_instret";

/// What a run should do with its per-scenario instret, from the `--record-instret`
/// / `--check-instret` flags (mutually exclusive at the CLI). `None` when neither
/// is set — the ordinary run does nothing extra.
#[derive(Debug, Clone, PartialEq)]
pub enum InstretGate {
    /// Write this run's instret to the path as the new baseline.
    Record(PathBuf),
    /// Compare this run against the baseline at the path; regressions past
    /// `tolerance` (fractional) fail the run.
    Check { path: PathBuf, tolerance: f64 },
}

/// A recorded per-scenario guest-instret baseline: scenario name → instructions
/// retired under snemu. `BTreeMap` so rendering + diffing are order-stable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstretBaseline {
    entries: BTreeMap<String, u64>,
}

/// One scenario whose current instret exceeds its recorded baseline past the
/// tolerance — a perf regression to investigate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regression {
    pub scenario: String,
    pub baseline: u64,
    pub current: u64,
}

impl Regression {
    /// The growth as a percentage of the baseline (baseline is never 0 for a
    /// scenario that ran, so the division is safe).
    #[must_use]
    pub fn pct(&self) -> f64 {
        ((self.current as f64 - self.baseline as f64) / self.baseline as f64) * 100.0
    }
}

impl InstretBaseline {
    /// Build from `(scenario, instret)` pairs. A later pair for the same scenario
    /// overwrites an earlier one (there is one instret per scenario per run).
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, u64)>) -> Self {
        Self { entries: pairs.into_iter().collect() }
    }

    /// On-disk form: `<scenario>\t<instret>` per line, sorted by name. Distinct
    /// from [`render_prometheus`](Self::render_prometheus) — this round-trips via
    /// [`parse_storage`](Self::parse_storage) for the record→check gate, where the
    /// Prometheus output is one-way scrape data.
    #[must_use]
    pub fn to_storage(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (scenario, instret) in &self.entries {
            let _ = writeln!(out, "{scenario}\t{instret}");
        }
        out
    }

    /// Parse [`to_storage`](Self::to_storage) output. Blank/whitespace-only lines
    /// are skipped; any other line without exactly one tab-separated `u64` count
    /// is a hard error (a corrupt baseline must fail loudly, not silently drop a
    /// scenario from the gate).
    pub fn parse_storage(text: &str) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let (name, count) = line
                .split_once('\t')
                .ok_or_else(|| format!("instret baseline: no tab in line {line:?}"))?;
            let instret = count
                .trim()
                .parse::<u64>()
                .map_err(|e| format!("instret baseline: bad count in {line:?}: {e}"))?;
            entries.insert(name.to_string(), instret);
        }
        Ok(Self { entries })
    }

    /// Scenarios whose `current` instret exceeds their recorded baseline by more
    /// than `tolerance` (fractional — `0.05` = 5%), sorted by scenario name.
    ///
    /// - A scenario **absent from the baseline** is ignored: a gate flags
    ///   regressions in known scenarios, not the arrival of new ones (those are
    ///   recorded on the next `--record`).
    /// - A scenario that got **faster** is never a regression.
    /// - `tolerance` absorbs the legitimate drift of ordinary kernel changes;
    ///   `0.0` makes any increase a regression (useful in a test).
    #[must_use]
    pub fn regressions(&self, current: &InstretBaseline, tolerance: f64) -> Vec<Regression> {
        self.entries
            .iter()
            .filter_map(|(scenario, &baseline)| {
                let &now = current.entries.get(scenario)?;
                let ceiling = (baseline as f64) * (1.0 + tolerance);
                (now as f64 > ceiling).then(|| Regression {
                    scenario: scenario.clone(),
                    baseline,
                    current: now,
                })
            })
            .collect()
    }

    /// Prometheus textfile format: one `HELP`/`TYPE` header, then one gauge
    /// sample per scenario, sorted by name (`BTreeMap` iteration order) so the
    /// output is byte-stable for a given input.
    #[must_use]
    pub fn render_prometheus(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP {METRIC} Guest instructions retired for an itest scenario under snemu.",
        );
        let _ = writeln!(out, "# TYPE {METRIC} gauge");
        for (scenario, instret) in &self.entries {
            let _ = writeln!(out, "{METRIC}{{scenario=\"{scenario}\"}} {instret}");
        }
        out
    }
}

/// Apply an [`InstretGate`] to a completed run's `current` baseline. Records
/// (writes the file) or checks (reads + diffs), printing to stderr. Returns
/// `false` only when a check found a regression — the caller folds that into the
/// run's exit code. File/parse errors are reported and treated as non-fatal
/// (`true`): the perf gate must never mask a green functional run.
pub fn apply_gate(gate: &InstretGate, current: &InstretBaseline) -> bool {
    match gate {
        InstretGate::Record(path) => {
            match std::fs::write(path, current.to_storage()) {
                Ok(()) => eprintln!(
                    "instret: recorded {} scenario(s) to {}",
                    current.entries.len(),
                    path.display(),
                ),
                Err(e) => eprintln!("instret: could not write {}: {e}", path.display()),
            }
            true
        }
        InstretGate::Check { path, tolerance } => {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("instret: could not read baseline {}: {e}", path.display());
                    return true;
                }
            };
            let baseline = match InstretBaseline::parse_storage(&text) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("instret: {e}");
                    return true;
                }
            };
            let regs = baseline.regressions(current, *tolerance);
            if regs.is_empty() {
                eprintln!(
                    "instret: no regression past {:.0}% vs {}",
                    tolerance * 100.0,
                    path.display(),
                );
                return true;
            }
            eprintln!(
                "instret: {} scenario(s) regressed past {:.0}% vs {}:",
                regs.len(),
                tolerance * 100.0,
                path.display(),
            );
            for r in &regs {
                eprintln!(
                    "  {:<40} {} → {} (+{:.1}%)",
                    r.scenario, r.baseline, r.current, r.pct(),
                );
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regressions_flags_only_scenarios_grown_past_the_tolerance() {
        let base = InstretBaseline::from_pairs([
            ("steady".to_string(), 1_000_000),
            ("slower".to_string(), 1_000_000),
            ("faster".to_string(), 1_000_000),
            ("noise".to_string(), 1_000_000),
        ]);
        let current = InstretBaseline::from_pairs([
            ("steady".to_string(), 1_000_000), // unchanged → not a regression
            ("slower".to_string(), 1_200_000), // +20% → regression at 5% tol
            ("faster".to_string(), 800_000),   // faster → never a regression
            ("noise".to_string(), 1_030_000),  // +3% → under 5% tol, ignored
            ("brand-new".to_string(), 9_999),  // absent from baseline → ignored
        ]);

        let regs = base.regressions(&current, 0.05);
        assert_eq!(regs.len(), 1, "only `slower` should trip");
        assert_eq!(regs[0].scenario, "slower");
        assert_eq!(regs[0].baseline, 1_000_000);
        assert_eq!(regs[0].current, 1_200_000);
        assert!((regs[0].pct() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn storage_round_trips() {
        let bl = InstretBaseline::from_pairs([
            ("b".to_string(), 20),
            ("a".to_string(), 10),
        ]);
        let text = bl.to_storage();
        assert_eq!(InstretBaseline::parse_storage(&text).expect("parses"), bl);
        // Stable + sorted: `a` before `b`.
        assert!(text.find("a\t").unwrap() < text.find("b\t").unwrap());
    }

    #[test]
    fn parse_storage_rejects_a_malformed_line() {
        assert!(InstretBaseline::parse_storage("scenario-without-a-count").is_err());
        assert!(InstretBaseline::parse_storage("scenario\tnot-a-number").is_err());
        // Blank lines and trailing whitespace are tolerated.
        assert!(InstretBaseline::parse_storage("a\t1\n\n  \nb\t2\n").is_ok());
    }

    #[test]
    fn apply_gate_check_returns_false_only_on_a_real_regression() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("instret-test-{}.base", std::process::id()));
        let recorded = InstretBaseline::from_pairs([("s".to_string(), 1_000_000)]);
        std::fs::write(&path, recorded.to_storage()).expect("write baseline");

        // Clean re-run → gate passes (true).
        let same = InstretBaseline::from_pairs([("s".to_string(), 1_000_000)]);
        let check = InstretGate::Check { path: path.clone(), tolerance: 0.05 };
        assert!(apply_gate(&check, &same), "identical run must pass the gate");

        // Regression → gate fails (false).
        let grown = InstretBaseline::from_pairs([("s".to_string(), 2_000_000)]);
        assert!(!apply_gate(&check, &grown), "a +100% scenario must fail the gate");

        // A missing baseline file is reported but non-fatal (true) — the perf gate
        // must never mask a green functional run.
        let missing = InstretGate::Check { path: dir.join("nope.base"), tolerance: 0.05 };
        assert!(apply_gate(&missing, &grown), "unreadable baseline must not fail the run");

        // Record always passes (true) and writes the file.
        let rec_path = dir.join(format!("instret-rec-{}.base", std::process::id()));
        assert!(apply_gate(&InstretGate::Record(rec_path.clone()), &grown));
        assert_eq!(
            InstretBaseline::parse_storage(&std::fs::read_to_string(&rec_path).unwrap()).unwrap(),
            grown,
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&rec_path);
    }

    /// A gate only fires on regression: same tree → same instret → no findings.
    #[test]
    fn an_identical_run_has_no_regressions() {
        let base = InstretBaseline::from_pairs([("a".to_string(), 42), ("b".to_string(), 7)]);
        assert!(base.regressions(&base.clone(), 0.0).is_empty());
    }

    #[test]
    fn render_prometheus_emits_one_stable_gauge_per_scenario() {
        // Insertion order deliberately not sorted — the render must sort.
        let bl = InstretBaseline::from_pairs([
            ("heartbeat-cadence".to_string(), 20_000_000),
            ("boot-reaches-heartbeat".to_string(), 10_000_000),
        ]);
        let out = bl.render_prometheus();

        // Exactly one HELP and one TYPE line.
        assert_eq!(out.matches(&format!("# HELP {METRIC} ")).count(), 1);
        assert_eq!(out.matches(&format!("# TYPE {METRIC} gauge")).count(), 1);

        // Both scenarios present, as labelled gauges, sorted by name.
        let boot = out.find("boot-reaches-heartbeat").expect("boot present");
        let beat = out.find("heartbeat-cadence").expect("cadence present");
        assert!(boot < beat, "scenarios must render sorted by name");
        assert!(out.contains(
            "snitchos_itest_scenario_instret{scenario=\"boot-reaches-heartbeat\"} 10000000"
        ));

        // Byte-stable: same input → identical output (the whole point of a gate).
        assert_eq!(out, bl.render_prometheus());
    }
}
