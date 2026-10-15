//! Ingestion wiring for the server: embedding provider construction, the
//! server's [`CollectionOpener`]/[`DocStore`] adapters over the collection
//! registry, and [`JobRunner`] construction.
//!
//! The runner executes pipeline work on Tokio's blocking pool, so the
//! adapters below lock the per-collection `Mutex<CollectionStore>` directly
//! — they are only ever called from blocking threads.

use std::sync::Arc;

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, Document, DocumentId, Error, Result, Vector,
};
use polyvec_embeddings::{EmbeddingProvider, RemoteEmbeddingProvider, StubEmbeddingProvider};
use polyvec_ingestion::{
    CollectionOpener, DocStore, JobRunner, JobStore, MarkitConverter, RunnerLimits,
};

use crate::config::{EmbeddingKind, ServerConfig};
use crate::registry::{Registry, SharedStore, lock_store};

/// Builds the configured embedding provider.
///
/// The API key (remote) comes from the environment only and is never logged;
/// [`StubEmbeddingProvider`] is logged loudly as dev-only at construction.
///
/// # Errors
/// Returns a message when the remote provider is selected but its
/// environment is incomplete or malformed.
pub fn build_provider(
    config: &ServerConfig,
) -> std::result::Result<Option<Arc<dyn EmbeddingProvider>>, String> {
    match &config.embedding {
        EmbeddingKind::None => Ok(None),
        EmbeddingKind::Stub { dimension } => {
            log::warn!(
                "embedding provider is the deterministic STUB (dimension {dimension}): \
                 NOT for production use — vectors are a hash function, not a model"
            );
            let provider = StubEmbeddingProvider::new(*dimension).map_err(|e| e.to_string())?;
            Ok(Some(Arc::new(provider)))
        }
        EmbeddingKind::Remote => {
            let provider = RemoteEmbeddingProvider::from_env()?;
            let tag = provider.model_id();
            log::info!(
                "embedding provider: remote '{}' version '{}'",
                tag.name,
                tag.version
            );
            Ok(Some(Arc::new(provider)))
        }
    }
}

/// Builds the ingestion [`JobRunner`] (not yet spawned) rooted at the data
/// directory.
///
/// # Errors
/// Returns a message when the job store (`{data_dir}/jobs.redb`) cannot be
/// opened.
pub fn build_runner(
    config: &ServerConfig,
    provider: Option<Arc<dyn EmbeddingProvider>>,
) -> std::result::Result<JobRunner, String> {
    let store = JobStore::open(&config.data_dir.join("jobs.redb")).map_err(|e| e.to_string())?;
    Ok(JobRunner::new(
        store,
        provider,
        Arc::new(MarkitConverter::new()),
        RunnerLimits {
            queue_capacity: config.job_queue_capacity,
            max_concurrency: config.job_concurrency,
            ..RunnerLimits::default()
        },
    ))
}

/// The server's [`CollectionOpener`]: resolves collections through the
/// shared registry (which opens and caches them on demand).
pub struct ServerOpener {
    registry: Arc<Registry>,
}

impl ServerOpener {
    /// Wraps the shared collection registry.
    #[must_use]
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }
}

impl CollectionOpener for ServerOpener {
    fn open(&self, id: &CollectionId) -> Result<Arc<dyn DocStore>> {
        let store = self.registry.get_or_open(id)?;
        Ok(Arc::new(ServerDocStore { store }))
    }
}

/// The server's [`DocStore`]: locks the per-collection mutex per call.
/// Only ever used from the runner's blocking threads.
pub struct ServerDocStore {
    store: SharedStore,
}

impl DocStore for ServerDocStore {
    fn config(&self) -> Result<CollectionConfig> {
        Ok(lock_store(&self.store)?.config().clone())
    }

    fn get_document(&self, id: &DocumentId) -> Result<Option<(Document, Vec<Chunk>)>> {
        let store = lock_store(&self.store)?;
        match store.get_document(id) {
            Ok(found) => Ok(Some(found)),
            Err(Error::NotFound { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    fn vectors_present(&self, chunks: &[Chunk]) -> Result<bool> {
        let store = lock_store(&self.store)?;
        store.vectors_present(chunks)
    }

    fn ingest_document(&self, doc: &Document, chunks: &[Chunk], vectors: &[Vector]) -> Result<()> {
        let mut store = lock_store(&self.store)?;
        store.ingest_document(doc, chunks, vectors)
    }

    fn stamp_model_tag(&self, model: &polyvec_core::ModelTag) -> Result<()> {
        let mut store = lock_store(&self.store)?;
        store.stamp_model_tag(model)
    }

    fn delete_document(&self, id: &DocumentId) -> Result<()> {
        let mut store = lock_store(&self.store)?;
        store.delete_document(id)
    }
}
