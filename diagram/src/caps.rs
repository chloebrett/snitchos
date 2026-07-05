//! Runtime (B2) target: the capability derivation tree. Folds `CapEvent`
//! telemetry frames into a `Graph` by their `parent_cap_id → cap_id` linkage —
//! the authority graph the running system snitches about itself. Pure: xtask
//! sources the frames (a capture, or a fresh snemu boot) and hands them here.

use protocol::CapObject;
use protocol::stream::OwnedFrame;

use crate::model::{Direction, Graph};

fn object_name(object: CapObject) -> &'static str {
    match object {
        CapObject::TelemetrySink => "TelemetrySink",
        CapObject::SpanSink => "SpanSink",
        CapObject::Endpoint => "Endpoint",
        CapObject::Reply => "Reply",
        CapObject::Notification => "Notification",
    }
}

/// Build the derivation tree from a frame stream. Each `CapEvent` contributes a
/// node keyed by `cap_id` (labelled with its object kind and holder); an edge
/// runs from `parent_cap_id` to `cap_id` for every non-root derivation
/// (`parent_cap_id == 0` marks a genuinely-root grant). Non-`CapEvent` frames
/// are ignored. Top-down layout so roots sit at the top.
pub fn derivation_tree(frames: &[OwnedFrame]) -> Graph {
    let mut graph = Graph::new(Direction::TopDown);
    let mut nodes_seen = std::collections::HashSet::new();
    let mut edges_seen = std::collections::HashSet::new();
    for frame in frames {
        let OwnedFrame::CapEvent { cap_id, parent_cap_id, holder, object, .. } = frame else {
            continue;
        };
        if nodes_seen.insert(*cap_id) {
            let label = format!("#{cap_id} {} h{holder}", object_name(*object));
            graph.node(&format!("cap{cap_id}"), &label);
        }
        if *parent_cap_id != 0 && edges_seen.insert((*parent_cap_id, *cap_id)) {
            graph.edge(&format!("cap{parent_cap_id}"), &format!("cap{cap_id}"));
        }
    }
    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::CapEventKind;

    fn cap_event(
        kind: CapEventKind,
        cap_id: u64,
        parent_cap_id: u64,
        holder: u32,
        object: CapObject,
    ) -> OwnedFrame {
        OwnedFrame::CapEvent {
            kind,
            cap_id,
            parent_cap_id,
            holder,
            object,
            rights: 0,
            badge: 0,
            t: 0,
            hart_id: 0,
        }
    }

    #[test]
    fn folds_cap_events_into_a_derivation_tree() {
        let frames = vec![
            cap_event(CapEventKind::Granted, 1, 0, 1, CapObject::Endpoint),
            OwnedFrame::Dropped { count: 3 },
            cap_event(CapEventKind::Transferred, 2, 1, 2, CapObject::Endpoint),
        ];
        let expected = "\
graph TD
    cap1[\"#1 Endpoint h1\"]
    cap2[\"#2 Endpoint h2\"]
    cap1 --> cap2
";
        assert_eq!(derivation_tree(&frames).to_mermaid(), expected);
    }
}
