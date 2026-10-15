//! Documents and chunks: the logical units above raw vectors.
//!
//! A [`Document`] is the ingested artifact (a file, a URL, a note). A
//! [`Chunk`] is an ordered slice of it. Vectors (embeddings) attach to
//! chunks in Phase 2. Identity is deterministic (see [`crate::ids`]); content
//! hashes make change detection cheap.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{ChunkId, CollectionId, ContentHash, DocumentId, Metadata};

/// Where a document came from and how it entered the database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Entered through the API by a human/operator.
    Manual {
        /// Who did it (caller-supplied label).
        by: String,
        /// Unix seconds at creation.
        at_secs: u64,
    },
    /// Entered by an ingestion job.
    Ingested {
        /// Which job/pipeline.
        source: String,
        /// Unix seconds at ingestion.
        at_secs: u64,
    },
}

/// Unix seconds now (saturates to 0 before the epoch, which never happens).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A logical document: source identity, metadata, content hash, version.
///
/// Versions are explicit: re-ingesting changed bytes creates version N+1 with
/// the same [`DocumentId`]. Chunks and vectors point at a document *version*.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Stable identity (derived from collection + source identity).
    pub id: DocumentId,
    /// Owning collection.
    pub collection: CollectionId,
    /// Durable source name (URI, path, external key).
    pub source: String,
    /// User metadata.
    pub metadata: Metadata,
    /// Hash of the ingested bytes; change detection compares this.
    pub content_hash: ContentHash,
    /// Starts at 1; incremented by [`Document::with_content`].
    pub version: u64,
    /// How the document entered the database.
    pub provenance: Provenance,
}

impl Document {
    /// Creates a version-1 document.
    #[must_use]
    pub fn new(
        id: DocumentId,
        collection: CollectionId,
        source: String,
        content_hash: ContentHash,
        metadata: Metadata,
    ) -> Self {
        Self {
            id,
            collection,
            source,
            metadata,
            content_hash,
            version: 1,
            provenance: Provenance::Manual {
                by: "api".to_string(),
                at_secs: now_secs(),
            },
        }
    }

    /// Returns the next version with a new content hash.
    ///
    /// Identity, collection, source, metadata and provenance are preserved;
    /// only the hash and version change.
    #[must_use]
    pub fn with_content(&self, content_hash: ContentHash) -> Self {
        Self {
            content_hash,
            version: self.version + 1,
            ..self.clone()
        }
    }
}

/// An ordered slice of a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// Stable identity (derived from document id + index).
    pub id: ChunkId,
    /// Owning document.
    pub document_id: DocumentId,
    /// Position within the document (0-based).
    pub index: u32,
    /// The text slice.
    pub text: String,
    /// Character offset of the slice start in the document text.
    pub char_start: usize,
    /// Character offset of the slice end (exclusive).
    pub char_end: usize,
    /// Hash of [`Chunk::text`]; cheap change detection per chunk.
    pub hash: ContentHash,
}

impl Chunk {
    /// Creates a chunk; the id and hash are derived.
    #[must_use]
    pub fn new(
        document_id: DocumentId,
        index: u32,
        text: &str,
        char_start: usize,
        char_end: usize,
    ) -> Self {
        Self::new_inner(
            ChunkId::for_index(&document_id, index),
            document_id,
            index,
            text,
            char_start,
            char_end,
        )
    }

    /// Creates a chunk for `version` of `document_id`; the id is derived
    /// from (document, version, index) — see
    /// [`ChunkId::for_versioned_index`].
    ///
    /// The ingestion pipeline uses this so each document version gets fresh
    /// chunk (and vector) ids; the old version's vectors are tombstoned
    /// without id collisions.
    #[must_use]
    pub fn new_versioned(
        document_id: DocumentId,
        version: u64,
        index: u32,
        text: &str,
        char_start: usize,
        char_end: usize,
    ) -> Self {
        Self::new_inner(
            ChunkId::for_versioned_index(&document_id, version, index),
            document_id,
            index,
            text,
            char_start,
            char_end,
        )
    }

    fn new_inner(
        id: ChunkId,
        document_id: DocumentId,
        index: u32,
        text: &str,
        char_start: usize,
        char_end: usize,
    ) -> Self {
        Self {
            id,
            hash: ContentHash::of(text.as_bytes()),
            document_id,
            index,
            text: text.to_string(),
            char_start,
            char_end,
        }
    }
}
