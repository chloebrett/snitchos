//! Runtime (B2) target: the scheduler's task-transition graph. Folds
//! `ContextSwitch` frames into a `from → to` graph, edges labelled with how
//! many times that hand-off happened, nodes named from `ThreadRegister`. Pure —
//! xtask sources the frames from a snemu boot.

use std::collections::{HashMap, HashSet};

use protocol::stream::OwnedFrame;

use crate::model::{Direction, Graph};

/// Build the transition graph: one node per task that appears in a switch
/// (named via `ThreadRegister`, else `task N`), one edge per distinct
/// `from → to` hand-off labelled with its count. Deterministic: nodes and edges
/// keep first-seen order.
pub fn transition_graph(frames: &[OwnedFrame]) -> Graph {
    let names: HashMap<u32, &str> = frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::ThreadRegister { id, name, .. } => Some((*id, name.as_str())),
            _ => None,
        })
        .collect();

    let mut graph = Graph::new(Direction::LeftRight);
    let mut node_seen: HashSet<u32> = HashSet::new();
    let mut edge_order: Vec<(u32, u32)> = Vec::new();
    let mut counts: HashMap<(u32, u32), u64> = HashMap::new();

    for frame in frames {
        let OwnedFrame::ContextSwitch { from, to, .. } = frame else {
            continue;
        };
        for id in [*from, *to] {
            if node_seen.insert(id) {
                let label = names.get(&id).map_or_else(|| format!("task {id}"), |n| (*n).to_string());
                graph.node(&format!("t{id}"), &label);
            }
        }
        let key = (*from, *to);
        if !counts.contains_key(&key) {
            edge_order.push(key);
        }
        *counts.entry(key).or_insert(0) += 1;
    }

    for (from, to) in edge_order {
        graph.edge_labeled(&format!("t{from}"), &format!("t{to}"), &counts[&(from, to)].to_string());
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::SwitchReason;

    fn thread_register(id: u32, name: &str) -> OwnedFrame {
        OwnedFrame::ThreadRegister { id, name: name.to_string(), priority: 0 }
    }

    fn context_switch(from: u32, to: u32) -> OwnedFrame {
        OwnedFrame::ContextSwitch { from, to, t: 0, reason: SwitchReason::Yield, hart_id: 0 }
    }

    #[test]
    fn counts_task_transitions_with_names() {
        let frames = vec![
            thread_register(1, "task_a"),
            thread_register(2, "idle"),
            context_switch(1, 2),
            context_switch(2, 1),
            context_switch(1, 2),
        ];
        let expected = "\
graph LR
    t1[\"task_a\"]
    t2[\"idle\"]
    t1 -->|2| t2
    t2 -->|1| t1
";
        assert_eq!(transition_graph(&frames).to_mermaid(), expected);
    }
}
