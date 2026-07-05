//! Static (B1) target: the integration-test scenario/workload matrix. Projects
//! the itest catalog into a table so coverage is legible — which workload each
//! scenario boots, its profile, its tags. Pure: xtask reads its `SCENARIOS`
//! registry and maps each entry into a `ScenarioMeta`.

use crate::model::Table;

/// One itest scenario's metadata, decoupled from `itest_harness::Scenario` so
/// this crate needn't depend on the harness. `workload == None` is the default
/// `init` boot (no `workload=` bootarg).
pub struct ScenarioMeta {
    pub name: String,
    pub workload: Option<String>,
    pub tags: Vec<String>,
    pub cpu_bound: bool,
}

fn workload_display(meta: &ScenarioMeta) -> String {
    meta.workload.clone().unwrap_or_else(|| "init (default)".to_string())
}

/// Project scenarios into a table sorted by (workload, name) so scenarios
/// sharing a boot sit together. Columns: Scenario, Workload, Profile, Tags.
pub fn matrix_table(scenarios: &[ScenarioMeta]) -> Table {
    let mut sorted: Vec<&ScenarioMeta> = scenarios.iter().collect();
    sorted.sort_by_key(|meta| (workload_display(meta), meta.name.clone()));

    let mut table = Table::new(&["Scenario", "Workload", "Profile", "Tags"]);
    for meta in sorted {
        let workload = workload_display(meta);
        let profile = if meta.cpu_bound { "cpu" } else { "wfi" };
        let tags = meta.tags.join(", ");
        table.row(&[&meta.name, &workload, profile, &tags]);
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, workload: Option<&str>, tags: &[&str], cpu_bound: bool) -> ScenarioMeta {
        ScenarioMeta {
            name: name.to_string(),
            workload: workload.map(str::to_string),
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            cpu_bound,
        }
    }

    #[test]
    fn tabulates_scenarios_sorted_by_workload_then_name() {
        let scenarios = vec![
            meta("spawn-storm", Some("spawn-storm"), &["smp", "stress"], true),
            meta("boot-reaches-heartbeat", Some("demo"), &["boot"], false),
            meta("default-boot-starts-init", None, &["boot"], false),
        ];
        let expected = "\
| Scenario | Workload | Profile | Tags |
| --- | --- | --- | --- |
| boot-reaches-heartbeat | demo | wfi | boot |
| default-boot-starts-init | init (default) | wfi | boot |
| spawn-storm | spawn-storm | cpu | smp, stress |
";
        assert_eq!(matrix_table(&scenarios).to_markdown(), expected);
    }
}
