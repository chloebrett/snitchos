//! Static (B1) target: the workspace crate graph. `parse_cargo_metadata`
//! turns `cargo metadata --format-version 1` JSON into `CrateNode`s;
//! `workspace_graph` projects those into a `Graph` of member-to-member edges.
//! Both are pure — xtask shells out to `cargo metadata` and feeds us the JSON.

use crate::model::{Direction, Graph};

/// One workspace member and the names of its declared dependencies (before
/// filtering to workspace-internal edges).
pub struct CrateNode {
    pub name: String,
    pub deps: Vec<String>,
}

/// Parse `cargo metadata --format-version 1` JSON into the workspace members.
/// A package is a member iff its `id` appears in `workspace_members`;
/// dependency names are captured verbatim (the member-vs-external filter is
/// `workspace_graph`'s job, keeping this function a faithful projection).
pub fn parse_cargo_metadata(json: &str) -> Result<Vec<CrateNode>, serde_json::Error> {
    let root: serde_json::Value = serde_json::from_str(json)?;

    let members: std::collections::HashSet<&str> = root["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();

    let mut nodes: Vec<CrateNode> = root["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pkg| pkg["id"].as_str().is_some_and(|id| members.contains(id)))
        .map(|pkg| CrateNode {
            name: pkg["name"].as_str().unwrap_or_default().to_string(),
            deps: pkg["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|dep| dep["name"].as_str())
                .map(str::to_string)
                .collect(),
        })
        .collect();

    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(nodes)
}

/// Project workspace members into a flowchart. Only edges whose target is
/// itself a workspace member survive (external crates like `clap`/`serde` are
/// dropped); node ids are the crate names with `-` sanitized to `_` so mermaid
/// doesn't misparse hyphens, while labels keep the real name. `layer_of` groups
/// each crate into a named cluster (kernel / userspace / …) — `None` leaves a
/// crate ungrouped. The layer mapping is editorial, so it lives with the caller.
pub fn workspace_graph(
    members: &[CrateNode],
    layer_of: impl Fn(&str) -> Option<String>,
) -> Graph {
    let is_member = |name: &str| members.iter().any(|m| m.name == name);
    let sanitize = |name: &str| name.replace('-', "_");

    let mut graph = Graph::new(Direction::LeftRight);
    for member in members {
        let id = sanitize(&member.name);
        match layer_of(&member.name) {
            Some(layer) => graph.node_in(&id, &member.name, &layer),
            None => graph.node(&id, &member.name),
        }
    }
    let mut seen = std::collections::HashSet::new();
    for member in members {
        for dep in member.deps.iter().filter(|d| is_member(d)) {
            if seen.insert((&member.name, dep)) {
                graph.edge(&sanitize(&member.name), &sanitize(dep));
            }
        }
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, deps: &[&str]) -> CrateNode {
        CrateNode {
            name: name.to_string(),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    #[test]
    fn keeps_only_edges_between_workspace_members() {
        let members = vec![
            node("xtask", &["diagram", "clap"]),
            node("diagram", &["protocol"]),
            node("protocol", &[]),
        ];
        let expected = "\
graph LR
    xtask[\"xtask\"]
    diagram[\"diagram\"]
    protocol[\"protocol\"]
    xtask --> diagram
    diagram --> protocol
";
        assert_eq!(workspace_graph(&members, |_| None).to_mermaid(), expected);
    }

    #[test]
    fn groups_crates_into_layer_subgraphs() {
        let members = vec![node("kernel", &["kernel-core"]), node("kernel-core", &[]), node("xtask", &[])];
        let layer = |name: &str| match name {
            "kernel" | "kernel-core" => Some("kernel".to_string()),
            _ => None,
        };
        let mermaid = workspace_graph(&members, layer).to_mermaid();
        assert!(mermaid.contains("subgraph kernel"), "kernel crates clustered");
        assert!(mermaid.contains("        kernel[\"kernel\"]"), "kernel node inside the subgraph");
        assert!(mermaid.contains("    xtask[\"xtask\"]"), "ungrouped crate stays top-level");
    }

    #[test]
    fn parses_member_names_and_deps_dropping_non_members() {
        let json = r#"{
            "packages": [
                { "id": "path+file:///w/xtask#0.0.1", "name": "xtask",
                  "dependencies": [ {"name":"diagram"}, {"name":"clap"} ] },
                { "id": "path+file:///w/diagram#0.0.1", "name": "diagram",
                  "dependencies": [ {"name":"protocol"} ] },
                { "id": "registry+https://r/clap#4.0.0", "name": "clap",
                  "dependencies": [] }
            ],
            "workspace_members": [
                "path+file:///w/xtask#0.0.1",
                "path+file:///w/diagram#0.0.1"
            ]
        }"#;
        let members = parse_cargo_metadata(json).expect("valid metadata");

        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["diagram", "xtask"], "members sorted by name for stable output");

        let xtask = members.iter().find(|m| m.name == "xtask").expect("xtask present");
        assert_eq!(xtask.deps, vec!["diagram".to_string(), "clap".to_string()]);
    }

    #[test]
    fn de_duplicates_repeated_edges() {
        // A crate can list the same dependency twice (e.g. a normal and a dev
        // dependency); cargo metadata reports both, but the graph shows one edge.
        let members = vec![node("hitch", &["hitch-pod", "hitch-pod"]), node("hitch-pod", &[])];
        let expected = "\
graph LR
    hitch[\"hitch\"]
    hitch_pod[\"hitch-pod\"]
    hitch --> hitch_pod
";
        assert_eq!(workspace_graph(&members, |_| None).to_mermaid(), expected);
    }

    #[test]
    fn sanitizes_hyphenated_ids_but_keeps_real_labels() {
        let members = vec![node("xtask", &["kernel-core"]), node("kernel-core", &[])];
        let expected = "\
graph LR
    xtask[\"xtask\"]
    kernel_core[\"kernel-core\"]
    xtask --> kernel_core
";
        assert_eq!(workspace_graph(&members, |_| None).to_mermaid(), expected);
    }
}
