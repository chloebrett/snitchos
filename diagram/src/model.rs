//! Typed diagram values with mermaid emitters. A target builds a `Graph`
//! (or, later, a `Sequence`/class model) and calls `to_mermaid()`; the model
//! is the testable seam, so tests assert on the emitted string rather than on
//! a boot or a `cargo metadata` shell-out.

/// Flow direction for a mermaid `graph` header.
pub enum Direction {
    LeftRight,
    TopDown,
}

struct Node {
    id: String,
    label: String,
    classes: Vec<String>,
    group: Option<String>,
}

struct Edge {
    from: String,
    to: String,
    label: Option<String>,
}

/// A named style shared by nodes carrying its name — a mermaid `classDef` plus
/// the equivalent DOT node attributes, so roots (etc.) look the same in both
/// backends.
struct ClassDef {
    name: String,
    mermaid: String,
    dot: Vec<(String, String)>,
}

/// `s` as a quoted JSON string.
///
/// Hand-written rather than `serde_json`, to match how this module already emits
/// mermaid and DOT: the model is the testable seam and the renderers are string
/// builders. Escaping is not optional here — labels carry *guest* data (a task name,
/// a capability descriptor), so a quote or a backslash arriving from the wire must not
/// be able to produce JSON the panel cannot parse.
fn quoted(s: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below 0x20 must be escaped for the output to be valid JSON.
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String cannot fail");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A directed graph rendered as a mermaid `graph` (flowchart) or DOT digraph.
/// Nodes keep insertion order so the emitted output is deterministic and
/// diffable. Nodes may carry style classes defined via [`Graph::define_class`].
pub struct Graph {
    direction: Direction,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    classes: Vec<ClassDef>,
}

impl Graph {
    pub fn new(direction: Direction) -> Self {
        Self { direction, nodes: Vec::new(), edges: Vec::new(), classes: Vec::new() }
    }

    pub fn node(&mut self, id: &str, label: &str) {
        self.push_node(id, label, &[], None);
    }

    pub fn node_classed(&mut self, id: &str, label: &str, classes: &[&str]) {
        self.push_node(id, label, classes, None);
    }

    /// Add a node inside a named subgraph/cluster `group`. Nodes sharing a group
    /// are boxed together (mermaid `subgraph`, DOT `cluster_*`); groups render in
    /// first-appearance order.
    pub fn node_in(&mut self, id: &str, label: &str, group: &str) {
        self.push_node(id, label, &[], Some(group));
    }

    fn push_node(&mut self, id: &str, label: &str, classes: &[&str], group: Option<&str>) {
        self.nodes.push(Node {
            id: id.to_string(),
            label: label.to_string(),
            classes: classes.iter().map(|c| (*c).to_string()).collect(),
            group: group.map(str::to_string),
        });
    }

    pub fn edge(&mut self, from: &str, to: &str) {
        self.edges.push(Edge { from: from.to_string(), to: to.to_string(), label: None });
    }

    pub fn edge_labeled(&mut self, from: &str, to: &str, label: &str) {
        self.edges.push(Edge {
            from: from.to_string(),
            to: to.to_string(),
            label: Some(label.to_string()),
        });
    }

    /// Register the conventional `root` style — a light-blue fill the runtime
    /// graphs share to highlight entry points (caps root grants, trace top-level
    /// spans). Nodes opt in via the class name `"root"`.
    pub(crate) fn define_root_class(&mut self) {
        self.define_class(
            "root",
            "fill:#dae8fc,stroke:#6c8ebf",
            &[("style", "filled"), ("fillcolor", "#dae8fc")],
        );
    }

    /// Register a style class: `mermaid` is the `classDef` body (e.g.
    /// `fill:#dae8fc,stroke:#6c8ebf`); `dot` is the equivalent DOT node
    /// attributes (e.g. `[("style", "filled"), ("fillcolor", "#dae8fc")]`).
    pub fn define_class(&mut self, name: &str, mermaid: &str, dot: &[(&str, &str)]) {
        self.classes.push(ClassDef {
            name: name.to_string(),
            mermaid: mermaid.to_string(),
            dot: dot.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        });
    }

    /// Groups (subgraph names) in first-appearance order across the nodes.
    fn group_order(&self) -> Vec<&str> {
        let mut groups: Vec<&str> = Vec::new();
        for group in self.nodes.iter().filter_map(|n| n.group.as_deref()) {
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups
    }

    /// The graph as JSON, for a consumer that draws it itself.
    ///
    /// The third renderer beside [`to_mermaid`](Self::to_mermaid) and
    /// [`to_dot`](Self::to_dot), and it exists for a different kind of consumer: those
    /// two emit a *picture*, this emits the structure. The browser panels need the
    /// structure because the argument for building them at all is interaction —
    /// hovering a capability to see its rights, selecting a span to follow it across a
    /// context switch — and a rendered image is exactly the thing Grafana already does
    /// well enough.
    ///
    /// Sharing this one model is what keeps a live panel and its committed `.md`
    /// counterpart describing the same graph. Two folds would drift; one fold with
    /// three renderers cannot.
    #[must_use]
    pub fn to_json(&self) -> String {
        let direction = match self.direction {
            Direction::LeftRight => "LR",
            Direction::TopDown => "TD",
        };

        let nodes: Vec<String> = self
            .nodes
            .iter()
            .map(|n| {
                let classes: Vec<String> =
                    n.classes.iter().map(|c| quoted(c)).collect();
                let group = n.group.as_ref().map_or_else(|| "null".to_string(), |g| quoted(g));
                format!(
                    r#"{{"id":{},"label":{},"classes":[{}],"group":{group}}}"#,
                    quoted(&n.id),
                    quoted(&n.label),
                    classes.join(","),
                )
            })
            .collect();

        let edges: Vec<String> = self
            .edges
            .iter()
            .map(|e| {
                let label =
                    e.label.as_ref().map_or_else(|| "null".to_string(), |l| quoted(l));
                format!(
                    r#"{{"from":{},"to":{},"label":{label}}}"#,
                    quoted(&e.from),
                    quoted(&e.to),
                )
            })
            .collect();

        format!(
            r#"{{"direction":"{direction}","nodes":[{}],"edges":[{}]}}"#,
            nodes.join(","),
            edges.join(","),
        )
    }

    pub fn to_mermaid(&self) -> String {
        let header = match self.direction {
            Direction::LeftRight => "graph LR",
            Direction::TopDown => "graph TD",
        };
        let node_line = |n: &Node, indent: &str| format!("{indent}{}[\"{}\"]", n.id, n.label);

        let mut lines = vec![header.to_string()];
        for group in self.group_order() {
            lines.push(format!("    subgraph {group}"));
            for n in self.nodes.iter().filter(|n| n.group.as_deref() == Some(group)) {
                lines.push(node_line(n, "        "));
            }
            lines.push("    end".to_string());
        }
        for n in self.nodes.iter().filter(|n| n.group.is_none()) {
            lines.push(node_line(n, "    "));
        }
        for e in &self.edges {
            lines.push(match &e.label {
                Some(label) => format!("    {} -->|{label}| {}", e.from, e.to),
                None => format!("    {} --> {}", e.from, e.to),
            });
        }
        for c in &self.classes {
            lines.push(format!("    classDef {} {};", c.name, c.mermaid));
        }
        for c in &self.classes {
            let ids: Vec<&str> = self
                .nodes
                .iter()
                .filter(|n| n.classes.contains(&c.name))
                .map(|n| n.id.as_str())
                .collect();
            if !ids.is_empty() {
                lines.push(format!("    class {} {};", ids.join(","), c.name));
            }
        }
        lines.join("\n") + "\n"
    }

    pub fn to_dot(&self) -> String {
        let rankdir = match self.direction {
            Direction::LeftRight => "LR",
            Direction::TopDown => "TB",
        };
        let node_line = |n: &Node, indent: &str| {
            let attrs: Vec<String> = n
                .classes
                .iter()
                .filter_map(|cn| self.classes.iter().find(|c| c.name == *cn))
                .flat_map(|c| c.dot.iter())
                .map(|(k, v)| format!("{k}=\"{v}\""))
                .collect();
            let attrs =
                if attrs.is_empty() { String::new() } else { format!(" {}", attrs.join(" ")) };
            format!("{indent}\"{}\" [label=\"{}\"{attrs}];", n.id, n.label)
        };

        let mut lines = vec![format!("digraph {{\n    rankdir={rankdir};")];
        for group in self.group_order() {
            lines.push(format!("    subgraph cluster_{group} {{"));
            lines.push(format!("        label=\"{group}\";"));
            for n in self.nodes.iter().filter(|n| n.group.as_deref() == Some(group)) {
                lines.push(node_line(n, "        "));
            }
            lines.push("    }".to_string());
        }
        for n in self.nodes.iter().filter(|n| n.group.is_none()) {
            lines.push(node_line(n, "    "));
        }
        for e in &self.edges {
            lines.push(match &e.label {
                Some(label) => format!("    \"{}\" -> \"{}\" [label=\"{label}\"];", e.from, e.to),
                None => format!("    \"{}\" -> \"{}\";", e.from, e.to),
            });
        }
        lines.push("}".to_string());
        lines.join("\n") + "\n"
    }
}

/// A markdown table — for tabular diagrams (e.g. the itest scenario/workload
/// matrix) that read better as a grid than as a node graph. Rows keep
/// insertion order so the emitted markdown is deterministic.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Self { headers: headers.iter().map(|h| (*h).to_string()).collect(), rows: Vec::new() }
    }

    pub fn row(&mut self, cells: &[&str]) {
        self.rows.push(cells.iter().map(|c| (*c).to_string()).collect());
    }

    pub fn to_markdown(&self) -> String {
        let render = |cells: &[String]| format!("| {} |", cells.join(" | "));
        let separator = vec!["---".to_string(); self.headers.len()];
        std::iter::once(render(&self.headers))
            .chain(std::iter::once(render(&separator)))
            .chain(self.rows.iter().map(|r| render(r)))
            .map(|line| line + "\n")
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a browser panel reads. A contract with TypeScript, which has no
    /// compiler to notice a renamed key — so it is pinned rather than described.
    #[test]
    fn serializes_nodes_and_edges_for_a_consumer_that_draws_its_own() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "Alpha");
        g.node("b", "Beta");
        g.edge_labeled("a", "b", "grants");

        assert_eq!(
            g.to_json(),
            r#"{"direction":"LR","nodes":[{"id":"a","label":"Alpha","classes":[],"group":null},{"id":"b","label":"Beta","classes":[],"group":null}],"edges":[{"from":"a","to":"b","label":"grants"}]}"#
        );
    }

    /// Groups and classes are how a fold says "these belong together" and "this one is
    /// a root". A renderer that cannot see them draws a flat, unstyled blob — the
    /// mermaid backend gets both, so the JSON one must too.
    #[test]
    fn serialization_carries_groups_and_classes() {
        let mut g = Graph::new(Direction::TopDown);
        g.define_class("root", "fill:#eee", &[("shape", "box")]);
        g.node_classed("r", "Root", &["root"]);
        g.node_in("c", "Child", "cluster one");

        let json = g.to_json();
        assert!(json.contains(r#""direction":"TD""#), "{json}");
        assert!(json.contains(r#""classes":["root"]"#), "{json}");
        assert!(json.contains(r#""group":"cluster one""#), "{json}");
    }

    /// An unlabelled edge is `null`, not `""` — the panel can then tell "no label"
    /// from "an empty one" without guessing, the same distinction `FrameView.name`
    /// keeps for unresolved names.
    #[test]
    fn an_unlabelled_edge_is_null_rather_than_empty() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "A");
        g.node("b", "B");
        g.edge("a", "b");

        assert!(g.to_json().contains(r#"{"from":"a","to":"b","label":null}"#));
    }

    /// Labels come from guest data — a task name, a capability descriptor — so they
    /// can contain characters that are not JSON-safe. Escaped, or the panel receives
    /// something it cannot parse.
    #[test]
    fn labels_are_escaped_so_guest_data_cannot_break_the_json() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", r#"say "hi"\now"#);

        let json = g.to_json();
        assert!(json.contains(r#"say \"hi\"#), "quotes escaped: {json}");
        // Round-trips, which is the property that actually matters.
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["nodes"][0]["label"], r#"say "hi"\now"#);
    }

    /// Control characters are escaped, or the whole payload is unparseable.
    ///
    /// JSON forbids a raw control character inside a string, so one arriving in a
    /// label — and labels are guest data: a task name read out of a NUL-padded byte
    /// array, a log line — would not corrupt *that node*, it would make `JSON.parse`
    /// throw and take the entire panel with it. Mutation testing found this: deleting
    /// the guard changed nothing any test could see.
    #[test]
    fn control_characters_in_guest_data_are_escaped() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "bell\u{7}and\u{1}start");

        let json = g.to_json();
        assert!(json.contains("\\u0007"), "escaped as a unicode escape: {json}");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["nodes"][0]["label"], "bell\u{7}and\u{1}start");
    }

    /// An empty graph is a normal state — a guest that has granted nothing yet — and
    /// must serialize to something the panel can render as "nothing here".
    #[test]
    fn an_empty_graph_serializes_to_empty_collections() {
        let json = Graph::new(Direction::LeftRight).to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(parsed["nodes"].as_array().map(Vec::len), Some(0));
        assert_eq!(parsed["edges"].as_array().map(Vec::len), Some(0));
    }

    /// The three renderers describe the same graph. If `to_json` ever disagreed with
    /// `to_mermaid` about what exists, the live panel and the committed diagram would
    /// quietly diverge — which is the one thing sharing the folds was meant to prevent.
    #[test]
    fn the_json_and_mermaid_renderers_agree_about_what_is_in_the_graph() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("one", "One");
        g.node("two", "Two");
        g.edge("one", "two");

        let mermaid = g.to_mermaid();
        let parsed: serde_json::Value = serde_json::from_str(&g.to_json()).expect("valid JSON");

        for node in parsed["nodes"].as_array().expect("nodes") {
            let label = node["label"].as_str().expect("a label");
            assert!(mermaid.contains(label), "mermaid is missing {label}: {mermaid}");
        }
        assert_eq!(parsed["edges"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn emits_a_markdown_table() {
        let mut t = Table::new(&["Scenario", "Workload"]);
        t.row(&["boot-reaches-heartbeat", "demo"]);
        t.row(&["default-boot-starts-init", "init (default)"]);

        let expected = "\
| Scenario | Workload |
| --- | --- |
| boot-reaches-heartbeat | demo |
| default-boot-starts-init | init (default) |
";
        assert_eq!(t.to_markdown(), expected);
    }

    #[test]
    fn grouped_nodes_render_as_subgraphs_in_both_backends() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node_in("a", "A", "kernel");
        g.node_in("b", "B", "host");
        g.node("c", "C");
        g.edge("a", "b");

        assert_eq!(
            g.to_mermaid(),
            "graph LR\n    subgraph kernel\n        a[\"A\"]\n    end\n    subgraph host\n        b[\"B\"]\n    end\n    c[\"C\"]\n    a --> b\n",
        );
        assert_eq!(
            g.to_dot(),
            "digraph {\n    rankdir=LR;\n    subgraph cluster_kernel {\n        label=\"kernel\";\n        \"a\" [label=\"A\"];\n    }\n    subgraph cluster_host {\n        label=\"host\";\n        \"b\" [label=\"B\"];\n    }\n    \"c\" [label=\"C\"];\n    \"a\" -> \"b\";\n}\n",
        );
    }

    #[test]
    fn labeled_edges_render_in_both_backends() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "A");
        g.node("b", "B");
        g.edge_labeled("a", "b", "42");

        assert_eq!(
            g.to_mermaid(),
            "graph LR\n    a[\"A\"]\n    b[\"B\"]\n    a -->|42| b\n",
        );
        assert_eq!(
            g.to_dot(),
            "digraph {\n    rankdir=LR;\n    \"a\" [label=\"A\"];\n    \"b\" [label=\"B\"];\n    \"a\" -> \"b\" [label=\"42\"];\n}\n",
        );
    }

    #[test]
    fn mermaid_emits_classdefs_and_assignments_for_styled_nodes() {
        let mut g = Graph::new(Direction::TopDown);
        g.define_class("root", "fill:#dae8fc,stroke:#6c8ebf", &[("style", "filled")]);
        g.node_classed("a", "Root", &["root"]);
        g.node("b", "Child");
        g.edge("a", "b");

        let expected = "\
graph TD
    a[\"Root\"]
    b[\"Child\"]
    a --> b
    classDef root fill:#dae8fc,stroke:#6c8ebf;
    class a root;
";
        assert_eq!(g.to_mermaid(), expected);
    }

    #[test]
    fn dot_merges_class_attributes_into_styled_nodes() {
        let mut g = Graph::new(Direction::TopDown);
        g.define_class("root", "unused-here", &[("style", "filled"), ("fillcolor", "#dae8fc")]);
        g.node_classed("a", "Root", &["root"]);
        g.node("b", "Child");

        let expected = "\
digraph {
    rankdir=TB;
    \"a\" [label=\"Root\" style=\"filled\" fillcolor=\"#dae8fc\"];
    \"b\" [label=\"Child\"];
}
";
        assert_eq!(g.to_dot(), expected);
    }

    #[test]
    fn emits_a_dot_digraph_for_local_graphviz_rendering() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "Crate A");
        g.node("b", "Crate B");
        g.edge("a", "b");

        let expected = "\
digraph {
    rankdir=LR;
    \"a\" [label=\"Crate A\"];
    \"b\" [label=\"Crate B\"];
    \"a\" -> \"b\";
}
";
        assert_eq!(g.to_dot(), expected);
    }

    #[test]
    fn emits_a_flowchart_with_labelled_nodes_and_edges() {
        let mut g = Graph::new(Direction::LeftRight);
        g.node("a", "Crate A");
        g.node("b", "Crate B");
        g.edge("a", "b");

        let expected = "\
graph LR
    a[\"Crate A\"]
    b[\"Crate B\"]
    a --> b
";
        assert_eq!(g.to_mermaid(), expected);
    }
}
