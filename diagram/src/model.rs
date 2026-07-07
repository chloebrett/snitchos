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
