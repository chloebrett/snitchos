use protocol::{CapEventKind, CapObject};
use snitchos_abi::CAP_NAME_LEN;

use crate::state::{CompletedSpan, SpanEvent, TraceKind};

struct Holding {
    parent_cap_id: u64,
    holder: u32,
    object: CapObject,
    rights: u32,
    badge: u64,
    start_t: u128,
    name: [u8; CAP_NAME_LEN],
    events: Vec<SpanEvent>,
}

/// Reconstructs capability hold-duration spans from `CapEvent` frames.
///
/// Each `cap_id` is ONE span: start = grant time, end = revoke time (or
/// `flush` time for still-held caps). The kernel emits one `Revoked` per
/// swept cap in a transitive revoke, so each closing is handled individually.
///
/// `observe` takes pre-anchored wall-clock nanoseconds — the caller (Step 4
/// `State::handle`) converts kernel ticks via `SessionAnchor` before calling.
pub struct CapTracker {
    open: std::collections::HashMap<u64, Holding>,
    closed: Vec<CompletedSpan>,
}

impl CapTracker {
    pub fn new() -> Self {
        Self { open: std::collections::HashMap::new(), closed: Vec::new() }
    }

    /// Feed a capability event. One-shot `Reply` caps are dropped as noise.
    /// Grants/Transfers open (or update) a holding; Revokes close one.
    pub fn observe(
        &mut self,
        kind: CapEventKind,
        cap_id: u64,
        parent_cap_id: u64,
        holder: u32,
        object: CapObject,
        rights: u32,
        badge: u64,
        t: u128,
        name: [u8; CAP_NAME_LEN],
    ) {
        if matches!(object, CapObject::Reply) {
            return;
        }
        match kind {
            CapEventKind::Granted | CapEventKind::Transferred | CapEventKind::Minted => {
                let event_name = match kind {
                    CapEventKind::Granted => "granted",
                    CapEventKind::Minted => "minted",
                    _ => "transferred",
                };
                let ev = SpanEvent {
                    name: event_name.to_string(),
                    time_ns: t,
                    attributes: vec![("holder".to_string(), holder.to_string())],
                };
                if let Some(holding) = self.open.get_mut(&cap_id) {
                    holding.holder = holder;
                    holding.events.push(ev);
                } else {
                    self.open.insert(
                        cap_id,
                        Holding {
                            parent_cap_id,
                            holder,
                            object,
                            rights,
                            badge,
                            start_t: t,
                            name,
                            events: vec![ev],
                        },
                    );
                }
            }
            CapEventKind::Revoked => {
                if let Some(mut holding) = self.open.remove(&cap_id) {
                    holding.events.push(SpanEvent {
                        name: "revoked".to_string(),
                        time_ns: t,
                        attributes: vec![("holder".to_string(), holder.to_string())],
                    });
                    self.closed.push(build_span(cap_id, &holding, t, true));
                }
            }
        }
    }

    /// Return and clear all spans closed since the last drain.
    pub fn drain_closed(&mut self) -> Vec<CompletedSpan> {
        std::mem::take(&mut self.closed)
    }

    /// Close all still-open holdings at `now_t` (session end / kernel restart).
    /// Clears the open map.
    pub fn flush(&mut self, now_t: u128) -> Vec<CompletedSpan> {
        self.open
            .drain()
            .map(|(cap_id, holding)| build_span(cap_id, &holding, now_t, false))
            .collect()
    }
}

fn cap_label(name: &[u8; CAP_NAME_LEN], object: CapObject) -> String {
    let named = snitchos_abi::name_str(name);
    if named.is_empty() { object_kind(object).to_string() } else { named.to_string() }
}

fn object_kind(object: CapObject) -> &'static str {
    match object {
        CapObject::TelemetrySink => "telemetry-sink",
        CapObject::SpanSink => "span-sink",
        CapObject::Endpoint => "endpoint",
        CapObject::Reply => "reply",
        CapObject::Notification => "notification",
    }
}

fn build_span(cap_id: u64, holding: &Holding, end_t: u128, revoked: bool) -> CompletedSpan {
    let mut extra = vec![
        ("cap.holder".to_string(), holding.holder.to_string()),
        ("cap.object".to_string(), object_kind(holding.object).to_string()),
        ("cap.rights".to_string(), holding.rights.to_string()),
        ("cap.revoked".to_string(), revoked.to_string()),
    ];
    if holding.badge != 0 {
        extra.push(("cap.badge".to_string(), holding.badge.to_string()));
    }
    CompletedSpan {
        trace: TraceKind::Capabilities,
        name: cap_label(&holding.name, holding.object),
        span_id: cap_id,
        parent_span_id: holding.parent_cap_id,
        start_time_ns: holding.start_t,
        end_time_ns: end_t,
        extra_attributes: extra,
        events: holding.events.clone(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_name() -> [u8; CAP_NAME_LEN] {
        [0u8; CAP_NAME_LEN]
    }

    fn packed_name(s: &str) -> [u8; CAP_NAME_LEN] {
        let mut buf = [0u8; CAP_NAME_LEN];
        let b = s.as_bytes();
        let len = b.len().min(CAP_NAME_LEN);
        buf[..len].copy_from_slice(&b[..len]);
        buf
    }

    fn grant(
        tracker: &mut CapTracker,
        cap_id: u64,
        parent_cap_id: u64,
        holder: u32,
        t: u128,
        name: [u8; CAP_NAME_LEN],
    ) {
        tracker.observe(
            CapEventKind::Granted,
            cap_id,
            parent_cap_id,
            holder,
            CapObject::Endpoint,
            0,
            0,
            t,
            name,
        );
    }

    fn revoke(tracker: &mut CapTracker, cap_id: u64, holder: u32, t: u128) {
        tracker.observe(
            CapEventKind::Revoked,
            cap_id,
            0,
            holder,
            CapObject::Endpoint,
            0,
            0,
            t,
            empty_name(),
        );
    }

    fn extra(span: &CompletedSpan, key: &str) -> Option<String> {
        span.extra_attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn grant_revoke_produces_duration_span() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, packed_name("fs"));
        revoke(&mut tracker, 1, 3, 90);
        let spans = tracker.drain_closed();
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.span_id, 1);
        assert_eq!(s.start_time_ns, 10);
        assert_eq!(s.end_time_ns, 90);
        assert_eq!(s.name, "fs");
        assert_eq!(extra(s, "cap.revoked").as_deref(), Some("true"));
        assert_eq!(s.trace, TraceKind::Capabilities);
    }

    #[test]
    fn open_cap_emitted_at_flush() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, empty_name());
        assert!(tracker.drain_closed().is_empty());
        let spans = tracker.flush(200);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.start_time_ns, 10);
        assert_eq!(s.end_time_ns, 200);
        assert_eq!(extra(s, "cap.revoked").as_deref(), Some("false"));
    }

    #[test]
    fn flush_clears_open_holdings() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, empty_name());
        tracker.flush(200);
        assert!(tracker.flush(300).is_empty());
    }

    #[test]
    fn transitive_revoke_closes_each_cap_on_its_own_revoked_event() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, packed_name("fs"));
        grant(&mut tracker, 2, 1, 5, 20, packed_name("fs"));
        revoke(&mut tracker, 1, 3, 90);
        revoke(&mut tracker, 2, 5, 90);
        let spans = tracker.drain_closed();
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|s| s.end_time_ns == 90));
        assert!(spans.iter().all(|s| extra(s, "cap.revoked").as_deref() == Some("true")));
    }

    #[test]
    fn reply_cap_is_dropped() {
        let mut tracker = CapTracker::new();
        tracker.observe(
            CapEventKind::Granted,
            99,
            0,
            3,
            CapObject::Reply,
            0,
            0,
            10,
            empty_name(),
        );
        assert!(tracker.flush(200).is_empty());
        assert!(tracker.drain_closed().is_empty());
    }

    #[test]
    fn root_cap_has_no_parent_span() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, empty_name());
        let spans = tracker.flush(200);
        assert_eq!(spans[0].parent_span_id, 0);
    }

    #[test]
    fn derived_cap_carries_parent_span_id() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, empty_name());
        grant(&mut tracker, 2, 1, 3, 20, empty_name());
        let spans = tracker.flush(200);
        let child = spans.iter().find(|s| s.span_id == 2).unwrap();
        assert_eq!(child.parent_span_id, 1);
    }

    #[test]
    fn unnamed_cap_uses_object_kind_as_label() {
        let mut tracker = CapTracker::new();
        tracker.observe(
            CapEventKind::Granted,
            1,
            0,
            3,
            CapObject::TelemetrySink,
            0,
            0,
            10,
            empty_name(),
        );
        let spans = tracker.flush(200);
        assert_eq!(spans[0].name, "telemetry-sink");
    }

    #[test]
    fn named_cap_uses_name_as_label() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, packed_name("fs"));
        let spans = tracker.flush(200);
        assert_eq!(spans[0].name, "fs");
    }

    #[test]
    fn span_carries_granted_and_revoked_events() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, empty_name());
        revoke(&mut tracker, 1, 3, 90);
        let spans = tracker.drain_closed();
        let events = &spans[0].events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "granted");
        assert_eq!(events[0].time_ns, 10);
        assert_eq!(events[1].name, "revoked");
        assert_eq!(events[1].time_ns, 90);
    }

    fn mint(
        tracker: &mut CapTracker,
        cap_id: u64,
        holder: u32,
        t: u128,
        name: [u8; CAP_NAME_LEN],
    ) {
        tracker.observe(
            CapEventKind::Minted,
            cap_id,
            0,
            holder,
            CapObject::Endpoint,
            0b0110,
            0,
            t,
            name,
        );
    }

    #[test]
    fn minted_event_opens_holding_named_minted() {
        let mut tracker = CapTracker::new();
        mint(&mut tracker, 1, 3, 10, packed_name("fs"));
        revoke(&mut tracker, 1, 3, 90);
        let spans = tracker.drain_closed();
        assert_eq!(spans.len(), 1);
        let events = &spans[0].events;
        assert_eq!(events[0].name, "minted");
        assert_eq!(events[0].time_ns, 10);
        assert_eq!(events[1].name, "revoked");
        assert_eq!(spans[0].name, "fs");
    }

    #[test]
    fn transferred_event_updates_holder_and_adds_event() {
        let mut tracker = CapTracker::new();
        grant(&mut tracker, 1, 0, 3, 10, packed_name("fs"));
        tracker.observe(
            CapEventKind::Transferred,
            1,
            0,
            7,
            CapObject::Endpoint,
            0,
            0,
            50,
            empty_name(),
        );
        let spans = tracker.flush(200);
        let events = &spans[0].events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].name, "transferred");
        assert_eq!(events[1].time_ns, 50);
        assert_eq!(extra(&spans[0], "cap.holder").as_deref(), Some("7"));
    }

    #[test]
    fn badge_attribute_present_only_when_nonzero() {
        let mut tracker = CapTracker::new();
        tracker.observe(
            CapEventKind::Granted,
            1,
            0,
            3,
            CapObject::Endpoint,
            0,
            42,
            10,
            empty_name(),
        );
        tracker.observe(
            CapEventKind::Granted,
            2,
            0,
            3,
            CapObject::Endpoint,
            0,
            0,
            10,
            empty_name(),
        );
        let spans = tracker.flush(200);
        let badged = spans.iter().find(|s| s.span_id == 1).unwrap();
        let plain = spans.iter().find(|s| s.span_id == 2).unwrap();
        assert_eq!(extra(badged, "cap.badge").as_deref(), Some("42"));
        assert!(extra(plain, "cap.badge").is_none());
    }
}
