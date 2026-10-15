//! Document/chunk metadata.
//!
//! Phase 2 stores metadata alongside documents and returns it with reads.
//! Filtering on metadata arrives with the retrieval layer (Phase 5); the
//! value model is fixed now so stored data stays forward-compatible.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A metadata value. Closed set on purpose: JSON-shaped, no nested maps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetadataValue {
    /// String value.
    Str(String),
    /// Integer value.
    Int(i64),
    /// Floating-point value.
    Float(f64),
    /// Boolean value.
    Bool(bool),
}

/// Key/value metadata attached to a document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Metadata(BTreeMap<String, MetadataValue>);

impl Metadata {
    /// Empty metadata.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a key.
    pub fn insert(&mut self, key: String, value: MetadataValue) {
        self.0.insert(key, value);
    }

    /// Reads a key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&MetadataValue> {
        self.0.get(key)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates over entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &MetadataValue)> {
        self.0.iter()
    }
}
