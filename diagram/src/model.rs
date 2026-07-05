//! Typed diagram values with mermaid emitters. A target builds a `Graph`
//! (or, later, a `Sequence`/class model) and calls `to_mermaid()`; the model
//! is the testable seam, so tests assert on the emitted string rather than on
//! a boot or a `cargo metadata` shell-out.

/// Flow direction for a mermaid `graph` header.
pub enum Direction {
    LeftRight,
    TopDown,
}

/// A directed graph rendered as a mermaid `graph` (flowchart). Nodes keep
/// insertion order so the emitted mermaid is deterministic and diffable.
pub struct Graph {
    direction: Direction,
    nodes: Vec<(String, String)>,
    edges: Vec<(String, String)>,
}

impl Graph {
    pub fn new(direction: Direction) -> Self {
        Self { direction, nodes: Vec::new(), edges: Vec::new() }
    }

    pub fn node(&mut self, id: &str, label: &str) {
        self.nodes.push((id.to_string(), label.to_string()));
    }

    pub fn edge(&mut self, from: &str, to: &str) {
        self.edges.push((from.to_string(), to.to_string()));
    }

    pub fn to_mermaid(&self) -> String {
        let header = match self.direction {
            Direction::LeftRight => "graph LR",
            Direction::TopDown => "graph TD",
        };
        let nodes = self.nodes.iter().map(|(id, label)| format!("    {id}[\"{label}\"]"));
        let edges = self.edges.iter().map(|(from, to)| format!("    {from} --> {to}"));
        std::iter::once(header.to_string())
            .chain(nodes)
            .chain(edges)
            .map(|line| line + "\n")
            .collect()
    }

    pub fn to_dot(&self) -> String {
        let rankdir = match self.direction {
            Direction::LeftRight => "LR",
            Direction::TopDown => "TB",
        };
        let nodes = self.nodes.iter().map(|(id, label)| format!("    \"{id}\" [label=\"{label}\"];"));
        let edges = self.edges.iter().map(|(from, to)| format!("    \"{from}\" -> \"{to}\";"));
        std::iter::once(format!("digraph {{\n    rankdir={rankdir};"))
            .chain(nodes)
            .chain(edges)
            .chain(std::iter::once("}".to_string()))
            .map(|line| line + "\n")
            .collect()
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
