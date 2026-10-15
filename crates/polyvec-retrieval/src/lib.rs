//! # polyvec-retrieval
//!
//! Lexical and hybrid retrieval over a Polyvec collection.
//!
//! * [`fusion`] — Reciprocal Rank Fusion over per-subsystem rankings
//!   (pure, deterministic, dependency-free).
//!
//! Dependency rule: this crate may depend on `polyvec-core` and on the
//! lexical engine (tantivy). It never depends on the server, the ingestion
//! runner, or the embedding providers — orchestration lives above.

pub mod fusion;

pub use fusion::{FusedHit, RankedHit, reciprocal_rank_fusion};
