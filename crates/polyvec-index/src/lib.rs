//! # polyvec-index
//!
//! Vector index layer: the [`AnnIndex`] ANN wrapper around `hnsw_rs`,
//! Polyvec-owned [`TombstoneSet`] deletion, and the [`ExactIndex`]
//! brute-force baseline used to *measure* recall.
//!
//! Dependency rule: this crate depends on `polyvec-core` and `hnsw_rs`
//! only — no Actix Web, no Tokio, no provider SDKs.

mod ann;
mod exact;
mod tombstones;

pub use ann::AnnIndex;
pub use exact::{ExactIndex, recall_at_k};
pub use tombstones::TombstoneSet;

/// One search result: graph node id + distance under the index metric.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// The node id assigned at insert time.
    pub node_id: usize,
    /// Distance under the index's metric (smaller is closer).
    pub distance: f32,
}
