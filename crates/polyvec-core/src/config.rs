//! Collection configuration contracts.
//!
//! [`CollectionConfig`] is validated at creation time: dimension bounds, HNSW
//! parameter sanity, model tag shape. A collection that fails validation is
//! never persisted.

use serde::{Deserialize, Serialize};

use crate::{CollectionId, DistanceMetric, Error, Result};

/// Maximum vector dimension accepted (bounds memory per vector).
pub const MAX_DIMENSION: u32 = 4096;

/// Identifies the embedding model a collection's vectors came from.
///
/// Stored so a future phase can refuse to mix vectors from different models
/// (same dimension, different semantics) in one collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTag {
    /// Model name, e.g. `"bge-m3"`.
    pub name: String,
    /// Model version, e.g. `"1.0"`.
    pub version: String,
}

impl ModelTag {
    /// Builds a tag; both parts must be non-empty.
    ///
    /// # Errors
    /// [`Error::Validation`] if either part is empty.
    pub fn new(name: &str, version: &str) -> Result<Self> {
        if name.is_empty() || version.is_empty() {
            return Err(Error::Validation {
                detail: "model tag name and version must both be non-empty".to_string(),
            });
        }
        Ok(Self {
            name: name.to_string(),
            version: version.to_string(),
        })
    }
}

/// HNSW build/search parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HnswParams {
    /// Max neighbors per node per layer (`4–64`; `hnsw_rs` hard-fails above 256,
    /// we stay far below).
    pub m: usize,
    /// Search width during construction.
    pub ef_construction: usize,
    /// Search width at query time. Must scale with collection size
    /// (measured: ef=64 under-recalls at N=10k; ef=200 recovers ~0.97).
    pub ef_search: usize,
    /// Expected element count (allocation hint; the index grows past it).
    pub max_elements: usize,
}

impl Default for HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 64,
            max_elements: 1_000_000,
        }
    }
}

impl HnswParams {
    /// Checks the parameter ranges.
    ///
    /// # Errors
    /// [`Error::Config`] if any parameter is out of range.
    pub fn validate(&self) -> Result<()> {
        if !(4..=64).contains(&self.m) {
            return Err(Error::Config {
                detail: format!("hnsw m must be 4..=64, got {}", self.m),
            });
        }
        if self.ef_construction < 16 {
            return Err(Error::Config {
                detail: format!(
                    "hnsw ef_construction must be >= 16, got {}",
                    self.ef_construction
                ),
            });
        }
        if self.ef_search < 8 {
            return Err(Error::Config {
                detail: format!("hnsw ef_search must be >= 8, got {}", self.ef_search),
            });
        }
        if self.max_elements == 0 {
            return Err(Error::Config {
                detail: "hnsw max_elements must be >= 1".to_string(),
            });
        }
        Ok(())
    }
}

/// Validated configuration of a collection. Immutable after creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionConfig {
    /// Collection identity.
    pub name: CollectionId,
    /// Vector dimension; every vector in the collection must match.
    pub dimension: u32,
    /// Distance metric; fixed for the life of the collection.
    pub metric: DistanceMetric,
    /// Embedding model the vectors came from, if known.
    pub model: Option<ModelTag>,
    /// HNSW parameters.
    pub hnsw: HnswParams,
}

impl CollectionConfig {
    /// Builds and validates a config.
    ///
    /// # Errors
    /// [`Error::Config`] if the dimension is 0 or above [`MAX_DIMENSION`],
    /// or if the HNSW parameters are out of range.
    pub fn new(name: CollectionId, dimension: u32, metric: DistanceMetric) -> Result<Self> {
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(Error::Config {
                detail: format!("dimension must be 1..={MAX_DIMENSION}, got {dimension}"),
            });
        }
        let hnsw = HnswParams::default();
        hnsw.validate()?;
        Ok(Self {
            name,
            dimension,
            metric,
            model: None,
            hnsw,
        })
    }

    /// Attaches the embedding-model tag.
    #[must_use]
    pub fn with_model(mut self, model: ModelTag) -> Self {
        self.model = Some(model);
        self
    }

    /// Overrides the HNSW parameters (validated).
    ///
    /// # Errors
    /// [`Error::Config`] if the parameters are out of range.
    pub fn with_hnsw(mut self, hnsw: HnswParams) -> Result<Self> {
        hnsw.validate()?;
        self.hnsw = hnsw;
        Ok(self)
    }
}
