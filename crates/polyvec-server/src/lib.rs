//! # polyvec-server
//!
//! Actix Web HTTP API over the Polyvec vector database engine.
//!
//! ## Data flow
//!
//! ```text
//! HTTP request (actix-web worker)
//!   │  1. route + JSON extract (size-limited, structured errors)
//!   │  2. id/path validation through `polyvec-core` constructors
//!   └─ 3. `web::block` → blocking pool: registry lookup (lazy open),
//!        `Mutex<CollectionStore>` op, response DTO            [polyvec-storage]
//! ```
//!
//! Handlers never run storage code on async workers: HNSW insert/search are
//! CPU-bound, so every [`CollectionStore`](polyvec_storage::CollectionStore)
//! call goes through `web::block` on Tokio's blocking pool, with the
//! per-collection mutex held only inside the blocking closure (never across
//! `.await`).
//!
//! Dependency rule: this crate may depend on `polyvec-core`,
//! `polyvec-storage`, `polyvec-embeddings`, and `polyvec-ingestion` —
//! never the other way around. Actix/Tokio types never leave this crate.
//!
//! Ingestion (`/ingest`, `/jobs`) runs through `polyvec-ingestion`'s
//! [`JobRunner`](polyvec_ingestion::JobRunner): handlers only submit and
//! inspect jobs; the pipeline (convert → canonicalize → chunk → embed →
//! store) executes on Tokio's blocking pool inside the runner.

pub mod config;
pub mod error;
pub mod ingest;
pub mod registry;
pub mod routes;
pub mod schemas;

use std::sync::Arc;

use polyvec_embeddings::EmbeddingProvider;
use polyvec_ingestion::{CollectionOpener, JobRunner};

pub use config::{EmbeddingKind, ServerConfig};
pub use error::{ApiError, json_error_handler};
pub use ingest::{ServerDocStore, ServerOpener, build_provider, build_runner};
pub use registry::Registry;
pub use routes::configure_routes;

/// Maximum `k` accepted by the search endpoint.
pub const MAX_K: usize = 1000;
/// Minimum search width (`ef`) accepted by the search endpoint.
pub const MIN_SEARCH_EF: usize = 8;
/// Maximum search width (`ef`) accepted by the search endpoint.
pub const MAX_SEARCH_EF: usize = 10_000;

/// State shared by every handler: the collection registry, the ingestion
/// job runner, and the (optional) embedding provider.
#[derive(Clone)]
pub struct AppState {
    /// Lazily-opened collection stores rooted at the data dir.
    pub registry: Arc<Registry>,
    /// Ingestion job runner (accept loop spawned at construction).
    pub runner: JobRunner,
    /// Embedding provider for text ingest/query (`None` = unconfigured).
    pub provider: Option<Arc<dyn EmbeddingProvider>>,
}

impl AppState {
    /// Builds shared state from the server config: registry, embedding
    /// provider, and job runner (whose accept loop is spawned here).
    ///
    /// # Panics
    /// Panics when called outside a Tokio runtime — the runner's accept
    /// loop is spawned with `tokio::spawn`.
    ///
    /// # Errors
    /// Returns a message when the embedding provider is misconfigured or
    /// the job store cannot be opened.
    pub fn new(config: &ServerConfig) -> Result<Self, String> {
        let provider = build_provider(config)?;
        Self::with_provider(config, provider)
    }

    /// Builds shared state with an explicit embedding provider (test seam).
    ///
    /// # Panics
    /// Panics when called outside a Tokio runtime — the runner's accept
    /// loop is spawned with `tokio::spawn`.
    ///
    /// # Errors
    /// Returns a message when the job store cannot be opened.
    pub fn with_provider(
        config: &ServerConfig,
        provider: Option<Arc<dyn EmbeddingProvider>>,
    ) -> Result<Self, String> {
        let registry = Arc::new(Registry::new(config.data_dir.clone()));
        let runner = build_runner(config, provider.clone())?;
        let opener: Arc<dyn CollectionOpener> = Arc::new(ServerOpener::new(Arc::clone(&registry)));
        runner.spawn(opener);
        Ok(Self {
            registry,
            runner,
            provider,
        })
    }
}
