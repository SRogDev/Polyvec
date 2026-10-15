//! # polyvec-retrieval
//!
//! Lexical and hybrid retrieval over a Polyvec collection.
//!
//! * [`fusion`] — Reciprocal Rank Fusion over per-subsystem rankings
//!   (pure, deterministic, dependency-free).
//! * [`lexical`] — tantivy-backed per-collection lexical index over chunk
//!   text (Phase 5; the lexical subsystem — it never owns the document
//!   model, which stays in `polyvec-core` / `polyvec-storage`).
//!
//! Dependency rule: this crate may depend on `polyvec-core` and on the
//! lexical engine (tantivy). It never depends on the server, the ingestion
//! runner, or the embedding providers — orchestration lives above.

pub mod fusion;
pub mod lexical;

pub use fusion::{FusedHit, RankedHit, reciprocal_rank_fusion};
pub use lexical::{LexicalDocument, LexicalHit, LexicalIndex};
