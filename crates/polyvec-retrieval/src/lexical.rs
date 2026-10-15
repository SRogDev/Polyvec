//! tantivy-backed lexical index: one per collection, over chunk text.
//!
//! This is the **lexical subsystem** adopted in the Phase 1 audit
//! (tantivy 0.26.2, `default-features = false`). It deliberately does
//! **not** own the document model: it stores chunk ids + text + filter
//! terms, and every write is driven from the catalog's documents/chunks
//! (see `LexicalDocument`). The catalog (redb) is the source of truth; the
//! lexical index is rebuilt from it whenever a collection is opened, so a
//! stale or corrupt tantivy directory can never serve ghost results.
//!
//! Schema (fixed):
//!
//! * `chunk_id` — `STRING | STORED`: the chunk's id, one token, exact.
//! * `doc_id` — `STRING | STORED`: owning document; the delete/upsert key.
//! * `text` — `TEXT`: chunk text, default tokenizer (lowercased,
//!   alphanumeric), indexed but **not** stored (callers join hits back to
//!   the catalog for text).
//! * `meta` — `STRING`, multi-valued: one `key=<json value>` term per
//!   document metadata entry, for exact-match metadata filters.
//!
//! Threading: `LexicalIndex` is `Send` but not `Sync` (tantivy's
//! `IndexWriter`); holders wrap it in a `Mutex`, like `CollectionStore`.

use std::path::Path;

use polyvec_core::{
    Chunk, ChunkId, DocumentId, Error, Metadata, MetadataFilter, MetadataValue, Result,
};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value};
use tantivy::{Index, IndexReader, IndexWriter, TantivyDocument, Term};

/// Tantivy index heap for the writer (50 MiB — chunk texts are small).
const WRITER_HEAP_BYTES: usize = 50_000_000;

/// One document's worth of lexical entries: the unit of upsert/delete.
#[derive(Clone, Debug)]
pub struct LexicalDocument<'a> {
    /// Owning document (the upsert/delete key).
    pub id: &'a DocumentId,
    /// Document metadata (indexed as filter terms; chunks inherit it).
    pub metadata: &'a Metadata,
    /// Chunks to index.
    pub chunks: &'a [Chunk],
}

/// One lexical search hit.
#[derive(Clone, Debug, PartialEq)]
pub struct LexicalHit {
    /// The matching chunk.
    pub chunk_id: ChunkId,
    /// tantivy BM25 score. Higher is better; query-dependent scale —
    /// never comparable across queries or modes.
    pub score: f32,
}

fn storage_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Storage {
        detail: format!("lexical index {what}: {e}"),
    }
}

/// Encodes one metadata entry as a single exact-match term.
/// `key=<JSON value>`: `author="roger"`, `pages=3`, `ratio=0.5`,
/// `draft=false`. The JSON encoding is canonical (`serde_json`), so the
/// index side and the query side always agree.
fn meta_term(key: &str, value: &MetadataValue) -> String {
    let json = match value {
        MetadataValue::Str(s) => serde_json::to_string(s).unwrap_or_default(),
        MetadataValue::Int(i) => i.to_string(),
        MetadataValue::Float(f) => serde_json::to_string(f).unwrap_or_default(),
        MetadataValue::Bool(b) => b.to_string(),
    };
    format!("{key}={json}")
}

struct Fields {
    chunk: Field,
    doc: Field,
    text: Field,
    meta: Field,
}

fn build_schema() -> (Schema, Fields) {
    let mut builder = tantivy::schema::SchemaBuilder::new();
    let chunk = builder.add_text_field("chunk_id", STRING | STORED);
    let doc = builder.add_text_field("doc_id", STRING | STORED);
    let text = builder.add_text_field("text", TEXT);
    let meta = builder.add_text_field("meta", STRING);
    (
        builder.build(),
        Fields {
            chunk,
            doc,
            text,
            meta,
        },
    )
}

/// tantivy-backed per-collection lexical index over chunk text.
pub struct LexicalIndex {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    fields: Fields,
}

// Compile-time proof that `LexicalIndex` can cross into `web::block`
// (which requires `Send`). If tantivy ever stops being `Send`, this fails
// the build instead of degrading into a runtime surprise.
#[allow(dead_code)]
fn _assert_lexical_index_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<LexicalIndex>();
}

impl LexicalIndex {
    /// Opens the index at `dir`, creating the directory and an empty index
    /// when missing.
    ///
    /// # Errors
    /// [`Error::Storage`] when the directory or the index cannot be opened.
    pub fn open(dir: &Path) -> Result<Self> {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| storage_err("create dir", e))?;
        }
        std::fs::create_dir_all(dir).map_err(|e| storage_err("create dir", e))?;
        let (schema, fields) = build_schema();
        let mmap = MmapDirectory::open(dir).map_err(|e| storage_err("open directory", e))?;
        let index =
            Index::open_or_create(mmap, schema).map_err(|e| storage_err("open_or_create", e))?;
        Self::from_index(index, fields)
    }

    /// Destroys any index at `dir` and builds a fresh one from `docs`
    /// (single commit). Used on collection open: the catalog is the source
    /// of truth, so a rebuild guarantees the lexical view matches it.
    ///
    /// # Errors
    /// [`Error::Storage`] when the directory or the index cannot be written.
    pub fn rebuild(dir: &Path, docs: &[LexicalDocument<'_>]) -> Result<Self> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| storage_err("clear directory", e))?;
        }
        let mut this = Self::open(dir)?;
        for doc in docs {
            this.add_document(doc)?;
        }
        this.writer
            .commit()
            .map_err(|e| storage_err("rebuild commit", e))?;
        this.reader
            .reload()
            .map_err(|e| storage_err("rebuild reload", e))?;
        Ok(this)
    }

    fn from_index(index: Index, fields: Fields) -> Result<Self> {
        let writer = index
            .writer::<TantivyDocument>(WRITER_HEAP_BYTES)
            .map_err(|e| storage_err("create writer", e))?;
        let reader = index
            .reader()
            .map_err(|e| storage_err("create reader", e))?;
        Ok(Self {
            index,
            writer,
            reader,
            fields,
        })
    }

    /// Replaces all chunk entries of one document: deletes every entry
    /// carrying its `doc_id`, adds the new chunks, commits once.
    ///
    /// # Errors
    /// [`Error::Storage`] on tantivy failures.
    pub fn upsert_document(&mut self, doc: &LexicalDocument<'_>) -> Result<()> {
        self.delete_document_terms(doc.id);
        for chunk in doc.chunks {
            self.add_document(&LexicalDocument {
                id: doc.id,
                metadata: doc.metadata,
                chunks: std::slice::from_ref(chunk),
            })?;
        }
        self.commit_and_reload("upsert")
    }

    /// Removes every chunk entry of one document. Idempotent: deleting an
    /// unknown document is a no-op success.
    ///
    /// # Errors
    /// [`Error::Storage`] on tantivy failures.
    pub fn delete_document(&mut self, doc_id: &DocumentId) -> Result<()> {
        self.delete_document_terms(doc_id);
        self.commit_and_reload("delete")
    }

    /// BM25 search over chunk text, best-first, up to `k` hits.
    ///
    /// `filter` adds exact-match metadata terms (all `Must`). An empty or
    /// whitespace-only query, or `k == 0`, returns no hits. A query the
    /// tantivy grammar rejects is [`Error::Validation`] (400 at the API).
    ///
    /// # Errors
    /// [`Error::Storage`] on tantivy failures; [`Error::Validation`] on a
    /// syntactically invalid query; [`Error::Corrupt`] if an indexed entry
    /// lost its stored chunk id (a bug, never user error).
    pub fn search(
        &self,
        text: &str,
        k: usize,
        filter: Option<&MetadataFilter>,
    ) -> Result<Vec<LexicalHit>> {
        if k == 0 || text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let parser = QueryParser::for_index(&self.index, vec![self.fields.text]);
        let text_query = parser.parse_query(text).map_err(|e| Error::Validation {
            detail: format!("invalid lexical query: {e}"),
        })?;
        let query: Box<dyn Query> = match filter {
            Some(f) if !f.is_empty() => {
                let mut subqueries: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, text_query)];
                for (key, value) in &f.0 {
                    let term = Term::from_field_text(self.fields.meta, &meta_term(key, value));
                    subqueries.push((
                        Occur::Must,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                    ));
                }
                Box::new(BooleanQuery::new(subqueries))
            }
            _ => text_query,
        };
        let searcher = self.reader.searcher();
        let top: Vec<(f32, tantivy::DocAddress)> = searcher
            .search(&*query, &TopDocs::with_limit(k).order_by_score())
            .map_err(|e| storage_err("search", e))?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| storage_err("fetch hit", e))?;
            let chunk_id = doc
                .get_first(self.fields.chunk)
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Corrupt {
                    detail: "lexical entry missing stored chunk_id".to_string(),
                })?;
            hits.push(LexicalHit {
                chunk_id: ChunkId::parse(chunk_id)?,
                score,
            });
        }
        Ok(hits)
    }

    /// Number of indexed chunk entries (what a search can return).
    #[must_use]
    pub fn num_chunks(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn add_document(&self, doc: &LexicalDocument<'_>) -> Result<()> {
        for chunk in doc.chunks {
            let mut tantivy_doc = TantivyDocument::default();
            tantivy_doc.add_text(self.fields.chunk, chunk.id.as_str());
            tantivy_doc.add_text(self.fields.doc, doc.id.as_str());
            tantivy_doc.add_text(self.fields.text, &chunk.text);
            for (key, value) in doc.metadata.iter() {
                tantivy_doc.add_text(self.fields.meta, meta_term(key, value));
            }
            self.writer
                .add_document(tantivy_doc)
                .map_err(|e| storage_err("add document", e))?;
        }
        Ok(())
    }

    fn delete_document_terms(&self, doc_id: &DocumentId) {
        let term = Term::from_field_text(self.fields.doc, doc_id.as_str());
        self.writer.delete_term(term);
    }

    fn commit_and_reload(&mut self, what: &str) -> Result<()> {
        self.writer.commit().map_err(|e| storage_err(what, e))?;
        self.reader.reload().map_err(|e| storage_err("reload", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyvec_core::{CollectionId, MetadataValue};

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "polyvec-lexical-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn doc_id(source: &str) -> DocumentId {
        DocumentId::for_source(&CollectionId::new("kb").unwrap(), source)
    }

    fn chunk(doc: &DocumentId, index: u32, text: &str) -> Chunk {
        Chunk::new(doc.clone(), index, text, 0, text.len())
    }

    fn meta(pairs: &[(&str, MetadataValue)]) -> Metadata {
        let mut m = Metadata::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        m
    }

    fn lex_doc<'a>(
        id: &'a DocumentId,
        metadata: &'a Metadata,
        chunks: &'a [Chunk],
    ) -> LexicalDocument<'a> {
        LexicalDocument {
            id,
            metadata,
            chunks,
        }
    }

    fn hit_ids(hits: &[LexicalHit]) -> Vec<String> {
        hits.iter()
            .map(|h| h.chunk_id.as_str().to_string())
            .collect()
    }

    #[test]
    fn exact_term_search_finds_the_chunk() {
        let dir = tmp("exact");
        let mut idx = LexicalIndex::open(&dir).unwrap();
        let id = doc_id("src://rust");
        let metadata = Metadata::new();
        let chunks = vec![
            chunk(&id, 0, "ownership and borrowing are the heart of rust"),
            chunk(&id, 1, "tokio powers async rust programs"),
        ];
        idx.upsert_document(&lex_doc(&id, &metadata, &chunks))
            .unwrap();
        assert_eq!(idx.num_chunks(), 2);

        let hits = idx.search("borrowing", 10, None).unwrap();
        assert_eq!(hit_ids(&hits), vec![chunks[0].id.as_str().to_string()]);
        assert!(hits[0].score > 0.0, "BM25 scores are positive");

        // A term in no chunk returns no hits — never an error.
        assert!(
            idx.search("xylophone-absent-term", 10, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn bm25_ranks_repeated_term_first() {
        let dir = tmp("bm25");
        let mut idx = LexicalIndex::open(&dir).unwrap();
        let id = doc_id("src://ranks");
        let metadata = Metadata::new();
        let chunks = vec![
            chunk(&id, 0, "the cat sat on the mat"),
            chunk(&id, 1, "quantum quantum quantum foam and quantum foam"),
        ];
        idx.upsert_document(&lex_doc(&id, &metadata, &chunks))
            .unwrap();

        let hits = idx.search("quantum", 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk_id, chunks[1].id);
    }

    #[test]
    fn upsert_replaces_old_chunks_delete_removes_all() {
        let dir = tmp("upsert-delete");
        let mut idx = LexicalIndex::open(&dir).unwrap();
        let id = doc_id("src://changing");
        let metadata = Metadata::new();

        // Version 1.
        let v1 = vec![chunk(&id, 0, "the old text mentions blorpt")];
        idx.upsert_document(&lex_doc(&id, &metadata, &v1)).unwrap();
        assert_eq!(idx.search("blorpt", 10, None).unwrap().len(), 1);

        // Version 2 (same document id, new chunks): old text must vanish.
        let v2 = vec![chunk(&id, 0, "the new text mentions quux")];
        idx.upsert_document(&lex_doc(&id, &metadata, &v2)).unwrap();
        assert_eq!(idx.num_chunks(), 1, "upsert must not duplicate");
        assert!(idx.search("blorpt", 10, None).unwrap().is_empty());
        assert_eq!(idx.search("quux", 10, None).unwrap().len(), 1);

        // Delete: nothing left, and deleting twice is fine.
        idx.delete_document(&id).unwrap();
        assert_eq!(idx.num_chunks(), 0);
        assert!(idx.search("quux", 10, None).unwrap().is_empty());
        idx.delete_document(&id).unwrap();
    }

    #[test]
    fn index_survives_reopen() {
        let dir = tmp("reopen");
        let id = doc_id("src://persist");
        let metadata = Metadata::new();
        let chunks = vec![chunk(&id, 0, "persistent wombat knowledge")];
        {
            let mut idx = LexicalIndex::open(&dir).unwrap();
            idx.upsert_document(&lex_doc(&id, &metadata, &chunks))
                .unwrap();
        }
        // Drop + reopen: the commit must have been durable.
        let idx = LexicalIndex::open(&dir).unwrap();
        assert_eq!(idx.num_chunks(), 1);
        let hits = idx.search("wombat", 10, None).unwrap();
        assert_eq!(hit_ids(&hits), vec![chunks[0].id.as_str().to_string()]);
    }

    #[test]
    fn metadata_filter_selects_matching_documents() {
        let dir = tmp("filter");
        let mut idx = LexicalIndex::open(&dir).unwrap();
        let id_a = doc_id("src://a");
        let id_b = doc_id("src://b");
        let meta_a = meta(&[("author", MetadataValue::Str("roger".to_string()))]);
        let meta_b = meta(&[("author", MetadataValue::Str("ana".to_string()))]);
        let chunks_a = vec![chunk(&id_a, 0, "shared term alpha")];
        let chunks_b = vec![chunk(&id_b, 0, "shared term alpha")];
        idx.upsert_document(&lex_doc(&id_a, &meta_a, &chunks_a))
            .unwrap();
        idx.upsert_document(&lex_doc(&id_b, &meta_b, &chunks_b))
            .unwrap();

        // No filter: both match.
        assert_eq!(idx.search("alpha", 10, None).unwrap().len(), 2);

        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "author".to_string(),
            MetadataValue::Str("roger".to_string()),
        );
        let filter = MetadataFilter(map);
        let hits = idx.search("alpha", 10, Some(&filter)).unwrap();
        assert_eq!(hit_ids(&hits), vec![chunks_a[0].id.as_str().to_string()]);

        // A filter matching nothing returns nothing (not an error).
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "author".to_string(),
            MetadataValue::Str("nobody".to_string()),
        );
        assert!(
            idx.search("alpha", 10, Some(&MetadataFilter(map)))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_query_is_validation_not_panic() {
        let dir = tmp("badquery");
        let idx = LexicalIndex::open(&dir).unwrap();
        let err = idx.search("(((", 10, None).unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got: {err:?}");
        // Degenerate inputs are empty results, not errors.
        assert!(idx.search("   ", 10, None).unwrap().is_empty());
        assert!(idx.search("term", 0, None).unwrap().is_empty());
    }

    #[test]
    fn rebuild_from_catalog_snapshot() {
        let dir = tmp("rebuild");
        // Stale index with ghost content.
        let mut idx = LexicalIndex::open(&dir).unwrap();
        let ghost = doc_id("src://ghost");
        let metadata = Metadata::new();
        let ghost_chunks = vec![chunk(&ghost, 0, "ghostly apparition text")];
        idx.upsert_document(&lex_doc(&ghost, &metadata, &ghost_chunks))
            .unwrap();
        drop(idx);

        // Rebuild from the (new) catalog snapshot: ghosts are gone.
        let real = doc_id("src://real");
        let real_chunks = vec![chunk(&real, 0, "real document content here")];
        let docs = vec![lex_doc(&real, &metadata, &real_chunks)];
        let idx = LexicalIndex::rebuild(&dir, &docs).unwrap();
        assert_eq!(idx.num_chunks(), 1);
        assert!(idx.search("apparition", 10, None).unwrap().is_empty());
        assert_eq!(idx.search("real", 10, None).unwrap().len(), 1);
    }
}
