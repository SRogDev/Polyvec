//! redb-backed catalog: the transactional source of truth.
//!
//! Tables (all `&str -> &str`, JSON values — debuggable with any tool):
//!
//! * `collections` — single row `"collection"` → [`CollectionRecord`].
//! * `documents` — doc id → [`Document`](polyvec_core::Document).
//! * `chunks` — chunk id → [`Chunk`](polyvec_core::Chunk).
//! * `vectors` — vector id → [`VectorRecord`] (node id, values, chunk link,
//!   hash, tombstone flag). Values live here (not just in the ANN dump) so
//!   the index can always be rebuilt from the catalog.
//! * `meta` — `"format_version"`, `"generation"`, `"synced_generation"`,
//!   `"next_node_id"`.
//!
//! Every mutation is one redb write transaction; uncommitted transactions
//! roll back. The ANN dump files are a rebuildable cache over this data.

use std::path::Path;

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};

use polyvec_core::{
    Chunk, ChunkId, CollectionConfig, CollectionLifecycle, ContentHash, Document, DocumentId,
    Error, Result, VectorId,
};

use crate::layout::CATALOG_NAME;

/// Catalog format version (bump when a table layout changes).
pub(crate) const CATALOG_FORMAT_VERSION: u32 = 1;

const T_COLLECTIONS: TableDefinition<&str, &str> = TableDefinition::new("collections");
const T_DOCUMENTS: TableDefinition<&str, &str> = TableDefinition::new("documents");
const T_CHUNKS: TableDefinition<&str, &str> = TableDefinition::new("chunks");
const T_VECTORS: TableDefinition<&str, &str> = TableDefinition::new("vectors");
const T_META: TableDefinition<&str, &str> = TableDefinition::new("meta");

const K_COLLECTION: &str = "collection";
const K_FORMAT_VERSION: &str = "format_version";
const K_GENERATION: &str = "generation";
const K_SYNCED_GENERATION: &str = "synced_generation";
const K_NEXT_NODE_ID: &str = "next_node_id";

/// The persisted collection row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CollectionRecord {
    /// Validated collection configuration.
    pub config: CollectionConfig,
    /// Lifecycle stage.
    pub lifecycle: CollectionLifecycle,
}

/// The persisted vector row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VectorRecord {
    /// Graph node id in the current index generation.
    pub node_id: u64,
    /// The embedding (source of truth for rebuilds).
    pub values: Vec<f32>,
    /// Owning chunk, if inserted via [`crate::CollectionStore::insert_vector_for_chunk`].
    pub chunk_id: Option<String>,
    /// Hex content hash of the values.
    pub hash: String,
    /// Polyvec-owned deletion flag (`hnsw_rs` has no delete API).
    pub tombstoned: bool,
}

/// Converts redb errors into [`Error::Storage`].
///
/// Generic because redb 4.x uses distinct error types per operation
/// (`DatabaseError`, `TransactionError`, `TableError`, …).
fn storage_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Storage {
        detail: format!("redb: {e}"),
    }
}

/// The catalog handle.
pub(crate) struct Catalog {
    db: Database,
}

impl Catalog {
    /// Creates a fresh catalog at `<dir>/catalog.redb`, initializing metadata.
    ///
    /// # Errors
    /// [`Error::Storage`] on I/O or redb failures.
    pub(crate) fn create(dir: &Path) -> Result<Self> {
        let db = Database::create(dir.join(CATALOG_NAME)).map_err(storage_err)?;
        let catalog = Self { db };
        let txn = catalog.db.begin_write().map_err(storage_err)?;
        {
            // Open every table once so reads never hit "table does not exist".
            let _ = txn.open_table(T_COLLECTIONS).map_err(storage_err)?;
            let _ = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
            let _ = txn.open_table(T_CHUNKS).map_err(storage_err)?;
            let _ = txn.open_table(T_VECTORS).map_err(storage_err)?;
            let mut meta = txn.open_table(T_META).map_err(storage_err)?;
            meta.insert(
                K_FORMAT_VERSION,
                CATALOG_FORMAT_VERSION.to_string().as_str(),
            )
            .map_err(storage_err)?;
            meta.insert(K_GENERATION, "0").map_err(storage_err)?;
            meta.insert(K_SYNCED_GENERATION, "0").map_err(storage_err)?;
            meta.insert(K_NEXT_NODE_ID, "0").map_err(storage_err)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(catalog)
    }

    /// Opens an existing catalog.
    ///
    /// # Errors
    /// [`Error::Storage`] if the file is missing or unreadable.
    pub(crate) fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(CATALOG_NAME);
        if !path.exists() {
            return Err(Error::NotFound {
                what: format!("collection catalog at {}", dir.display()),
            });
        }
        let db = Database::open(&path).map_err(storage_err)?;
        Ok(Self { db })
    }

    /// Reads a metadata value.
    pub(crate) fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_META).map_err(storage_err)?;
        Ok(table
            .get(key)
            .map_err(storage_err)?
            .map(|g| g.value().to_string()))
    }

    /// Writes a metadata value (own transaction).
    pub(crate) fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_META).map_err(storage_err)?;
            table.insert(key, value).map_err(storage_err)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// Reads a u64 metadata value (missing → 0).
    pub(crate) fn meta_u64(&self, key: &str) -> Result<u64> {
        Ok(self
            .meta_get(key)?
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0))
    }

    /// Returns the catalog format version.
    pub(crate) fn format_version(&self) -> Result<u32> {
        Ok(self
            .meta_get(K_FORMAT_VERSION)?
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0))
    }

    /// Reads the collection record.
    pub(crate) fn collection_get(&self) -> Result<Option<CollectionRecord>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_COLLECTIONS).map_err(storage_err)?;
        table
            .get(K_COLLECTION)
            .map_err(storage_err)?
            .map(|g| serde_json::from_str(g.value()))
            .transpose()
            .map_err(|e| Error::Storage {
                detail: format!("decode collection record: {e}"),
            })
    }

    /// Writes the collection record (own transaction).
    pub(crate) fn collection_put(&self, record: &CollectionRecord) -> Result<()> {
        let json = serde_json::to_string(record).map_err(|e| Error::Storage {
            detail: format!("encode collection record: {e}"),
        })?;
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_COLLECTIONS).map_err(storage_err)?;
            table
                .insert(K_COLLECTION, json.as_str())
                .map_err(storage_err)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// Stores a document (own transaction).
    pub(crate) fn document_put(&self, doc: &Document) -> Result<()> {
        let json = serde_json::to_string(doc).map_err(|e| Error::Storage {
            detail: format!("encode document: {e}"),
        })?;
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
            table
                .insert(doc.id.as_str(), json.as_str())
                .map_err(storage_err)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// Reads a document.
    pub(crate) fn document_get(&self, id: &DocumentId) -> Result<Option<Document>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
        table
            .get(id.as_str())
            .map_err(storage_err)?
            .map(|g| serde_json::from_str(g.value()))
            .transpose()
            .map_err(|e| Error::Storage {
                detail: format!("decode document: {e}"),
            })
    }

    /// Removes a document. Returns `true` if it existed.
    pub(crate) fn document_remove(&self, id: &DocumentId) -> Result<bool> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        let removed = {
            let mut table = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
            table.remove(id.as_str()).map_err(storage_err)?.is_some()
        };
        txn.commit().map_err(storage_err)?;
        Ok(removed)
    }

    /// Number of stored documents.
    pub(crate) fn document_count(&self) -> Result<u64> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
        table.len().map_err(storage_err)
    }

    /// All documents, in key order. Used by the lexical rebuild (the
    /// catalog is the source of truth for the tantivy index).
    pub(crate) fn documents_all(&self) -> Result<Vec<Document>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_DOCUMENTS).map_err(storage_err)?;
        let mut out = Vec::new();
        let mut iter = table.iter().map_err(storage_err)?;
        for entry in iter.by_ref() {
            let (_k, v) = entry.map_err(storage_err)?;
            let doc: Document = serde_json::from_str(v.value()).map_err(|e| Error::Storage {
                detail: format!("decode document: {e}"),
            })?;
            out.push(doc);
        }
        Ok(out)
    }

    /// Stores a chunk (own transaction).
    pub(crate) fn chunk_put(&self, chunk: &Chunk) -> Result<()> {
        let json = serde_json::to_string(chunk).map_err(|e| Error::Storage {
            detail: format!("encode chunk: {e}"),
        })?;
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_CHUNKS).map_err(storage_err)?;
            table
                .insert(chunk.id.as_str(), json.as_str())
                .map_err(storage_err)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// All chunks of a document, ordered by index.
    pub(crate) fn chunks_of(&self, doc_id: &DocumentId) -> Result<Vec<Chunk>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_CHUNKS).map_err(storage_err)?;
        let mut chunks = Vec::new();
        let mut iter = table.iter().map_err(storage_err)?;
        for entry in iter.by_ref() {
            let (_, v) = entry.map_err(storage_err)?;
            let chunk: Chunk = serde_json::from_str(v.value()).map_err(|e| Error::Storage {
                detail: format!("decode chunk: {e}"),
            })?;
            if chunk.document_id == *doc_id {
                chunks.push(chunk);
            }
        }
        chunks.sort_by_key(|c| c.index);
        Ok(chunks)
    }

    /// Reads a chunk by id.
    pub(crate) fn chunk_get(&self, id: &ChunkId) -> Result<Option<Chunk>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_CHUNKS).map_err(storage_err)?;
        table
            .get(id.as_str())
            .map_err(storage_err)?
            .map(|g| serde_json::from_str(g.value()))
            .transpose()
            .map_err(|e| Error::Storage {
                detail: format!("decode chunk: {e}"),
            })
    }

    /// Removes a chunk. Returns `true` if it existed.
    pub(crate) fn chunk_remove(&self, id: &ChunkId) -> Result<bool> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        let removed = {
            let mut table = txn.open_table(T_CHUNKS).map_err(storage_err)?;
            table.remove(id.as_str()).map_err(storage_err)?.is_some()
        };
        txn.commit().map_err(storage_err)?;
        Ok(removed)
    }

    /// Number of stored chunks.
    pub(crate) fn chunk_count(&self) -> Result<u64> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_CHUNKS).map_err(storage_err)?;
        table.len().map_err(storage_err)
    }

    /// Reads a vector record.
    pub(crate) fn vector_get(&self, id: &VectorId) -> Result<Option<VectorRecord>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_VECTORS).map_err(storage_err)?;
        table
            .get(id.as_str())
            .map_err(storage_err)?
            .map(|g| serde_json::from_str(g.value()))
            .transpose()
            .map_err(|e| Error::Storage {
                detail: format!("decode vector record: {e}"),
            })
    }

    /// All vector records with their ids.
    pub(crate) fn vectors_all(&self) -> Result<Vec<(VectorId, VectorRecord)>> {
        let txn = self.db.begin_read().map_err(storage_err)?;
        let table = txn.open_table(T_VECTORS).map_err(storage_err)?;
        let mut out = Vec::new();
        let mut iter = table.iter().map_err(storage_err)?;
        for entry in iter.by_ref() {
            let (k, v) = entry.map_err(storage_err)?;
            let id = VectorId::parse(k.value()).map_err(|e| Error::Storage {
                detail: format!("decode vector id: {e}"),
            })?;
            let record: VectorRecord =
                serde_json::from_str(v.value()).map_err(|e| Error::Storage {
                    detail: format!("decode vector record: {e}"),
                })?;
            out.push((id, record));
        }
        Ok(out)
    }

    /// Atomically marks a vector tombstoned and bumps the generation.
    pub(crate) fn vector_tombstone(&self, id: &VectorId) -> Result<()> {
        // Decode first (own scope) so the read guard is dropped before the write.
        let mut record: VectorRecord = {
            let txn = self.db.begin_read().map_err(storage_err)?;
            let table = txn.open_table(T_VECTORS).map_err(storage_err)?;
            let Some(guard) = table.get(id.as_str()).map_err(storage_err)? else {
                return Err(Error::NotFound {
                    what: format!("vector '{id}'"),
                });
            };
            serde_json::from_str(guard.value()).map_err(|e| Error::Storage {
                detail: format!("decode vector record: {e}"),
            })?
        };
        record.tombstoned = true;
        let json = serde_json::to_string(&record).map_err(|e| Error::Storage {
            detail: format!("encode vector record: {e}"),
        })?;
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_VECTORS).map_err(storage_err)?;
            table
                .insert(id.as_str(), json.as_str())
                .map_err(storage_err)?;
            Self::bump_generation_in(&txn)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// Atomically clears all tombstone flags — bumps the generation only if
    /// any flag was actually cleared. Used by compacting rebuilds.
    pub(crate) fn vector_untombstone_all(&self) -> Result<()> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        {
            let mut table = txn.open_table(T_VECTORS).map_err(storage_err)?;
            let mut dirty: Vec<(String, String)> = Vec::new();
            let mut iter = table.iter().map_err(storage_err)?;
            for entry in iter.by_ref() {
                let (k, v) = entry.map_err(storage_err)?;
                let mut record: VectorRecord =
                    serde_json::from_str(v.value()).map_err(|e| Error::Storage {
                        detail: format!("decode vector record: {e}"),
                    })?;
                if record.tombstoned {
                    record.tombstoned = false;
                    let json = serde_json::to_string(&record).map_err(|e| Error::Storage {
                        detail: format!("encode vector record: {e}"),
                    })?;
                    dirty.push((k.value().to_string(), json));
                }
            }
            drop(iter);
            if dirty.is_empty() {
                return Ok(());
            }
            for (k, json) in &dirty {
                table
                    .insert(k.as_str(), json.as_str())
                    .map_err(storage_err)?;
            }
            Self::bump_generation_in(&txn)?;
        }
        txn.commit().map_err(storage_err)?;
        Ok(())
    }

    /// Inserts a vector record, allocating the next node id, bumping the
    /// generation — all in one transaction. Returns the node id.
    pub(crate) fn vector_insert_new(
        &self,
        id: &VectorId,
        values: Vec<f32>,
        chunk_id: Option<String>,
    ) -> Result<u64> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        let node_id = {
            let mut vectors = txn.open_table(T_VECTORS).map_err(storage_err)?;
            let hash = ContentHash::of(&f32_slice_to_bytes(&values)).to_hex();
            // Read node counter + generation, then write both back — the meta
            // table is opened exactly once in this transaction.
            let (next, generation) = {
                let meta = txn.open_table(T_META).map_err(storage_err)?;
                let next: u64 = meta
                    .get(K_NEXT_NODE_ID)
                    .map_err(storage_err)?
                    .and_then(|g| g.value().parse::<u64>().ok())
                    .unwrap_or(0);
                let generation: u64 = meta
                    .get(K_GENERATION)
                    .map_err(storage_err)?
                    .and_then(|g| g.value().parse::<u64>().ok())
                    .unwrap_or(0);
                (next, generation)
            };
            let record = VectorRecord {
                node_id: next,
                values,
                chunk_id,
                hash,
                tombstoned: false,
            };
            let json = serde_json::to_string(&record).map_err(|e| Error::Storage {
                detail: format!("encode vector record: {e}"),
            })?;
            vectors
                .insert(id.as_str(), json.as_str())
                .map_err(storage_err)?;
            drop(vectors);
            let mut meta = txn.open_table(T_META).map_err(storage_err)?;
            meta.insert(K_NEXT_NODE_ID, (next + 1).to_string().as_str())
                .map_err(storage_err)?;
            meta.insert(K_GENERATION, (generation + 1).to_string().as_str())
                .map_err(storage_err)?;
            next
        };
        txn.commit().map_err(storage_err)?;
        Ok(node_id)
    }

    /// Inserts many vector records in ONE transaction (single generation
    /// bump). Returns the allocated node ids in order.
    pub(crate) fn vector_insert_batch(
        &self,
        items: Vec<(VectorId, Vec<f32>, Option<String>)>,
    ) -> Result<Vec<u64>> {
        let txn = self.db.begin_write().map_err(storage_err)?;
        let node_ids = {
            // Read counters first (meta opened once, then dropped).
            let (mut next, generation) = {
                let meta = txn.open_table(T_META).map_err(storage_err)?;
                let next: u64 = meta
                    .get(K_NEXT_NODE_ID)
                    .map_err(storage_err)?
                    .and_then(|g| g.value().parse::<u64>().ok())
                    .unwrap_or(0);
                let generation: u64 = meta
                    .get(K_GENERATION)
                    .map_err(storage_err)?
                    .and_then(|g| g.value().parse::<u64>().ok())
                    .unwrap_or(0);
                (next, generation)
            };
            let mut vectors = txn.open_table(T_VECTORS).map_err(storage_err)?;
            let mut node_ids = Vec::with_capacity(items.len());
            for (id, values, chunk_id) in items {
                let hash = ContentHash::of(&f32_slice_to_bytes(&values)).to_hex();
                let record = VectorRecord {
                    node_id: next,
                    values,
                    chunk_id,
                    hash,
                    tombstoned: false,
                };
                let json = serde_json::to_string(&record).map_err(|e| Error::Storage {
                    detail: format!("encode vector record: {e}"),
                })?;
                vectors
                    .insert(id.as_str(), json.as_str())
                    .map_err(storage_err)?;
                node_ids.push(next);
                next += 1;
            }
            drop(vectors);
            let mut meta = txn.open_table(T_META).map_err(storage_err)?;
            meta.insert(K_NEXT_NODE_ID, next.to_string().as_str())
                .map_err(storage_err)?;
            meta.insert(K_GENERATION, (generation + 1).to_string().as_str())
                .map_err(storage_err)?;
            node_ids
        };
        txn.commit().map_err(storage_err)?;
        Ok(node_ids)
    }

    /// Reads the current generation / synced generation / next node id.
    pub(crate) fn generation(&self) -> Result<u64> {
        self.meta_u64(K_GENERATION)
    }

    /// Marks the catalog synced up to `generation`.
    pub(crate) fn mark_synced(&self, generation: u64) -> Result<()> {
        self.meta_set(K_SYNCED_GENERATION, &generation.to_string())
    }

    /// Reads the synced generation.
    pub(crate) fn synced_generation(&self) -> Result<u64> {
        self.meta_u64(K_SYNCED_GENERATION)
    }

    fn bump_generation_in(txn: &redb::WriteTransaction) -> Result<()> {
        let mut meta = txn.open_table(T_META).map_err(storage_err)?;
        let generation: u64 = meta
            .get(K_GENERATION)
            .map_err(storage_err)?
            .and_then(|g| g.value().parse::<u64>().ok())
            .unwrap_or(0);
        meta.insert(K_GENERATION, (generation + 1).to_string().as_str())
            .map_err(storage_err)?;
        Ok(())
    }
}

/// Encodes an f32 slice as little-endian bytes (for hashing).
fn f32_slice_to_bytes(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}
