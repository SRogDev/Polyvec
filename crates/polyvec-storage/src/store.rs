//! [`CollectionStore`]: one collection's durable database.
//!
//! ## Commit protocol (the honest version)
//!
//! There is **no atomicity across redb and the hnsw dump files** — they are
//! separate files and no protocol here pretends otherwise. Instead:
//!
//! 1. **redb commits first.** Every vector write (insert/update/delete) is a
//!    redb transaction that updates the vector record, the tombstone flag,
//!    and the `generation` counter. When the transaction commits, the write
//!    is durable — even if the process dies a nanosecond later.
//! 2. **The in-memory index mutates second.** If the process dies between 1
//!    and 2, the next open rebuilds the index from redb (see below).
//! 3. **`flush()` checkpoints.** The hnsw graph is dumped to temp files,
//!    renamed into place, checksummed into the sidecar (temp + rename), and
//!    only then is `synced_generation` advanced in redb.
//!
//! ## Recovery on open
//!
//! * Sidecar missing / dump files missing / dump older than the catalog
//!   (`sidecar.generation < catalog.generation`) → **rebuild from redb**
//!   (slow path, always correct), then checkpoint. Reported as
//!   [`Recovery::RebuiltFromCatalog`].
//! * Sidecar newer than the catalog (`sidecar.generation > generation`) →
//!   [`Error::Corrupt`] (impossible through this API; means external
//!   tampering).
//! * Sidecar version / dimension / metric mismatch → [`Error::Corrupt`].
//! * Checksum mismatch → [`Error::Corrupt`] with a recovery hint (delete the
//!   dump files + sidecar and reopen to rebuild from the catalog). This is a
//!   deliberate stop: a damaged dump is never silently trusted.
//! * Dump matches the catalog but `synced_generation` lags (crash between
//!   dump and the mark-synced transaction) → the mark is completed now,
//!   reported as [`Recovery::CompletedFlush`].
//!
//! ## Deletion
//!
//! Tombstone set owned by Polyvec (`hnsw_rs` has no delete API). The tombstone
//! flag commits in redb, so deletes hide from search **immediately** —
//! before any flush. [`CollectionStore::rebuild`] (also triggered
//! automatically by [`CollectionStore::flush`] past the tombstone ratio)
//! compacts the graph.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use polyvec_core::{
    Chunk, ChunkId, CollectionConfig, CollectionLifecycle, Document, DocumentId, Error,
    INDEX_FORMAT_VERSION, IndexState, ModelTag, Result, SyncStatus, Vector, VectorId,
};
use polyvec_index::{AnnIndex, TombstoneSet};

use crate::catalog::{CATALOG_FORMAT_VERSION, Catalog, CollectionRecord};
use crate::layout::{self, DUMP_BASENAME, DUMP_TMP_BASENAME, Sidecar};

/// Minimum tombstone count before auto-compaction kicks in.
/// `flush()` compacts when tombstones exceed 1/5 of live vectors.
const TOMBSTONE_MIN: u64 = 16;

/// What `open` had to do to reach a consistent state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Dump matched the catalog; nothing to repair.
    Clean,
    /// The dump was current but the mark-synced transaction never committed
    /// (crash inside `flush`); the mark was completed during open.
    CompletedFlush {
        /// The generation now marked synced.
        generation: u64,
    },
    /// The dump was missing or stale; the index was rebuilt from the redb
    /// catalog and checkpointed.
    RebuiltFromCatalog {
        /// Why the rebuild happened.
        reason: String,
    },
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// Stable vector identity.
    pub vector_id: VectorId,
    /// Distance under the collection's metric (smaller is closer).
    pub distance: f32,
}

/// A stored vector as returned by [`CollectionStore::get_vector`].
#[derive(Clone, Debug, PartialEq)]
pub struct StoredVector {
    /// Stable vector identity.
    pub id: VectorId,
    /// The embedding.
    pub values: Vec<f32>,
    /// Owning chunk, if linked.
    pub chunk_id: Option<ChunkId>,
}

/// Aggregate counts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Live (non-tombstoned) vectors.
    pub live_vectors: u64,
    /// Tombstoned vectors (hidden, still in the graph until rebuild).
    pub tombstoned: u64,
    /// Stored documents.
    pub documents: u64,
    /// Stored chunks.
    pub chunks: u64,
    /// Committed write batches.
    pub generation: u64,
    /// Generation the on-disk dump reflects.
    pub synced_generation: u64,
}

/// One collection's durable database: redb catalog + ANN index + recovery.
pub struct CollectionStore {
    dir: PathBuf,
    pub(crate) catalog: Catalog,
    config: CollectionConfig,
    index: AnnIndex,
    tombstones: TombstoneSet,
    /// node id → vector id (includes tombstoned nodes).
    reverse: HashMap<usize, VectorId>,
    live_vectors: u64,
    documents: u64,
    chunks: u64,
    generation: u64,
    synced_generation: u64,
}

impl CollectionStore {
    // ------------------------------------------------------------------
    // construction
    // ------------------------------------------------------------------

    /// Creates a new collection in `dir` (which may not exist yet).
    ///
    /// # Errors
    /// [`Error::AlreadyExists`] if `dir` already holds a collection;
    /// [`Error::Storage`] on I/O failures. `config` was validated at
    /// construction ([`CollectionConfig::new`]).
    pub fn create(dir: &Path, config: CollectionConfig) -> Result<Self> {
        if dir.join(crate::layout::CATALOG_NAME).exists() {
            return Err(Error::AlreadyExists {
                what: format!("collection at {}", dir.display()),
            });
        }
        fs::create_dir_all(dir).map_err(|e| Error::Storage {
            detail: format!("create collection dir {}: {e}", dir.display()),
        })?;
        let catalog = Catalog::create(dir)?;
        catalog.collection_put(&CollectionRecord {
            config: config.clone(),
            lifecycle: CollectionLifecycle::Active,
        })?;
        let index = AnnIndex::new(config.metric, &config.hnsw, config.dimension);
        // No dump yet — the first checkpoint writes one. The sidecar records
        // the empty state so open() takes the rebuild path (a no-op here).
        layout::write_sidecar(
            dir,
            &Sidecar::empty(config.dimension, &config.metric.to_string()),
        )?;
        Ok(Self {
            dir: dir.to_path_buf(),
            catalog,
            config,
            index,
            tombstones: TombstoneSet::new(),
            reverse: HashMap::new(),
            live_vectors: 0,
            documents: 0,
            chunks: 0,
            generation: 0,
            synced_generation: 0,
        })
    }

    /// Opens an existing collection, recovering if needed.
    ///
    /// Returns the store plus what recovery was required. See the module
    /// docs for the exact protocol.
    ///
    /// # Errors
    /// [`Error::NotFound`] if `dir` holds no collection;
    /// [`Error::Corrupt`] on version/dimension/metric/checksum mismatch
    /// (never a panic); [`Error::Storage`] on I/O failures.
    pub fn open(dir: &Path) -> Result<(Self, Recovery)> {
        Self::open_inner(dir)
    }

    /// Actual open implementation (kept separate for readability).
    fn open_inner(dir: &Path) -> Result<(Self, Recovery)> {
        let catalog = Catalog::open(dir)?;
        if catalog.format_version()? != CATALOG_FORMAT_VERSION {
            return Err(Error::Corrupt {
                detail: format!(
                    "catalog format version {} unsupported (code speaks {CATALOG_FORMAT_VERSION})",
                    catalog.format_version()?
                ),
            });
        }
        let record = catalog.collection_get()?.ok_or_else(|| Error::Corrupt {
            detail: "catalog has no collection record".to_string(),
        })?;
        let config = record.config;
        let generation = catalog.generation()?;
        let synced_generation = catalog.synced_generation()?;
        let sidecar = layout::read_sidecar(dir)?;

        let decision = decide_open(dir, &config, generation, sidecar)?;

        let mut store = Self {
            dir: dir.to_path_buf(),
            catalog,
            config: config.clone(),
            index: AnnIndex::new(config.metric, &config.hnsw, config.dimension),
            tombstones: TombstoneSet::new(),
            reverse: HashMap::new(),
            live_vectors: 0,
            documents: 0,
            chunks: 0,
            generation,
            synced_generation,
        };

        let recovery = match decision {
            OpenDecision::Fast(s) => {
                store.index = AnnIndex::load(
                    dir,
                    &s.basename,
                    config.metric,
                    config.dimension,
                    usize::try_from(s.point_count).expect("point count exceeds address space"),
                )?;
                store.rebuild_memory_maps()?;
                if synced_generation == generation {
                    Recovery::Clean
                } else {
                    // Crash between dump and mark-synced: complete the mark.
                    store.catalog.mark_synced(generation)?;
                    store.synced_generation = generation;
                    Recovery::CompletedFlush { generation }
                }
            }
            OpenDecision::Rebuild(reason) => {
                store.rebuild_from_catalog()?;
                store.checkpoint()?;
                Recovery::RebuiltFromCatalog { reason }
            }
        };
        Ok((store, recovery))
    }

    /// Rebuilds the in-memory tombstone set, reverse map, and live count
    /// from the catalog (used after loading a dump).
    fn rebuild_memory_maps(&mut self) -> Result<()> {
        self.tombstones.clear();
        self.reverse.clear();
        self.live_vectors = 0;
        self.documents = self.catalog.document_count()?;
        self.chunks = self.catalog.chunk_count()?;
        for (id, record) in self.catalog.vectors_all()? {
            let node = node_idx(record.node_id);
            self.reverse.insert(node, id);
            if record.tombstoned {
                self.tombstones.insert(node);
            } else {
                self.live_vectors += 1;
            }
        }
        Ok(())
    }

    /// Builds a fresh index from the catalog's live vectors (slow path).
    /// Tombstone flags are cleared — the new graph has no dead nodes.
    fn rebuild_from_catalog(&mut self) -> Result<()> {
        let mut live: Vec<(u64, VectorId, Vec<f32>)> = self
            .catalog
            .vectors_all()?
            .into_iter()
            .filter(|(_, r)| !r.tombstoned)
            .map(|(id, r)| (r.node_id, id, r.values))
            .collect();
        live.sort_by_key(|(node, _, _)| *node);

        let index = AnnIndex::new(self.config.metric, &self.config.hnsw, self.config.dimension);
        if !live.is_empty() {
            let owned: Vec<Vec<f32>> = live.iter().map(|(_, _, v)| v.clone()).collect();
            let refs: Vec<(&Vec<f32>, usize)> = owned
                .iter()
                .zip(live.iter())
                .map(|(v, (node, _, _))| (v, node_idx(*node)))
                .collect();
            index.insert_batch(&refs)?;
        }
        self.index = index;
        self.catalog.vector_untombstone_all()?;
        self.generation = self.catalog.generation()?;
        self.tombstones.clear();
        self.reverse.clear();
        self.live_vectors = 0;
        for (node, id, _) in &live {
            self.reverse.insert(node_idx(*node), id.clone());
            self.live_vectors += 1;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // reads
    // ------------------------------------------------------------------

    /// The collection configuration.
    #[must_use]
    pub fn config(&self) -> &CollectionConfig {
        &self.config
    }

    /// Current index state (generation, sync status, counts).
    #[must_use]
    pub fn index_state(&self) -> IndexState {
        IndexState {
            index_type: "hnsw_rs".to_string(),
            format_version: INDEX_FORMAT_VERSION,
            generation: self.generation,
            sync: if self.generation == self.synced_generation {
                SyncStatus::Clean
            } else {
                SyncStatus::Dirty
            },
            vector_count: self.live_vectors,
            tombstone_count: self.tombstones.len() as u64,
        }
    }

    /// Aggregate counts.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            live_vectors: self.live_vectors,
            tombstoned: self.tombstones.len() as u64,
            documents: self.documents,
            chunks: self.chunks,
            generation: self.generation,
            synced_generation: self.synced_generation,
        }
    }

    /// Top-k search. Tombstoned vectors never appear.
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on dimension
    /// mismatch or non-finite query components.
    pub fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<SearchHit>> {
        self.index
            .search(query, k, ef, &self.tombstones)?
            .into_iter()
            .map(|hit| {
                let vector_id = self
                    .reverse
                    .get(&hit.node_id)
                    .ok_or_else(|| Error::Internal {
                        detail: format!(
                            "index returned node {} with no catalog mapping",
                            hit.node_id
                        ),
                    })?;
                Ok(SearchHit {
                    vector_id: vector_id.clone(),
                    distance: hit.distance,
                })
            })
            .collect()
    }

    /// Reads a stored vector.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id is unknown or tombstoned.
    pub fn get_vector(&self, id: &VectorId) -> Result<StoredVector> {
        let record = self
            .catalog
            .vector_get(id)?
            .ok_or_else(|| Error::NotFound {
                what: format!("vector '{id}'"),
            })?;
        if record.tombstoned {
            return Err(Error::NotFound {
                what: format!("vector '{id}' (deleted)"),
            });
        }
        Ok(StoredVector {
            id: id.clone(),
            values: record.values,
            chunk_id: record.chunk_id.map(|s| ChunkId::parse(&s)).transpose()?,
        })
    }

    /// Reads a document with its chunks (ordered by index).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the document is unknown.
    pub fn get_document(&self, id: &DocumentId) -> Result<(Document, Vec<Chunk>)> {
        let doc = self
            .catalog
            .document_get(id)?
            .ok_or_else(|| Error::NotFound {
                what: format!("document '{id}'"),
            })?;
        let chunks = self.catalog.chunks_of(id)?;
        Ok((doc, chunks))
    }

    /// Reads a chunk by id. Returns `None` when unknown.
    ///
    /// Used by the retrieval layer to enrich hits with chunk text.
    ///
    /// # Errors
    /// [`Error::Storage`] if the catalog cannot be read.
    pub fn get_chunk(&self, id: &ChunkId) -> Result<Option<Chunk>> {
        self.catalog.chunk_get(id)
    }

    /// Every document with its chunks. Used to rebuild the lexical index
    /// from the catalog on collection open (the catalog is the source of
    /// truth; the tantivy index is derived).
    ///
    /// # Errors
    /// [`Error::Storage`] if the catalog cannot be read.
    pub fn all_documents(&self) -> Result<Vec<(Document, Vec<Chunk>)>> {
        let mut out = Vec::new();
        for doc in self.catalog.documents_all()? {
            let chunks = self.catalog.chunks_of(&doc.id)?;
            out.push((doc, chunks));
        }
        Ok(out)
    }

    // ------------------------------------------------------------------
    // writes
    // ------------------------------------------------------------------

    /// Inserts a vector. The id must be new (or tombstoned, which re-inserts).
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on dimension
    /// mismatch; [`Error::AlreadyExists`] if a live vector has this id.
    pub fn insert_vector(&mut self, vector: Vector) -> Result<()> {
        self.insert_vector_inner(vector, None)
    }

    /// Inserts many vectors atomically (one catalog transaction).
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) on the first
    /// bad vector (nothing is inserted); [`Error::AlreadyExists`] on id
    /// collision.
    pub fn insert_vectors(&mut self, vectors: Vec<Vector>) -> Result<()> {
        let dim = self.config.dimension;
        let mut seen = HashSet::new();
        for v in &vectors {
            if v.dimension() != dim {
                return Err(Error::Validation {
                    detail: format!(
                        "dimension mismatch: vector has {}, collection expects {dim}",
                        v.dimension()
                    ),
                });
            }
            if !seen.insert(v.id.clone()) {
                return Err(Error::Validation {
                    detail: format!("duplicate vector id '{}' in batch", v.id),
                });
            }
            if let Some(existing) = self.catalog.vector_get(&v.id)? {
                if !existing.tombstoned {
                    return Err(Error::AlreadyExists {
                        what: format!("vector '{}'", v.id),
                    });
                }
            }
        }
        let items: Vec<(VectorId, Vec<f32>, Option<String>)> = vectors
            .into_iter()
            .map(|v| (v.id, v.values, None))
            .collect();
        let node_ids = self.catalog.vector_insert_batch(items.clone())?;
        let owned: Vec<Vec<f32>> = items.iter().map(|(_, v, _)| v.clone()).collect();
        let refs: Vec<(&Vec<f32>, usize)> = owned
            .iter()
            .zip(node_ids.iter())
            .map(|(v, n)| (v, node_idx(*n)))
            .collect();
        self.index.insert_batch(&refs)?;
        for ((id, _, _), node) in items.iter().zip(node_ids.iter()) {
            self.reverse.insert(node_idx(*node), id.clone());
            // NOTE: for re-inserts over a tombstoned id, the OLD node stays
            // tombstoned in the graph (a dead duplicate); only the new node
            // is mapped. The stale tombstone is cleared by the next rebuild.
        }
        self.live_vectors += node_ids.len() as u64;
        self.generation = self.catalog.generation()?;
        Ok(())
    }

    fn insert_vector_inner(&mut self, vector: Vector, chunk_id: Option<ChunkId>) -> Result<()> {
        let dim = self.config.dimension;
        if vector.dimension() != dim {
            return Err(Error::Validation {
                detail: format!(
                    "dimension mismatch: vector has {}, collection expects {dim}",
                    vector.dimension()
                ),
            });
        }
        if let Some(existing) = self.catalog.vector_get(&vector.id)? {
            if !existing.tombstoned {
                return Err(Error::AlreadyExists {
                    what: format!("vector '{}'", vector.id),
                });
            }
            // Re-insert over a tombstoned id: the old node stays tombstoned
            // in the graph (a dead duplicate); the new node takes over the
            // identity. The stale tombstone is cleared by the next rebuild.
        }
        let node_id = self.catalog.vector_insert_new(
            &vector.id,
            vector.values.clone(),
            chunk_id.map(|c| c.to_string()),
        )?;
        self.index.insert(node_idx(node_id), &vector.values)?;
        self.reverse.insert(node_idx(node_id), vector.id);
        self.live_vectors += 1;
        self.generation = self.catalog.generation()?;
        Ok(())
    }

    /// Inserts the embedding for a chunk (id derived from the chunk).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the chunk is unknown; otherwise like
    /// [`CollectionStore::insert_vector`].
    pub fn insert_vector_for_chunk(
        &mut self,
        chunk_id: &ChunkId,
        values: Vec<f32>,
    ) -> Result<VectorId> {
        if self.catalog.chunk_get(chunk_id)?.is_none() {
            return Err(Error::NotFound {
                what: format!("chunk '{chunk_id}'"),
            });
        }
        let vector_id = VectorId::for_chunk(chunk_id);
        let vector = Vector::new(vector_id.clone(), values, self.config.dimension)?;
        self.insert_vector_inner(vector, Some(chunk_id.clone()))?;
        Ok(vector_id)
    }

    /// Updates a vector: same id, new values (tombstone old node + insert
    /// new node — `hnsw_rs` has no in-place update).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id is unknown or deleted.
    pub fn update_vector(&mut self, vector: Vector) -> Result<()> {
        let dim = self.config.dimension;
        if vector.dimension() != dim {
            return Err(Error::Validation {
                detail: format!(
                    "dimension mismatch: vector has {}, collection expects {dim}",
                    vector.dimension()
                ),
            });
        }
        let existing = self
            .catalog
            .vector_get(&vector.id)?
            .ok_or_else(|| Error::NotFound {
                what: format!("vector '{}'", vector.id),
            })?;
        if existing.tombstoned {
            return Err(Error::NotFound {
                what: format!("vector '{}' (deleted)", vector.id),
            });
        }
        let chunk_id = existing.chunk_id.map(|s| ChunkId::parse(&s)).transpose()?;
        // Tombstone the old node (durable first), then insert the new one.
        self.catalog.vector_tombstone(&vector.id)?;
        let old_node = node_idx(existing.node_id);
        self.tombstones.insert(old_node);
        self.reverse.remove(&old_node);
        self.live_vectors -= 1;

        let node_id = self.catalog.vector_insert_new(
            &vector.id,
            vector.values.clone(),
            chunk_id.map(|c| c.to_string()),
        )?;
        self.index.insert(node_idx(node_id), &vector.values)?;
        self.reverse.insert(node_idx(node_id), vector.id);
        self.live_vectors += 1;
        self.generation = self.catalog.generation()?;
        Ok(())
    }

    /// Deletes a vector. The tombstone commits in redb, so the vector hides
    /// from search immediately — no flush needed. Idempotent on deletes.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the id is unknown.
    pub fn delete_vector(&mut self, id: &VectorId) -> Result<()> {
        let existing = self
            .catalog
            .vector_get(id)?
            .ok_or_else(|| Error::NotFound {
                what: format!("vector '{id}'"),
            })?;
        if existing.tombstoned {
            return Ok(());
        }
        self.catalog.vector_tombstone(id)?;
        self.tombstones.insert(node_idx(existing.node_id));
        self.live_vectors -= 1;
        self.generation = self.catalog.generation()?;
        Ok(())
    }

    /// Stores a document with its chunks.
    ///
    /// # Errors
    /// [`Error::Validation`](polyvec_core::Error::Validation) if a chunk
    /// names a different document.
    pub fn put_document(&mut self, doc: &Document, chunks: &[Chunk]) -> Result<()> {
        for chunk in chunks {
            if chunk.document_id != doc.id {
                return Err(Error::Validation {
                    detail: format!(
                        "chunk '{}' belongs to document '{}', not '{}'",
                        chunk.id, chunk.document_id, doc.id
                    ),
                });
            }
        }
        let old_chunks = self.catalog.chunks_of(&doc.id)?.len() as u64;
        let is_new = self.catalog.document_get(&doc.id)?.is_none();
        self.catalog.document_put(doc)?;
        for chunk in chunks {
            self.catalog.chunk_put(chunk)?;
        }
        // Re-putting an existing document (new version) replaces its chunks;
        // counters must reflect the replacement, not accumulate.
        if is_new {
            self.documents += 1;
        }
        self.chunks = self.chunks - old_chunks + chunks.len() as u64;
        Ok(())
    }

    /// Writes one document version atomically from the ingestion pipeline:
    /// tombstones the previous version's vectors, removes its chunks, then
    /// stores the new document, chunks, and vectors.
    ///
    /// The caller assigns version-scoped chunk ids (see
    /// [`polyvec_core::ChunkId::for_versioned_index`]), so the new vectors
    /// never collide with the tombstoned old ones. Old vectors hide from
    /// search immediately (tombstone post-filter); the vector batch itself
    /// is atomic via [`CollectionStore::insert_vectors`].
    ///
    /// If the vector insert fails, the document and chunks are stored but no
    /// vectors are — the ingestion runner treats this as a retryable partial
    /// write and re-runs the same version idempotently.
    ///
    /// # Errors
    /// [`Error::Validation`] if a chunk belongs to another document or a
    /// vector is invalid; [`Error::AlreadyExists`] on live vector id
    /// collision; [`Error::Storage`] on I/O failures.
    pub fn ingest_document(
        &mut self,
        doc: &Document,
        chunks: &[Chunk],
        vectors: &[Vector],
    ) -> Result<()> {
        for chunk in chunks {
            if chunk.document_id != doc.id {
                return Err(Error::Validation {
                    detail: format!(
                        "chunk '{}' belongs to document '{}', not '{}'",
                        chunk.id, chunk.document_id, doc.id
                    ),
                });
            }
        }
        // Retire the previous version: tombstone its vectors (immediately
        // hidden from search) and drop its chunk records.
        let old_chunks = self.catalog.chunks_of(&doc.id)?;
        let is_new = self.catalog.document_get(&doc.id)?.is_none();
        for chunk in &old_chunks {
            let vector_id = VectorId::for_chunk(&chunk.id);
            if let Some(record) = self.catalog.vector_get(&vector_id)? {
                if !record.tombstoned {
                    self.catalog.vector_tombstone(&vector_id)?;
                    self.tombstones.insert(node_idx(record.node_id));
                    self.live_vectors -= 1;
                }
            }
            self.catalog.chunk_remove(&chunk.id)?;
        }
        // Store the new version.
        self.catalog.document_put(doc)?;
        for chunk in chunks {
            self.catalog.chunk_put(chunk)?;
        }
        // Atomic vector batch (validates dimension/dupes/collisions first).
        self.insert_vectors(vectors.to_vec())?;
        if is_new {
            self.documents += 1;
        }
        self.chunks = self.chunks - old_chunks.len() as u64 + chunks.len() as u64;
        Ok(())
    }

    /// Claims the collection for an embedding model (one-time).
    ///
    /// Sets the config's model tag and persists it. Setting the same tag
    /// twice is a no-op; a *different* tag is refused — vectors from two
    /// models must never mix silently in one collection.
    ///
    /// # Errors
    /// [`Error::AlreadyExists`] if another model tag is already set;
    /// [`Error::Storage`] if the config cannot be persisted.
    pub fn stamp_model_tag(&mut self, tag: &ModelTag) -> Result<()> {
        match &self.config.model {
            Some(existing) if *existing == *tag => return Ok(()),
            Some(existing) => {
                return Err(Error::AlreadyExists {
                    what: format!(
                        "collection '{}' already uses model {} {}; refusing {} {}",
                        self.config.name, existing.name, existing.version, tag.name, tag.version
                    ),
                });
            }
            None => {}
        }
        self.config.model = Some(tag.clone());
        let mut record = self
            .catalog
            .collection_get()?
            .ok_or_else(|| Error::Corrupt {
                detail: "collection record missing while stamping model tag".to_string(),
            })?;
        record.config = self.config.clone();
        self.catalog.collection_put(&record)?;
        Ok(())
    }

    /// Returns `true` when every chunk in `chunks` has a live
    /// (non-tombstoned) vector in the catalog.
    ///
    /// The ingestion pipeline uses this on the unchanged-content path: a
    /// previous attempt may have stored the document and chunks but failed
    /// before the vector batch committed. Without this check, a retry would
    /// see the unchanged hash and report "skipped" for a document that has
    /// no searchable vectors — partial progress reported as complete.
    ///
    /// # Errors
    /// [`Error::Storage`] if the catalog cannot be read.
    pub fn vectors_present(&self, chunks: &[Chunk]) -> Result<bool> {
        for chunk in chunks {
            let vector_id = VectorId::for_chunk(&chunk.id);
            match self.catalog.vector_get(&vector_id)? {
                Some(record) if !record.tombstoned => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Deletes a document, its chunks, and tombstones their vectors.
    ///
    /// # Errors
    /// [`Error::NotFound`] if the document is unknown.
    pub fn delete_document(&mut self, id: &DocumentId) -> Result<()> {
        if self.catalog.document_get(id)?.is_none() {
            return Err(Error::NotFound {
                what: format!("document '{id}'"),
            });
        }
        let chunks = self.catalog.chunks_of(id)?;
        let n_chunks = chunks.len() as u64;
        for chunk in &chunks {
            let vector_id = VectorId::for_chunk(&chunk.id);
            if let Some(record) = self.catalog.vector_get(&vector_id)? {
                if !record.tombstoned {
                    self.catalog.vector_tombstone(&vector_id)?;
                    self.tombstones.insert(node_idx(record.node_id));
                    self.live_vectors -= 1;
                }
            }
            self.catalog.chunk_remove(&chunk.id)?;
        }
        // Referential integrity: drop every relation touching the document
        // or its chunks, in both directions. A deleted endpoint must never
        // be reachable through the relation API or traversal again.
        let mut endpoints: Vec<&str> = Vec::with_capacity(chunks.len() + 1);
        endpoints.push(id.as_str());
        for chunk in &chunks {
            endpoints.push(chunk.id.as_str());
        }
        self.remove_relations_of(&endpoints)?;
        self.catalog.document_remove(id)?;
        self.documents -= 1;
        self.chunks -= n_chunks;
        self.generation = self.catalog.generation()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // maintenance
    // ------------------------------------------------------------------

    /// Checkpoints: dumps the index, writes the sidecar, marks synced.
    ///
    /// Automatically compacts (rebuild) when the tombstone ratio exceeds
    /// [`TOMBSTONE_RATIO`] with at least [`TOMBSTONE_MIN`] tombstones.
    ///
    /// # Errors
    /// [`Error::Storage`] on dump or I/O failures.
    pub fn flush(&mut self) -> Result<()> {
        if self.should_compact() {
            return self.rebuild();
        }
        self.checkpoint()
    }

    /// Compacts: rebuilds the graph from live vectors only, clearing
    /// tombstones, then checkpoints.
    ///
    /// # Errors
    /// [`Error::Storage`] on dump or I/O failures.
    pub fn rebuild(&mut self) -> Result<()> {
        self.rebuild_from_catalog()?;
        self.checkpoint()
    }

    fn should_compact(&self) -> bool {
        let t = self.tombstones.len() as u64;
        // t/live > TOMBSTONE_RATIO (0.2), in integer math.
        t >= TOMBSTONE_MIN && (self.live_vectors == 0 || t > self.live_vectors / 5)
    }

    /// Dump + sidecar + mark-synced.
    fn checkpoint(&mut self) -> Result<()> {
        let generation = self.catalog.generation()?;
        let tombstone_count = self.tombstones.len() as u64;
        if self.live_vectors == 0 && tombstone_count == 0 {
            // Nothing to dump — record the clean empty state without
            // invoking hnsw_rs on an empty graph.
            let sidecar = Sidecar::empty(self.config.dimension, &self.config.metric.to_string());
            layout::write_sidecar(&self.dir, &sidecar)?;
        } else {
            // Dump to a temp basename, then rename into place: a crash
            // mid-dump never leaves a half-written live file behind.
            let used = self.index.dump(&self.dir, DUMP_TMP_BASENAME)?;
            let (tmp_graph, tmp_data) = layout::dump_paths(&self.dir, &used);
            let (fin_graph, fin_data) = layout::dump_paths(&self.dir, DUMP_BASENAME);
            fs::rename(&tmp_graph, &fin_graph).map_err(|e| Error::Storage {
                detail: format!("rename graph dump into place: {e}"),
            })?;
            fs::rename(&tmp_data, &fin_data).map_err(|e| Error::Storage {
                detail: format!("rename data dump into place: {e}"),
            })?;
            let sidecar = Sidecar {
                format_version: INDEX_FORMAT_VERSION,
                dimension: self.config.dimension,
                metric: self.config.metric.to_string(),
                basename: DUMP_BASENAME.to_string(),
                dump_present: true,
                sha256_graph: layout::sha256_file(&fin_graph)?,
                sha256_data: layout::sha256_file(&fin_data)?,
                generation,
                point_count: self.live_vectors + tombstone_count,
                tombstone_count,
            };
            layout::write_sidecar(&self.dir, &sidecar)?;
        }
        self.catalog.mark_synced(generation)?;
        self.generation = generation;
        self.synced_generation = generation;
        Ok(())
    }
}

// --- open helpers --------------------------------------------------------

/// How `open` reaches a consistent state.
enum OpenDecision {
    /// The dump matches the catalog: load it.
    Fast(Sidecar),
    /// The dump is missing or stale: rebuild from the catalog.
    Rebuild(String),
}

/// Decides between fast load and catalog rebuild (pure decision logic).
fn decide_open(
    dir: &Path,
    config: &CollectionConfig,
    generation: u64,
    sidecar: Option<Sidecar>,
) -> Result<OpenDecision> {
    let Some(s) = sidecar else {
        return Ok(OpenDecision::Rebuild("no sidecar found".to_string()));
    };
    if s.format_version != INDEX_FORMAT_VERSION {
        return Err(Error::Corrupt {
            detail: format!(
                "index format version {} unsupported (code speaks {INDEX_FORMAT_VERSION})",
                s.format_version
            ),
        });
    }
    if s.dimension != config.dimension {
        return Err(Error::Corrupt {
            detail: format!(
                "dump dimension {} != collection dimension {}",
                s.dimension, config.dimension
            ),
        });
    }
    if s.metric != config.metric.to_string() {
        return Err(Error::Corrupt {
            detail: format!(
                "dump metric '{}' != collection metric '{}'",
                s.metric, config.metric
            ),
        });
    }
    if s.dump_present {
        let (graph, data) = layout::dump_paths(dir, &s.basename);
        if graph.exists() && data.exists() {
            layout::verify_dump(dir, &s)?; // Corrupt on mismatch
            if s.generation > generation {
                return Err(Error::Corrupt {
                    detail: format!(
                        "dump generation {} is newer than catalog generation {generation} \
                         (external tampering?)",
                        s.generation
                    ),
                });
            }
            if s.generation < generation {
                return Ok(OpenDecision::Rebuild(format!(
                    "dump is stale (dump gen {}, catalog gen {generation})",
                    s.generation
                )));
            }
            return Ok(OpenDecision::Fast(s));
        }
        return Ok(OpenDecision::Rebuild("dump files missing".to_string()));
    }
    Ok(OpenDecision::Rebuild("no index dump yet".to_string()))
}

/// Converts a catalog node id (`u64`) to the `hnsw_rs` node id (`usize`).
///
/// Node ids come from a monotonic in-catalog counter; on 64-bit platforms
/// the conversion is lossless.
fn node_idx(node_id: u64) -> usize {
    usize::try_from(node_id).expect("node id exceeds address space")
}

/// Reads a collection's config without opening the full store.
///
/// Opens the redb catalog read-only and returns the stored
/// [`CollectionConfig`] — the ANN index is never loaded, so this is the
/// cheap way to enumerate collections (the HTTP server uses it for
/// `GET /v1/collections`).
///
/// # Errors
/// [`Error::NotFound`] if `dir` holds no collection;
/// [`Error::Storage`] on I/O or decode failures.
pub fn read_collection_config(dir: &Path) -> Result<CollectionConfig> {
    let catalog = Catalog::open(dir)?;
    catalog
        .collection_get()?
        .map(|record| record.config)
        .ok_or_else(|| Error::NotFound {
            what: format!("collection at {}", dir.display()),
        })
}
