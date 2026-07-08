//! Runtime (B2) target: the span call-graph. Folds `SpanStart`/`SpanEnd` frames
//! into a graph of span *names* (resolved via `StringRegister`) linked
//! parent → child — collapsed by name so repeated instances (every heartbeat,
//! every task tick) stay one node. Edges are labelled with occurrence count;
//! root spans (`parent == SpanId(0)`) are styled. Pure — xtask sources frames.

use std::collections::{HashMap, HashSet};

use protocol::stream::OwnedFrame;

use crate::fold::OrderedCounter;
use crate::model::{Direction, Graph};

fn node_id(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

pub fn span_call_graph(frames: &[OwnedFrame]) -> Graph {
    let strings: HashMap<u32, &str> = frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::StringRegister { id, value } => Some((id.0, value.as_str())),
            _ => None,
        })
        .collect();

    // span id → its name, so a child can resolve its parent's name.
    let mut span_name: HashMap<u64, &str> = HashMap::new();
    for frame in frames {
        if let OwnedFrame::SpanStart { id, name_id, .. } = frame
            && let Some(name) = strings.get(&name_id.0)
        {
            span_name.insert(id.0, name);
        }
    }

    let mut graph = Graph::new(Direction::TopDown);
    graph.define_root_class();

    // Node count = how many times each span opened (the profiling signal that
    // makes even a top-level, unparented span informative, given SnitchOS spans
    // are mostly flat). Edges are parent→child nesting with their own counts.
    let mut nodes: OrderedCounter<&str> = OrderedCounter::new();
    let mut roots: HashSet<&str> = HashSet::new();
    let mut edges: OrderedCounter<(&str, &str)> = OrderedCounter::new();

    for frame in frames {
        let OwnedFrame::SpanStart { parent, name_id, .. } = frame else {
            continue;
        };
        let Some(name) = strings.get(&name_id.0).copied() else {
            continue;
        };
        nodes.add(name);
        if parent.0 == 0 {
            roots.insert(name);
        } else if let Some(parent_name) = span_name.get(&parent.0).copied() {
            edges.add((parent_name, name));
        }
    }

    for (&name, count) in nodes.iter() {
        let label = format!("{name} ×{count}");
        if roots.contains(name) {
            graph.node_classed(&node_id(name), &label, &["root"]);
        } else {
            graph.node(&node_id(name), &label);
        }
    }
    for (&(parent, child), count) in edges.iter() {
        graph.edge_labeled(&node_id(parent), &node_id(child), &count.to_string());
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{SpanId, StringId};

    fn string_register(id: u32, value: &str) -> OwnedFrame {
        OwnedFrame::StringRegister { id: StringId(id), value: value.to_string() }
    }

    fn span_start(id: u64, parent: u64, name_id: u32) -> OwnedFrame {
        OwnedFrame::SpanStart {
            id: SpanId(id),
            parent: SpanId(parent),
            name_id: StringId(name_id),
            t: 0,
            task_id: 0,
            hart_id: 0,
        }
    }

    #[test]
    fn collapses_spans_by_name_into_a_call_graph() {
        let frames = vec![
            string_register(1, "kernel.boot"),
            string_register(2, "heartbeat"),
            span_start(10, 0, 1),
            span_start(11, 10, 2),
            span_start(12, 10, 2),
        ];
        let expected = "\
graph TD
    kernel_boot[\"kernel.boot ×1\"]
    heartbeat[\"heartbeat ×2\"]
    kernel_boot -->|2| heartbeat
    classDef root fill:#dae8fc,stroke:#6c8ebf;
    class kernel_boot root;
";
        assert_eq!(span_call_graph(&frames).to_mermaid(), expected);
    }
}
