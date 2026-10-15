//! Ingestion pipeline for Polyvec: bytes in, searchable vectors out.
//!
//! ## Stages
//!
//! ```text
//! bytes → convert (Converter trait; markit behind it)
//!       → canonicalize (deterministic Markdown normalization; no LLM rewriting)
//!       → chunk (section-aware, deterministic ids)
//!       → embed (provider, batched) or supplied vectors
//!       → store (atomic per-document write; version bump tombstones the old)
//! ```
//!
//! ## Module map
//!
//! * [`converter`] — the [`Converter`](converter::Converter) trait (markit is
//!   replaceable) and [`MarkitConverter`](converter::MarkitConverter).
//! * [`canonical`] — [`CanonicalDocument`](canonical::CanonicalDocument):
//!   normalized markdown plus original + canonical hashes and full source
//!   attribution. Nothing about the source is discarded.
//! * [`chunk`] — [`Chunker`](chunk::Chunker): section-aware, deterministic.
//! * [`jobs`] — [`IngestionJob`](jobs::IngestionJob) records persisted in
//!   redb, with a crash-safe state machine and idempotency keys.
//! * [`runner`] — [`JobRunner`](runner::JobRunner): bounded concurrency,
//!   bounded retries, backpressure. Talks to storage through the
//!   [`DocStore`](runner::DocStore) / [`CollectionOpener`](runner::CollectionOpener)
//!   traits so this crate never depends on the server.

pub mod canonical;
pub mod chunk;
pub mod converter;
pub mod jobs;
pub mod runner;

pub use canonical::{CanonicalDocument, canonicalize, normalize_markdown};
pub use chunk::{Chunker, RawChunk};
pub use converter::{ConversionInfo, ConvertedDocument, Converter, MarkitConverter, SourceHint};
pub use jobs::{IngestionJob, JobId, JobOp, JobState, JobStore};
pub use runner::{
    CollectionOpener, DeleteRequest, DocStore, IngestRequest, JobRunner, RunnerLimits, SubmitError,
};
