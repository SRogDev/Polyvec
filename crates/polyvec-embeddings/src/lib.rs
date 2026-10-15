//! Embedding provider interface for Polyvec.
//!
//! The [`EmbeddingProvider`] trait is the seam between the ingestion pipeline
//! and whatever turns text into vectors. It is synchronous and dyn-safe:
//! implementations that do I/O (the remote adapter) use blocking HTTP and
//! must run on a blocking thread — the ingestion runner guarantees this.
//!
//! Implementations in this crate:
//! * [`StubEmbeddingProvider`] — deterministic hash-based vectors for tests
//!   and development. **NOT for production**: the vectors carry no semantic
//!   meaning.
//! * [`RemoteEmbeddingProvider`] — generic OpenAI-compatible `/embeddings`
//!   endpoint, configured from the environment.
//!
//! Dimension/model compatibility against the collection is enforced by the
//! ingestion pipeline, never silently mixed.

mod provider;
mod remote;
mod stub;

pub use provider::{EmbedError, EmbedResult, EmbeddingProvider, ErrorKind};
pub use remote::RemoteEmbeddingProvider;
pub use stub::StubEmbeddingProvider;
