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
}

#[cfg(test)]
mod tests {
    use super::*;

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
