//! # polyvec-core
//!
//! Shared domain foundation of the Polyvec vector database.
//!
//! Everything every other crate needs, and nothing else:
//!
//! * **Domain types** — the vocabulary of the database: collections,
//!   documents, chunks, vectors, metadata, and the identifiers that name
//!   them (`ids`, `document`, `vector`, `metadata`).
//! * **IDs** — strongly-typed, validated identifiers (`CollectionId`,
//!   `DocumentId`, `ChunkId`, `VectorId`) with deterministic identity rules,
//!   so raw `u64`/`String` never leak across crate boundaries.
//! * **Config contracts** — the validated configuration shapes each
//!   subsystem consumes (`CollectionConfig`, `DistanceMetric`, `HnswParams`).
//! * **State** — collection lifecycle and [`IndexState`] tracking.
//! * **Hashes** — [`ContentHash`] (SHA-256) for change detection.
//! * **Retrieval contracts** — query [`RetrievalMode`]s, [`MetadataFilter`],
//!   score/path vocabulary shared by the vector, lexical, and hybrid paths.
//! * [`Error`] — the workspace-wide error type.
//!
//! What deliberately does **not** live here:
//!
//! * No Actix Web, no Tokio, no HTTP types — the web/API crates depend on
//!   `polyvec-core`, never the reverse (see `ARCHITECTURE.md`).
//! * No embedding providers, no index implementations, no storage engines.
//!   Those crates are created when their phase needs them — no abstraction
//!   without a concrete use case.

pub mod config;
pub mod document;
pub mod error;
pub mod hash;
pub mod ids;
pub mod metadata;
pub mod metric;
pub mod relation;
pub mod retrieval;
pub mod state;
pub mod vector;

pub use config::{ChunkingConfig, CollectionConfig, HnswParams, MAX_DIMENSION, ModelTag};
pub use document::{Chunk, Document, Provenance};
pub use error::{Error, Result};
pub use hash::ContentHash;
pub use ids::{ChunkId, CollectionId, DocumentId, VectorId};
pub use metadata::{Metadata, MetadataValue};
pub use metric::DistanceMetric;
pub use relation::{Relation, RelationId, RelationType};
pub use retrieval::{
    DEFAULT_EXPAND_DEPTH, DEFAULT_EXPAND_MAX_NODES, ExpandConfig, MAX_EXPAND_DEPTH,
    MAX_EXPAND_MAX_NODES, MetadataFilter, RetrievalMode, RetrievalPath, RrfConfig, ScoreKind,
};
pub use state::{CollectionLifecycle, INDEX_FORMAT_VERSION, IndexState, SyncStatus};
pub use vector::Vector;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_std_error<T: std::error::Error + std::fmt::Debug>(_: &T) {}

    #[test]
    fn config_error_display_contains_detail() {
        let err = Error::Config {
            detail: "dimension must be positive, got 0".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid configuration: dimension must be positive, got 0"
        );
    }

    #[test]
    fn unsupported_error_display_contains_detail() {
        let err = Error::Unsupported {
            detail: "metric 'manhattan' not compiled into this index".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "not supported: metric 'manhattan' not compiled into this index"
        );
    }

    #[test]
    fn internal_error_display_contains_detail() {
        let err = Error::Internal {
            detail: "segment 7 missing from manifest".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "internal error: segment 7 missing from manifest"
        );
    }

    #[test]
    fn error_implements_std_error() {
        let err = Error::Config {
            detail: "x".to_string(),
        };
        // Compile-time check that `?` and error-chain interop work.
        assert_std_error(&err);
    }
}
