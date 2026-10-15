//! ANN index: `hnsw_rs` wrapper with Polyvec-owned deletion.
//!
//! [`AnnIndex`] is an enum over the two supported metrics because `hnsw_rs`
//! is generic over its distance function. It owns:
//!
//! * dimension/finiteness validation (anndists panics on length mismatch —
//!   we return [`Error::Validation`](polyvec_core::Error::Validation) first),
//! * tombstone-aware search (oversample `k + tombstones`, post-filter, take
//!   k — deletes hide immediately),
//! * dump/load with an explicit point count (`hnsw_rs` exposes no count API).
//!
//! [`hnsw_rs` has no delete API](https://github.com/jpboth/hnswlib-rs), so
//! updates are *tombstone the old node + insert a new node*; periodic
//! rebuilds compact the graph. Node ids are assigned by the storage layer
//! and must be unique within one index generation.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use hnsw_rs::prelude::*;
use polyvec_core::{DistanceMetric, Error, HnswParams, Result};

use crate::{SearchHit, TombstoneSet};

/// Lifetime note: `HnswIo::load_hnsw` ties the loaded index's lifetime to the
/// `&mut HnswIo` handle (`'a: 'b`), so an owned long-lived index needs the
/// handle to live as long as the index. We `Box::leak` one `HnswIo` per
/// collection open — a ~100-byte struct holding two paths, no `unsafe`,
/// freed at process exit. The alternative (a self-referential-struct crate)
/// was judged heavier than the leak for one awkward upstream API.
fn leak_io(dir: &Path, basename: &str) -> &'static mut HnswIo {
    Box::leak(Box::new(HnswIo::new(dir, basename)))
}

/// The ANN index for one collection.
pub enum AnnIndex {
    /// Cosine-distance index.
    Cosine {
        /// The underlying HNSW graph.
        hnsw: Hnsw<'static, f32, DistCosine>,
        /// Expected vector dimension.
        dimension: u32,
        /// Inserted point count (`hnsw_rs` exposes none).
        count: AtomicUsize,
    },
    /// Euclidean-distance index.
    L2 {
        /// The underlying HNSW graph.
        hnsw: Hnsw<'static, f32, DistL2>,
        /// Expected vector dimension.
        dimension: u32,
        /// Inserted point count (`hnsw_rs` exposes none).
        count: AtomicUsize,
    },
}

impl AnnIndex {
    /// Creates an empty index for `metric` with `dimension`.
    #[must_use]
    pub fn new(metric: DistanceMetric, params: &HnswParams, dimension: u32) -> Self {
        // max_layer 16: Hnsw::new clamps to its internal NB_LAYER_MAX anyway.
        match metric {
            DistanceMetric::Cosine => Self::Cosine {
                hnsw: Hnsw::<f32, DistCosine>::new(
                    params.m,
                    params.max_elements,
                    16,
                    params.ef_construction,
                    DistCosine {},
                ),
                dimension,
                count: AtomicUsize::new(0),
            },
            DistanceMetric::L2 => Self::L2 {
                hnsw: Hnsw::<f32, DistL2>::new(
                    params.m,
                    params.max_elements,
                    16,
                    params.ef_construction,
                    DistL2 {},
                ),
                dimension,
                count: AtomicUsize::new(0),
            },
        }
    }

    /// The metric this index was built with.
    #[must_use]
    pub fn metric(&self) -> DistanceMetric {
        match self {
            Self::Cosine { .. } => DistanceMetric::Cosine,
            Self::L2 { .. } => DistanceMetric::L2,
        }
    }

    /// Expected vector dimension.
    #[must_use]
    pub fn dimension(&self) -> u32 {
        match self {
            Self::Cosine { dimension, .. } | Self::L2 { dimension, .. } => *dimension,
        }
    }

    /// Number of inserted points (includes tombstoned ones until rebuild).
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Cosine { count, .. } | Self::L2 { count, .. } => count.load(Ordering::Relaxed),
        }
    }

    /// Whether no point has been inserted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Validates a vector against this index (dimension + finiteness).
    fn check_vector(&self, values: &[f32], what: &str) -> Result<()> {
        if values.len() != self.dimension() as usize {
            return Err(Error::Validation {
                detail: format!(
                    "{what}: dimension mismatch: got {}, index expects {}",
                    values.len(),
                    self.dimension()
                ),
            });
        }
        if values.iter().any(|x| !x.is_finite()) {
            return Err(Error::Validation {
                detail: format!("{what}: components must be finite (no NaN/Inf)"),
            });
        }
        Ok(())
    }

    /// Inserts one vector under `node_id`.
    ///
    /// `node_id` must be unique within this index generation (the storage
    /// layer assigns them from a monotonic counter).
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on dimension
    /// mismatch or non-finite components.
    pub fn insert(&self, node_id: usize, values: &[f32]) -> Result<()> {
        self.check_vector(values, "insert")?;
        match self {
            Self::Cosine { hnsw, count, .. } => {
                hnsw.insert((values, node_id));
                count.fetch_add(1, Ordering::Relaxed);
            }
            Self::L2 { hnsw, count, .. } => {
                hnsw.insert((values, node_id));
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Inserts many vectors (parallel inside `hnsw_rs`).
    ///
    /// All items are validated before anything is inserted.
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on the first
    /// invalid item; nothing is inserted in that case.
    pub fn insert_batch(&self, items: &[(&Vec<f32>, usize)]) -> Result<()> {
        for (values, _) in items {
            self.check_vector(values, "insert_batch")?;
        }
        match self {
            Self::Cosine { hnsw, count, .. } => {
                hnsw.parallel_insert(items);
                count.fetch_add(items.len(), Ordering::Relaxed);
            }
            Self::L2 { hnsw, count, .. } => {
                hnsw.parallel_insert(items);
                count.fetch_add(items.len(), Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Top-k search with tombstones hidden.
    ///
    /// Requests `k + tombstones` candidates and post-filters, so deleted
    /// vectors never appear and live results still fill k when at least k
    /// live vectors exist. `ef < k` is clamped up, not an error.
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on dimension
    /// mismatch or non-finite query components.
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        tombstones: &TombstoneSet,
    ) -> Result<Vec<SearchHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        self.check_vector(query, "search query")?;
        // Oversample so tombstoned candidates can't starve the result set.
        let want = k + tombstones.len();
        let ef = ef.max(want);
        let neighbours = match self {
            Self::Cosine { hnsw, .. } => hnsw.search(query, want, ef),
            Self::L2 { hnsw, .. } => hnsw.search(query, want, ef),
        };
        Ok(neighbours
            .iter()
            .filter(|n| !tombstones.contains(n.get_origin_id()))
            .take(k)
            .map(|n| SearchHit {
                node_id: n.get_origin_id(),
                distance: n.get_distance(),
            })
            .collect())
    }

    /// Dumps the graph + data files as `$basename.hnsw.{graph,data}` in `dir`.
    ///
    /// Returns the effective basename (`hnsw_rs` may uniquify it; with our
    /// non-mmap usage it overwrites in place and returns `basename`).
    ///
    /// # Errors
    /// [`Error::Storage`](polyvec_core::Error::Storage) if the dump fails.
    pub fn dump(&self, dir: &Path, basename: &str) -> Result<String> {
        let used = match self {
            Self::Cosine { hnsw, .. } => AnnT::file_dump(hnsw, dir, basename),
            Self::L2 { hnsw, .. } => AnnT::file_dump(hnsw, dir, basename),
        }
        .map_err(|e| Error::Storage {
            detail: format!("hnsw dump failed: {e}"),
        })?;
        Ok(used)
    }

    /// Loads a dumped index. The dump's distance must match `metric`
    /// (`hnsw_rs` checks the stored distance name and fails otherwise).
    ///
    /// `point_count` seeds the point counter (`hnsw_rs` exposes none); the
    /// storage layer passes the catalog's live+dead count.
    ///
    /// # Errors
    /// [`Error::Storage`](polyvec_core::Error::Storage) if the files are
    /// missing, unreadable, or built with a different metric.
    pub fn load(
        dir: &Path,
        basename: &str,
        metric: DistanceMetric,
        dimension: u32,
        point_count: usize,
    ) -> Result<Self> {
        // NOTE: callers verify the sidecar checksum BEFORE reaching here, so
        // hnsw_rs's internal asserts (magic, dimension) cannot fire on
        // corrupt input — corrupt dumps become Error::Corrupt, never a panic.
        let io = leak_io(dir, basename);
        match metric {
            DistanceMetric::Cosine => {
                let hnsw: Hnsw<'static, f32, DistCosine> =
                    io.load_hnsw().map_err(|e| Error::Storage {
                        detail: format!("hnsw load failed: {e}"),
                    })?;
                Ok(Self::Cosine {
                    hnsw,
                    dimension,
                    count: AtomicUsize::new(point_count),
                })
            }
            DistanceMetric::L2 => {
                let hnsw: Hnsw<'static, f32, DistL2> =
                    io.load_hnsw().map_err(|e| Error::Storage {
                        detail: format!("hnsw load failed: {e}"),
                    })?;
                Ok(Self::L2 {
                    hnsw,
                    dimension,
                    count: AtomicUsize::new(point_count),
                })
            }
        }
    }
}
