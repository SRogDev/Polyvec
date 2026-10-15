//! # polyvec-storage
//!
//! Durable storage layer: the redb-backed [`CollectionStore`] (catalog +
//! ANN index + checksummed persistence layout + crash recovery).
//!
//! The catalog is the transactional source of truth; the hnsw dump files
//! are a rebuildable cache over it. See the [`CollectionStore`] docs for
//! the commit protocol and the exact recovery rules.
//!
//! Dependency rule: this crate depends on `polyvec-core`, `polyvec-index`,
//! `redb`, `serde`/`serde_json` only — no Actix Web, no Tokio, no provider
//! SDKs.

mod catalog;
mod layout;
mod store;

pub use layout::{DUMP_BASENAME, SIDECAR_NAME, Sidecar};
pub use store::{
    CollectionStore, Recovery, SearchHit, Stats, StoredVector, read_collection_config,
};
