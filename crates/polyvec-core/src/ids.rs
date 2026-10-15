//! Strongly-typed, validated identifiers.
//!
//! Identity rules (deterministic — the same inputs always yield the same id):
//!
//! * [`CollectionId`] — caller-chosen slug: lowercase ASCII alphanumeric,
//!   `-`, `_`; 1–64 chars.
//! * [`DocumentId`] — derived: `{collection}#{sha256(source_identity)[..24]}`.
//!   The same source identity in the same collection always maps to the same
//!   document, which is what makes re-ingestion idempotent.
//! * [`ChunkId`] — derived: `{document}#c{index:06}`. Chunk `i` of a document
//!   is always the same chunk. Version-scoped variant `{document}#v{version}`
//!   `#c{index:06}` (see [`ChunkId::for_versioned_index`]) gives each document
//!   version fresh chunk ids so old vectors can be tombstoned without id
//!   collisions.
//! * [`VectorId`] — derived: `{chunk}#v` for chunk embeddings (one vector per
//!   chunk in Phase 2).
//!
//! Raw strings still cross the FFI/test boundary via [`std::str::FromStr`],
//! which re-validates the shape.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// A validated newtype wrapper around the id string.
macro_rules! id_type {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            /// Borrows the id as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    CollectionId,
    "Identifies a collection. Caller-chosen slug: lowercase ASCII alphanumeric, `-`, `_`; 1–64 chars."
);
id_type!(
    DocumentId,
    "Identifies a document. Derived deterministically from collection + source identity."
);
id_type!(
    ChunkId,
    "Identifies a chunk. Derived deterministically from document id + chunk index."
);
id_type!(
    VectorId,
    "Identifies a stored vector. Derived deterministically from the chunk it embeds."
);

impl CollectionId {
    /// Validates a caller-chosen slug.
    ///
    /// # Errors
    /// [`Error::Validation`] if the slug is empty, longer than 64 chars, or
    /// contains anything outside lowercase ASCII alphanumeric, `-`, `_`.
    pub fn new(slug: &str) -> Result<Self> {
        let valid = !slug.is_empty()
            && slug.len() <= 64
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if valid {
            Ok(Self(slug.to_string()))
        } else {
            Err(Error::Validation {
                detail: format!("collection id must be 1-64 chars of [a-z0-9-_], got {slug:?}"),
            })
        }
    }
}

impl FromStr for CollectionId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

/// Short hex digest used to derive deterministic ids.
fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    format!("{digest:x}")[..24].to_string()
}

impl DocumentId {
    /// Derives the stable id for a source identity inside a collection.
    ///
    /// `source_identity` is whatever names the source durably (a URI, a file
    /// path, an external key). The same pair always yields the same id.
    #[must_use]
    pub fn for_source(collection: &CollectionId, source_identity: &str) -> Self {
        Self(format!(
            "{}#{}",
            collection.as_str(),
            short_hash(&format!("{}|{source_identity}", collection.as_str()))
        ))
    }

    /// Parses a previously derived id, validating its shape.
    ///
    /// # Errors
    /// [`Error::Validation`] if empty, longer than 256 chars, or containing
    /// control characters.
    pub fn parse(s: &str) -> Result<Self> {
        validate_derived_id(s, "document")?;
        Ok(Self(s.to_string()))
    }
}

impl FromStr for DocumentId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl ChunkId {
    /// Derives the stable id for chunk `index` of `document`.
    #[must_use]
    pub fn for_index(document: &DocumentId, index: u32) -> Self {
        Self(format!("{}#c{index:06}", document.as_str()))
    }

    /// Derives the stable id for chunk `index` of `version` of `document`.
    ///
    /// Version-scoped: re-ingesting changed content bumps the document
    /// version and yields fresh chunk ids, so the old version's vectors can
    /// be tombstoned without id collisions with the new version's vectors.
    #[must_use]
    pub fn for_versioned_index(document: &DocumentId, version: u64, index: u32) -> Self {
        Self(format!("{}#v{version}#c{index:06}", document.as_str()))
    }

    /// Parses a previously derived id, validating its shape.
    ///
    /// # Errors
    /// [`Error::Validation`] if the shape is invalid.
    pub fn parse(s: &str) -> Result<Self> {
        validate_derived_id(s, "chunk")?;
        Ok(Self(s.to_string()))
    }
}

impl FromStr for ChunkId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl VectorId {
    /// Derives the stable id for the embedding of `chunk`.
    #[must_use]
    pub fn for_chunk(chunk: &ChunkId) -> Self {
        Self(format!("{}#v", chunk.as_str()))
    }

    /// Recovers the chunk id this vector was derived from.
    ///
    /// Returns `None` unless the id has the `{chunk}#v` shape produced by
    /// [`VectorId::for_chunk`].
    #[must_use]
    pub fn chunk_id(&self) -> Option<ChunkId> {
        self.0.strip_suffix("#v").map(|s| ChunkId(s.to_string()))
    }

    /// Parses a previously derived id, validating its shape.
    ///
    /// # Errors
    /// [`Error::Validation`] if the shape is invalid.
    pub fn parse(s: &str) -> Result<Self> {
        validate_derived_id(s, "vector")?;
        Ok(Self(s.to_string()))
    }
}

impl FromStr for VectorId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Shared shape check for derived ids.
fn validate_derived_id(s: &str, kind: &str) -> Result<()> {
    let ok = !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(Error::Validation {
            detail: format!("{kind} id must be 1-256 chars without control characters"),
        })
    }
}
