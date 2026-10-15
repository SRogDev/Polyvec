//! Collection lifecycle and index-state tracking.
//!
//! [`IndexState`] is the durable record of "what does the ANN index on disk
//! contain right now". The storage layer updates it on every flush/rebuild;
//! recovery compares it against the redb catalog to decide whether the
//! dumped index can be trusted or must be rebuilt.

use serde::{Deserialize, Serialize};

/// Whether the on-disk ANN index reflects everything committed in redb.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncStatus {
    /// Dumped index matches the catalog (same generation).
    Clean,
    /// Catalog has commits the dumped index does not (crash before flush,
    /// or writes since the last flush).
    Dirty,
}

/// Lifecycle of a collection.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionLifecycle {
    /// Being created (not yet queryable).
    Creating,
    /// Live: reads and writes accepted.
    Active,
    /// Being deleted (not yet implemented in Phase 2; reserved).
    Deleting,
}

/// Durable record of the ANN index state for one collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexState {
    /// Index implementation, e.g. `"hnsw_rs"`.
    pub index_type: String,
    /// Polyvec index format version (bump when the dump layout changes).
    pub format_version: u32,
    /// Monotonic counter; bumped on every flush/rebuild.
    pub generation: u64,
    /// Whether the dump matches the catalog.
    pub sync: SyncStatus,
    /// Live (non-tombstoned) vectors the dump contains.
    pub vector_count: u64,
    /// Tombstoned vectors hidden from search but still in the dump.
    pub tombstone_count: u64,
}

/// Current format version of the index sidecar/dump layout.
pub const INDEX_FORMAT_VERSION: u32 = 1;

impl IndexState {
    /// Fresh state for a newly created collection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            index_type: "hnsw_rs".to_string(),
            format_version: INDEX_FORMAT_VERSION,
            generation: 0,
            sync: SyncStatus::Clean,
            vector_count: 0,
            tombstone_count: 0,
        }
    }
}

impl Default for IndexState {
    fn default() -> Self {
        Self::new()
    }
}
