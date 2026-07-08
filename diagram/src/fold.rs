//! Shared building blocks for the runtime (B2) graph folds (`caps`, `trace`,
//! `switches`), which all walk a telemetry frame stream into a `Graph`.

use std::collections::HashMap;
use std::hash::Hash;

use protocol::stream::OwnedFrame;

/// Resolve task/process ids to names from the stream's `ThreadRegister` frames.
/// Shared by the `caps` and `switches` folds.
pub(crate) fn thread_names(frames: &[OwnedFrame]) -> HashMap<u32, &str> {
    frames
        .iter()
        .filter_map(|f| match f {
            OwnedFrame::ThreadRegister { id, name, .. } => Some((*id, name.as_str())),
            _ => None,
        })
        .collect()
}

/// Counts occurrences of a key while preserving first-seen order — the idiom
/// behind the labelled edges (and `trace`'s per-name node counts) in the
/// runtime folds.
pub(crate) struct OrderedCounter<K> {
    order: Vec<K>,
    counts: HashMap<K, u64>,
}

impl<K: Eq + Hash + Clone> OrderedCounter<K> {
    pub(crate) fn new() -> Self {
        Self { order: Vec::new(), counts: HashMap::new() }
    }

    pub(crate) fn add(&mut self, key: K) {
        if !self.counts.contains_key(&key) {
            self.order.push(key.clone());
        }
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// First-seen order, each key paired with its total count.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, u64)> {
        self.order.iter().map(move |k| (k, self.counts[k]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_keys_in_first_seen_order() {
        let mut counter = OrderedCounter::new();
        counter.add("a");
        counter.add("b");
        counter.add("a");
        let got: Vec<(&str, u64)> = counter.iter().map(|(k, n)| (*k, n)).collect();
        assert_eq!(got, vec![("a", 2), ("b", 1)]);
    }
}
