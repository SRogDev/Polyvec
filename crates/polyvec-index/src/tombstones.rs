//! Polyvec-owned tombstone set.
//!
//! `hnsw_rs` has no delete API, so Polyvec owns deletion: a tombstoned node
//! id stays in the graph but is filtered out of every search result
//! (immediately — no flush needed). [`TombstoneSet`] is the in-memory mirror;
//! the durable copy lives in the storage layer's redb catalog.

use std::collections::HashSet;

/// The set of graph node ids hidden from search results.
#[derive(Clone, Debug, Default)]
pub struct TombstoneSet {
    ids: HashSet<usize>,
}

impl TombstoneSet {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `node_id` as deleted. Returns `true` if it was newly added.
    pub fn insert(&mut self, node_id: usize) -> bool {
        self.ids.insert(node_id)
    }

    /// Whether `node_id` is tombstoned.
    #[must_use]
    pub fn contains(&self, node_id: usize) -> bool {
        self.ids.contains(&node_id)
    }

    /// Un-marks `node_id`. Returns `true` if it was present.
    pub fn remove(&mut self, node_id: usize) -> bool {
        self.ids.remove(&node_id)
    }

    /// Drops all tombstones (used after a compacting rebuild).
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    /// Number of tombstoned nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether no node is tombstoned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Iterates over tombstoned node ids.
    pub fn iter(&self) -> impl Iterator<Item = &usize> {
        self.ids.iter()
    }
}
