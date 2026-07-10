//! Runtime (B2) target: the capability derivation tree. Folds `CapEvent`
//! telemetry frames into a `Graph` by their `parent_cap_id → cap_id` linkage —
//! the authority graph the running system snitches about itself. Pure: xtask
//! sources the frames (a capture, or a fresh snemu boot) and hands them here.

use std::collections::{HashMap, HashSet};

use protocol::stream::OwnedFrame;
use protocol::{CapEventKind, CapObject};

use crate::fold::thread_names;
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

/// Decode a rights bitmask into `|`-joined flag names (e.g. `RECV|MINT`), so the
/// diagram shows *what authority* a cap carries — the least-authority story.
fn rights_str(rights: u32) -> String {
    use snitchos_abi::rights as r;
    [
        (r::EMIT, "EMIT"),
        (r::SEND, "SEND"),
        (r::RECV, "RECV"),
        (r::MINT, "MINT"),
        (r::SIGNAL, "SIGNAL"),
        (r::WAIT, "WAIT"),
    ]
    .into_iter()
    .filter(|(bit, _)| rights & bit != 0)
    .map(|(_, name)| name)
    .collect::<Vec<_>>()
    .join("|")
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

/// Build the derivation tree from a frame stream. Each non-`Reply` `CapEvent`
/// contributes a node keyed by `cap_id`, labelled `#id name holder [rights]`
/// (holder resolved to a process name via `ThreadRegister` when known, rights
/// decoded to flag names); an edge runs `parent_cap_id → cap_id`, and a
/// `parent_cap_id == 0` grant is styled as a root. **Isolated** grants — no
/// parent and no children, i.e. the per-process bootstrap telemetry/span sinks —
/// are dropped, so what remains is the actual delegation structure. Top-down.
pub fn derivation_tree(frames: &[OwnedFrame]) -> Graph {
    let names = thread_names(frames);
    let revoked: HashSet<u64> = frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::CapEvent { kind: CapEventKind::Revoked, cap_id, .. } => Some(*cap_id),
            _ => None,
        })
        .collect();

    // Pass 1: collect non-Reply nodes (label + root-ness) and derivation edges.
    let mut order: Vec<u64> = Vec::new();
    let mut label_of: HashMap<u64, String> = HashMap::new();
    let mut roots: HashSet<u64> = HashSet::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut edge_order: Vec<(u64, u64)> = Vec::new();
    let mut edges_seen: HashSet<(u64, u64)> = HashSet::new();
    for frame in frames {
        let OwnedFrame::CapEvent { cap_id, parent_cap_id, holder, object, name, rights, .. } = frame
        else {
            continue;
        };
        if matches!(object, CapObject::Reply) {
            continue;
        }
        if seen.insert(*cap_id) {
            order.push(*cap_id);
            let named = snitchos_abi::name_str(name);
            let descriptor = if named.is_empty() { object_name(*object) } else { named };
            let holder_label =
                names.get(holder).map_or_else(|| format!("h{holder}"), |n| (*n).to_string());
            let mut label = format!("#{cap_id} {descriptor} {holder_label}");
            let rs = rights_str(*rights);
            if !rs.is_empty() {
                label.push_str(" [");
                label.push_str(&rs);
                label.push(']');
            }
            if revoked.contains(cap_id) {
                label.push_str(" ⊘ revoked");
            }
            label_of.insert(*cap_id, label);
            if *parent_cap_id == 0 {
                roots.insert(*cap_id);
            }
        }
        if *parent_cap_id != 0 && edges_seen.insert((*parent_cap_id, *cap_id)) {
            edge_order.push((*parent_cap_id, *cap_id));
        }
    }

    // Keep only edges between real (non-Reply) nodes; a node is dropped if it
    // ends up in no edge (isolated bootstrap grant).
    let edges: Vec<(u64, u64)> = edge_order
        .into_iter()
        .filter(|(from, to)| label_of.contains_key(from) && label_of.contains_key(to))
        .collect();
    let connected: HashSet<u64> = edges.iter().flat_map(|(from, to)| [*from, *to]).collect();

    let mut graph = Graph::new(Direction::TopDown);
    graph.define_root_class();
    for cap_id in order {
        if !connected.contains(&cap_id) {
            continue;
        }
        let id = format!("cap{cap_id}");
        let label = &label_of[&cap_id];
        if roots.contains(&cap_id) {
            graph.node_classed(&id, label, &["root"]);
        } else {
            graph.node(&id, label);
        }
    }
    for (from, to) in edges {
        graph.edge(&format!("cap{from}"), &format!("cap{to}"));
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
        // Named root with a child, so it survives the isolated-node drop.
        let mut root = cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint);
        if let OwnedFrame::CapEvent { name, .. } = &mut root {
            *name = snitchos_abi::pack_name("fs.root");
        }
        let child = cap_event(CapEventKind::Transferred, 2, 1, 7, CapObject::Endpoint);
        let mermaid = derivation_tree(&[root, child]).to_mermaid();
        assert!(mermaid.contains("cap1[\"#1 fs.root h6\"]"), "name preferred over object kind");
    }

    #[test]
    fn minted_cap_renders_as_a_root_node() {
        // A self-minted endpoint (parent 0) is a derivation-tree root, exactly
        // like a `Granted` root — the tree builder keys on `parent_cap_id`, not
        // the kind. A child keeps it from being dropped as an isolated node.
        let mut root = cap_event(CapEventKind::Minted, 1, 0, 6, CapObject::Endpoint);
        if let OwnedFrame::CapEvent { name, .. } = &mut root {
            *name = snitchos_abi::pack_name("fs");
        }
        let child = cap_event(CapEventKind::Transferred, 2, 1, 7, CapObject::Endpoint);
        let mermaid = derivation_tree(&[root, child]).to_mermaid();
        assert!(mermaid.contains("cap1[\"#1 fs h6\"]"), "minted cap is a node");
        assert!(mermaid.contains("cap1 --> cap2"), "minted cap roots its child");
    }

    #[test]
    fn shows_decoded_rights_in_the_label() {
        use snitchos_abi::rights;
        let mut root = cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint);
        if let OwnedFrame::CapEvent { rights: bits, .. } = &mut root {
            *bits = rights::RECV | rights::MINT;
        }
        let child = cap_event(CapEventKind::Transferred, 2, 1, 7, CapObject::Endpoint);
        let mermaid = derivation_tree(&[root, child]).to_mermaid();
        assert!(mermaid.contains("[RECV|MINT]"), "rights decoded onto the label");
    }

    #[test]
    fn resolves_holder_ids_to_process_names() {
        let frames = vec![
            OwnedFrame::ThreadRegister { id: 6, name: "fs-server".to_string(), priority: 0 },
            cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 2, 1, 7, CapObject::Endpoint),
        ];
        let mermaid = derivation_tree(&frames).to_mermaid();
        assert!(mermaid.contains("#1 Endpoint fs-server"), "holder id resolved to its name");
        assert!(!mermaid.contains(" h6"), "raw holder id not shown when named");
    }

    #[test]
    fn drops_one_shot_reply_caps_as_derivation_noise() {
        // Reply caps are minted per-`call` — one-shots that clutter a derivation
        // view. cap3 is minted off cap1 but must not appear.
        let frames = vec![
            cap_event(CapEventKind::Granted, 1, 0, 6, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 2, 1, 7, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 3, 1, 6, CapObject::Reply),
        ];
        let mermaid = derivation_tree(&frames).to_mermaid();
        assert!(!mermaid.contains("Reply"), "reply caps are dropped");
        assert!(!mermaid.contains("cap3"), "the reply cap node is absent");
        assert!(mermaid.contains("cap1 --> cap2"), "the real delegation edge remains");
    }

    #[test]
    fn drops_isolated_bootstrap_grants() {
        let frames = vec![
            // Isolated: parent 0, never a parent of anything → dropped.
            cap_event(CapEventKind::Granted, 9, 0, 4, CapObject::TelemetrySink),
            // Connected delegation stays.
            cap_event(CapEventKind::Granted, 1, 0, 4, CapObject::Endpoint),
            cap_event(CapEventKind::Transferred, 2, 1, 6, CapObject::Endpoint),
        ];
        let mermaid = derivation_tree(&frames).to_mermaid();
        assert!(!mermaid.contains("cap9"), "isolated bootstrap grant dropped");
        assert!(!mermaid.contains("TelemetrySink"), "the childless sink is gone");
        assert!(mermaid.contains("cap1 --> cap2"), "the delegation stays");
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
            cap_event(CapEventKind::Transferred, 2, 1, 6, CapObject::Endpoint),
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
