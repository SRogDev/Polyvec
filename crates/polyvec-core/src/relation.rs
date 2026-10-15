//! Typed relations between documents/chunks (Phase 6).
//!
//! Relations are explicitly supplied (no extraction, no LLM), directed
//! (`source` → `target`), and carry provenance in `metadata` (who/what
//! created the relation) plus `created_at` (when). The type vocabulary is
//! fixed — see [`RelationType`].
//!
//! Pure domain vocabulary: no storage, no HTTP, no async.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Metadata, Result};

/// The fixed initial relation-type vocabulary (Phase 6).
///
/// Deliberately small: relations are supplied by the caller, and every type
/// must earn its place with retrieval semantics. New types are a
/// versioned-vocabulary change, not a runtime extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// Source cites, links to, or builds on the target.
    References,
    /// Source is topically related to the target (weak link).
    RelatedTo,
    /// Source supersedes / replaces the target (versioning knowledge).
    Supersedes,
}

impl FromStr for RelationType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "references" => Ok(Self::References),
            "related_to" => Ok(Self::RelatedTo),
            "supersedes" => Ok(Self::Supersedes),
            other => Err(Error::Validation {
                detail: format!(
                    "unknown relation type '{other}'; expected 'references', 'related_to', or 'supersedes'"
                ),
            }),
        }
    }
}

/// A validated relation identifier.
///
/// Caller-chosen slug (`[a-zA-Z0-9_-]`, 1–128 chars) or server-generated
/// (`rel-00000001`, …) when the create request omits one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationId(String);

impl RelationId {
    /// Validates a caller-chosen id.
    ///
    /// # Errors
    /// [`Error::Validation`] if the id is empty, longer than 128 chars, or
    /// contains anything outside ASCII alphanumeric, `-`, `_`.
    pub fn new(id: &str) -> Result<Self> {
        let valid = !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if valid {
            Ok(Self(id.to_string()))
        } else {
            Err(Error::Validation {
                detail: format!("invalid relation id '{id}': 1-128 chars of [a-zA-Z0-9_-]"),
            })
        }
    }

    /// Borrows the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One directed relation between two entities (documents or chunks).
///
/// `source` and `target` hold document or chunk ids; endpoint existence is
/// enforced by the storage layer (referential integrity), not here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    /// Stable identity.
    pub id: RelationId,
    /// What the edge means.
    pub relation_type: RelationType,
    /// The edge's tail: a document or chunk id.
    pub source: String,
    /// The edge's head: a document or chunk id.
    pub target: String,
    /// Provenance and annotations (flat string/number/boolean values).
    /// Convention: `created_by` names who/what supplied the relation.
    pub metadata: Metadata,
    /// Unix seconds when the relation was recorded.
    pub created_at: i64,
}

impl Relation {
    /// Builds a relation, validating the endpoints.
    ///
    /// # Errors
    /// [`Error::Validation`] when an endpoint is empty or when
    /// `source == target` (self-loops are rejected: they carry no
    /// retrieval meaning and almost always signal a client bug).
    pub fn new(
        id: RelationId,
        relation_type: RelationType,
        source: String,
        target: String,
        metadata: Metadata,
        created_at: i64,
    ) -> Result<Self> {
        if source.is_empty() || target.is_empty() {
            return Err(Error::Validation {
                detail: "relation source and target must not be empty".to_string(),
            });
        }
        if source == target {
            return Err(Error::Validation {
                detail: "relation source and target must differ (self-loops are not allowed)"
                    .to_string(),
            });
        }
        Ok(Self {
            id,
            relation_type,
            source,
            target,
            metadata,
            created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MetadataValue;

    #[test]
    fn relation_type_from_str_round_trip() {
        for (s, t) in [
            ("references", RelationType::References),
            ("related_to", RelationType::RelatedTo),
            ("supersedes", RelationType::Supersedes),
        ] {
            assert_eq!(s.parse::<RelationType>().unwrap(), t);
            assert_eq!(serde_json::to_string(&t).unwrap(), format!("\"{s}\""));
            let back: RelationType = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            assert_eq!(back, t);
        }
    }

    #[test]
    fn relation_type_from_str_rejects_unknown() {
        for bad in ["cites", "REFERENCES", "", "related-to"] {
            let err = bad.parse::<RelationType>().unwrap_err();
            assert!(
                err.to_string().contains("unknown relation type"),
                "got: {err}"
            );
        }
    }

    #[test]
    fn relation_id_validation() {
        assert!(RelationId::new("rel-00000001").is_ok());
        assert!(RelationId::new("a").is_ok());
        assert!(RelationId::new(&"x".repeat(128)).is_ok());
        for bad in ["", &"x".repeat(129), "has space", "dot.name", "uniçode"] {
            assert!(RelationId::new(bad).is_err(), "'{bad}' must be rejected");
        }
    }

    #[test]
    fn relation_new_rejects_empty_endpoints_and_self_loops() {
        let id = RelationId::new("r1").unwrap();
        let meta = Metadata::new();
        assert!(
            Relation::new(
                id.clone(),
                RelationType::References,
                String::new(),
                "t".to_string(),
                meta.clone(),
                0
            )
            .is_err()
        );
        assert!(
            Relation::new(
                id.clone(),
                RelationType::References,
                "s".to_string(),
                String::new(),
                meta.clone(),
                0
            )
            .is_err()
        );
        let err = Relation::new(
            id.clone(),
            RelationType::References,
            "same".to_string(),
            "same".to_string(),
            meta.clone(),
            0,
        )
        .unwrap_err();
        assert!(err.to_string().contains("must differ"), "got: {err}");
    }

    #[test]
    fn relation_new_stores_fields_and_serde_round_trips() {
        let mut meta = Metadata::new();
        meta.insert(
            "created_by".to_string(),
            MetadataValue::Str("roger".to_string()),
        );
        let rel = Relation::new(
            RelationId::new("r1").unwrap(),
            RelationType::Supersedes,
            "doc-a".to_string(),
            "doc-b".to_string(),
            meta,
            1_790_000_000,
        )
        .unwrap();
        assert_eq!(rel.relation_type, RelationType::Supersedes);
        assert_eq!(rel.source, "doc-a");
        assert_eq!(rel.target, "doc-b");
        let json = serde_json::to_string(&rel).unwrap();
        let back: Relation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rel);
    }
}
