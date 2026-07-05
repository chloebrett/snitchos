//! Runtime (B2) target: the capability derivation tree. Folds `CapEvent`
//! telemetry frames into a `Graph` by their `parent_cap_id → cap_id` linkage —
//! the authority graph the running system snitches about itself. Pure: xtask
//! sources the frames (a capture, or a fresh snemu boot) and hands them here.

use protocol::stream::OwnedFrame;
use protocol::{CapEventKind, CapObject};

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

/// Tracks whether `CapEvent` emission has gone quiescent, so a snemu boot can
/// stop stepping once the authority graph has settled instead of running its
/// full step ceiling. "Quiescent" = at least one `CapEvent` seen, and
/// `window` steps have since elapsed with no new one. A fresh `CapEvent` resets
/// the window (init's children mint reply caps in bursts as they do IPC).
pub struct CapQuiescence {
    window: u64,
    last_count: usize,
    last_change_step: Option<u64>,
}

impl CapQuiescence {
    pub fn new(window: u64) -> Self {
        Self { window, last_count: 0, last_change_step: None }
    }

    /// Observe the cumulative `CapEvent` count at instruction `step`. Returns
    /// `true` once emission is quiescent (see [`CapQuiescence`]).
    pub fn observe(&mut self, count: usize, step: u64) -> bool {
        if count > self.last_count {
            self.last_count = count;
            self.last_change_step = Some(step);
        }
        match self.last_change_step {
            Some(changed) => step.saturating_sub(changed) >= self.window,
            None => false,
        }
    }
}

/// Build the derivation tree from a frame stream. Each `CapEvent` contributes a
/// node keyed by `cap_id` (labelled with its object kind and holder); an edge
/// runs from `parent_cap_id` to `cap_id` for every non-root derivation
/// (`parent_cap_id == 0` marks a genuinely-root grant). Non-`CapEvent` frames
/// are ignored. Top-down layout so roots sit at the top.
pub fn derivation_tree(frames: &[OwnedFrame]) -> Graph {
    let mut graph = Graph::new(Direction::TopDown);
    graph.define_class(
        "root",
        "fill:#dae8fc,stroke:#6c8ebf",
        &[("style", "filled"), ("fillcolor", "#dae8fc")],
    );

    let revoked: std::collections::HashSet<u64> = frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::CapEvent { kind: CapEventKind::Revoked, cap_id, .. } => Some(*cap_id),
            _ => None,
        })
        .collect();

    let mut nodes_seen = std::collections::HashSet::new();
    let mut edges_seen = std::collections::HashSet::new();
    for frame in frames {
        let OwnedFrame::CapEvent { cap_id, parent_cap_id, holder, object, name, .. } = frame else {
            continue;
        };
        // One-shot reply caps are unparented per-`call` leaves — noise in a
        // derivation view. Drop them so the tree shows lasting authority.
        if matches!(object, CapObject::Reply) {
            continue;
        }
        if nodes_seen.insert(*cap_id) {
            let named = snitchos_abi::name_str(name);
            let descriptor = if named.is_empty() { object_name(*object) } else { named };
            let mut label = format!("#{cap_id} {descriptor} h{holder}");
            if revoked.contains(cap_id) {
                label.push_str(" ⊘ revoked");
            }
            let id = format!("cap{cap_id}");
            // parent_cap_id 0 marks a genuinely-root grant — style it distinctly.
            if *parent_cap_id == 0 {
                graph.node_classed(&id, &label, &["root"]);
            } else {
                graph.node(&id, &label);
            }
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
            name: [0u8; snitchos_abi::CAP_NAME_LEN],
        }
    }

    #[test]
    fn quiescence_needs_at_least_one_cap_event() {
        let mut q = CapQuiescence::new(10);
        assert!(!q.observe(0, 1000), "no CapEvent seen — never quiescent");
    }

    #[test]
    fn quiescence_trips_after_the_window_since_the_last_cap_event() {
        let mut q = CapQuiescence::new(10);
        assert!(!q.observe(1, 5), "first cap at step 5");
        assert!(!q.observe(1, 14), "9 < 10 elapsed");
        assert!(q.observe(1, 15), "10 >= 10 elapsed — quiescent");
    }

    #[test]
    fn a_new_cap_event_resets_the_window() {
        let mut q = CapQuiescence::new(10);
        q.observe(1, 5);
        assert!(!q.observe(2, 12), "new cap at 12 resets the window");
        assert!(!q.observe(2, 21), "9 since reset");
        assert!(q.observe(2, 22), "10 since reset — quiescent");
    }

    #[test]
    fn labels_prefer_the_cap_name_over_the_object_kind() {
        let mut named = cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint);
        if let OwnedFrame::CapEvent { name, .. } = &mut named {
            *name = snitchos_abi::pack_name("fs.root");
        }
        let expected = "\
graph TD
    cap1[\"#1 fs.root h6\"]
    classDef root fill:#dae8fc,stroke:#6c8ebf;
    class cap1 root;
";
        assert_eq!(derivation_tree(&[named]).to_mermaid(), expected);
    }

    #[test]
    fn drops_one_shot_reply_caps_as_derivation_noise() {
        // Reply caps are minted per-`call` with parent_cap_id 0 — unparented
        // one-shots that clutter a derivation view without adding structure.
        let frames = vec![
            cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 2, 0, 6, CapObject::Reply),
        ];
        let expected = "\
graph TD
    cap1[\"#1 Endpoint h6\"]
    classDef root fill:#dae8fc,stroke:#6c8ebf;
    class cap1 root;
";
        assert_eq!(derivation_tree(&frames).to_mermaid(), expected);
    }

    #[test]
    fn marks_root_grants_with_the_root_class() {
        let frames = vec![
            cap_event(CapEventKind::Granted, 1, 0, 4, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 2, 1, 6, CapObject::Endpoint),
        ];
        let mermaid = derivation_tree(&frames).to_mermaid();
        assert!(mermaid.contains("classDef root"), "defines the root class");
        assert!(mermaid.contains("class cap1 root;"), "cap1 (parent 0) styled as root");
        assert!(!mermaid.contains("class cap2 root"), "cap2 (derived) is not a root");
    }

    #[test]
    fn annotates_revoked_caps_in_the_label() {
        let frames = vec![
            cap_event(CapEventKind::Granted, 1, 0, 4, CapObject::Endpoint),
            cap_event(CapEventKind::Revoked, 1, 0, 4, CapObject::Endpoint),
        ];
        let mermaid = derivation_tree(&frames).to_mermaid();
        assert!(mermaid.contains("#1 Endpoint h4 ⊘ revoked"), "revoked cap is annotated");
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
    classDef root fill:#dae8fc,stroke:#6c8ebf;
    class cap1 root;
";
        assert_eq!(derivation_tree(&frames).to_mermaid(), expected);
    }
}
