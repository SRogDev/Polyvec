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
//! Dependency rule: this crate may depend on `polyvec-core` and
//! `polyvec-storage` only — never the other way around. Actix/Tokio types
//! never leave this crate.
//!
//! Text-query-via-embedding endpoints are **Phase 4** and do not exist here.

pub mod config;
pub mod error;
pub mod registry;
pub mod routes;
pub mod schemas;

use std::sync::Arc;

pub use config::ServerConfig;
pub use error::{ApiError, json_error_handler};
pub use registry::Registry;
pub use routes::configure_routes;

/// Maximum `k` accepted by the search endpoint.
pub const MAX_K: usize = 1000;
/// Minimum search width (`ef`) accepted by the search endpoint.
pub const MIN_SEARCH_EF: usize = 8;
/// Maximum search width (`ef`) accepted by the search endpoint.
pub const MAX_SEARCH_EF: usize = 10_000;

/// State shared by every handler: the collection registry.
#[derive(Clone)]
pub struct AppState {
    /// Lazily-opened collection stores rooted at the data dir.
    pub registry: Arc<Registry>,
}

impl AppState {
    /// Builds shared state from the server config.
    #[must_use]
    pub fn new(config: &ServerConfig) -> Self {
        Self {
            registry: Arc::new(Registry::new(config.data_dir.clone())),
        }
    }
}
