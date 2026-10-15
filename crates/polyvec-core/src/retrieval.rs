//! Retrieval contracts: query modes, metadata filters, and the shared
//! result vocabulary (Phase 5).
//!
//! These types are pure domain vocabulary — no tantivy, no HTTP, no async.
//! Score semantics are documented per mode in `docs/retrieval.md`; the one
//! rule that matters here: **scores are never comparable across modes** (a
//! BM25 score and an RRF score live in different universes — only ranks
//! within one response are meaningful).

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Metadata, MetadataValue, RelationType, Result};

/// Default traversal depth for `expand` (hops from a base hit).
pub const DEFAULT_EXPAND_DEPTH: u8 = 3;
/// Hard ceiling for traversal depth (per query).
pub const MAX_EXPAND_DEPTH: u8 = 5;
/// Default cap on expanded nodes per query.
pub const DEFAULT_EXPAND_MAX_NODES: usize = 100;
/// Hard ceiling for expanded nodes per query.
pub const MAX_EXPAND_MAX_NODES: usize = 1000;

/// Relationship-expansion config for the query endpoint.
///
/// Runs the base retrieval first, then a bounded BFS over the supplied
/// relations seeded from the base hits' chunk/document ids, then appends
/// the reached nodes as extra hits with path evidence (see
/// `docs/retrieval.md`). Absent by default — Phase 5 queries behave
/// exactly as before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandConfig {
    /// How many relation hops to walk from each base hit (1–5, default 3).
    pub depth: u8,
    /// Cap on expanded nodes for the whole query (1–1000, default 100).
    pub max_nodes: usize,
    /// Only walk edges of these types; `None` walks every type.
    pub relation_types: Option<Vec<RelationType>>,
}

impl ExpandConfig {
    /// Builds a config, applying defaults and rejecting absurd values.
    ///
    /// # Errors
    /// [`Error::Validation`] when `depth` is outside `1..=5` or
    /// `max_nodes` is outside `1..=1000`.
    pub fn new(
        depth: Option<u8>,
        max_nodes: Option<usize>,
        relation_types: Option<Vec<RelationType>>,
    ) -> Result<Self> {
        let depth = depth.unwrap_or(DEFAULT_EXPAND_DEPTH);
        if !(1..=MAX_EXPAND_DEPTH).contains(&depth) {
            return Err(Error::Validation {
                detail: format!("expand.depth must be within 1..={MAX_EXPAND_DEPTH}, got {depth}"),
            });
        }
        let max_nodes = max_nodes.unwrap_or(DEFAULT_EXPAND_MAX_NODES);
        if !(1..=MAX_EXPAND_MAX_NODES).contains(&max_nodes) {
            return Err(Error::Validation {
                detail: format!(
                    "expand.max_nodes must be within 1..={MAX_EXPAND_MAX_NODES}, got {max_nodes}"
                ),
            });
        }
        Ok(Self {
            depth,
            max_nodes,
            relation_types,
        })
    }

    /// Whether an edge of this type is walkable under the config.
    #[must_use]
    pub fn allows(&self, relation_type: RelationType) -> bool {
        self.relation_types
            .as_ref()
            .is_none_or(|types| types.contains(&relation_type))
    }
}

/// Which retrieval subsystem(s) answer a query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalMode {
    /// ANN vector search over embeddings. This is the Phase 4 behavior and
    /// stays the default.
    #[default]
    Vector,
    /// BM25 lexical search over chunk text (tantivy). Needs no embedding
    /// provider — exact-term queries work with the engine alone.
    Lexical,
    /// Reciprocal Rank Fusion of the vector and lexical rankings.
    Hybrid,
}

impl FromStr for RetrievalMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "vector" => Ok(Self::Vector),
            "lexical" => Ok(Self::Lexical),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(Error::Validation {
                detail: format!(
                    "unknown retrieval mode '{other}'; expected 'vector', 'lexical', or 'hybrid'"
                ),
            }),
        }
    }
}

/// What a `score` number means. Scores are only comparable *within* one
/// kind, never across kinds or across queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScoreKind {
    /// Raw metric distance (L2 / cosine distance). **Lower is better.**
    Distance,
    /// tantivy BM25 score. **Higher is better.** Scale is query-dependent.
    Bm25,
    /// Reciprocal Rank Fusion score, Σ 1/(k + rank). **Higher is better.**
    /// Not comparable across queries (depends on the fused lists).
    Rrf,
    /// Relationship-expansion score: the number of relation hops from the
    /// nearest base hit. **Lower is better** (fewer hops = closer).
    /// Only appears on hits added by `expand`.
    Relationship,
}

/// Which subsystem produced a hit — the retrieval-path explanation carried
/// on every result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalPath {
    /// The ANN vector index.
    Vector,
    /// The tantivy lexical index.
    Lexical,
    /// The relation subsystem (bounded traversal over supplied relations).
    /// Only appears on hits added by `expand`.
    Relationship,
}

/// Tuning for Reciprocal Rank Fusion.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RrfConfig {
    /// The RRF `k` constant: `score = Σ 1/(k + rank)` over the fused
    /// rankings (ranks are 1-based). Default 60 (the literature default).
    pub k: f64,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self { k: 60.0 }
    }
}

impl RrfConfig {
    /// Builds a config, rejecting non-finite or absurd `k`.
    ///
    /// # Errors
    /// [`Error::Validation`] when `k` is not in `1..=10_000`.
    pub fn new(k: f64) -> Result<Self> {
        if !k.is_finite() || !(1.0..=10_000.0).contains(&k) {
            return Err(Error::Validation {
                detail: format!("rrf_k must be within 1..=10000, got {k}"),
            });
        }
        Ok(Self { k })
    }
}

/// Metadata filter: exact-equality match on document metadata.
///
/// Serializes as a plain JSON object (`{"author": "roger", "pages": 3}`) so
/// it can be embedded directly in a query body: JSON strings map to
/// [`MetadataValue::Str`], integers to `Int`, non-integers to `Float`,
/// booleans to `Bool`; anything else (null, arrays, objects) is rejected.
/// The same filter value is honored by the vector, lexical, and hybrid
/// paths (see `docs/retrieval.md` for how each path applies it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetadataFilter(pub BTreeMap<String, MetadataValue>);

impl Serialize for MetadataFilter {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            match value {
                MetadataValue::Str(s) => map.serialize_entry(key, s)?,
                MetadataValue::Int(i) => map.serialize_entry(key, i)?,
                MetadataValue::Float(f) => map.serialize_entry(key, f)?,
                MetadataValue::Bool(b) => map.serialize_entry(key, b)?,
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for MetadataFilter {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let raw = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let mut out = BTreeMap::new();
        for (key, value) in raw {
            let mv = match value {
                serde_json::Value::String(s) => MetadataValue::Str(s),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        MetadataValue::Int(i)
                    } else if let Some(f) = n.as_f64() {
                        MetadataValue::Float(f)
                    } else {
                        return Err(D::Error::custom(format!(
                            "filter value for '{key}' is not a valid number"
                        )));
                    }
                }
                serde_json::Value::Bool(b) => MetadataValue::Bool(b),
                other => {
                    return Err(D::Error::custom(format!(
                        "filter value for '{key}' must be a string, number, or boolean, got {other}"
                    )));
                }
            };
            out.insert(key, mv);
        }
        Ok(Self(out))
    }
}

impl MetadataFilter {
    /// Whether the filter matches everything (no constraints).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Exact-equality semantics: every entry must be present in `metadata`
    /// with an equal value. A missing key never matches; floats compare
    /// exactly (no epsilon — filters are identity checks, not similarity).
    #[must_use]
    pub fn matches(&self, metadata: &Metadata) -> bool {
        self.0
            .iter()
            .all(|(key, want)| metadata.get(key) == Some(want))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_default_is_vector_phase4_behavior() {
        assert_eq!(RetrievalMode::default(), RetrievalMode::Vector);
    }

    #[test]
    fn mode_serde_round_trip() {
        for mode in [
            RetrievalMode::Vector,
            RetrievalMode::Lexical,
            RetrievalMode::Hybrid,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: RetrievalMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
        }
        assert_eq!(
            serde_json::to_string(&RetrievalMode::Hybrid).unwrap(),
            "\"hybrid\""
        );
    }

    #[test]
    fn mode_from_str_rejects_unknown() {
        assert_eq!(
            "lexical".parse::<RetrievalMode>().unwrap(),
            RetrievalMode::Lexical
        );
        let err = "semantic".parse::<RetrievalMode>().unwrap_err();
        assert!(
            err.to_string().contains("unknown retrieval mode"),
            "got: {err}"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn rrf_config_default_is_60() {
        assert_eq!(RrfConfig::default().k, 60.0);
        assert_eq!(RrfConfig::new(60.0).unwrap().k, 60.0);
    }

    #[test]
    fn rrf_config_rejects_absurd_k() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 10_001.0] {
            assert!(RrfConfig::new(bad).is_err(), "k={bad} must be rejected");
        }
    }

    fn sample_metadata() -> Metadata {
        let mut m = Metadata::new();
        m.insert(
            "author".to_string(),
            MetadataValue::Str("roger".to_string()),
        );
        m.insert("pages".to_string(), MetadataValue::Int(3));
        m.insert("ratio".to_string(), MetadataValue::Float(0.5));
        m.insert("draft".to_string(), MetadataValue::Bool(false));
        m
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = MetadataFilter::default();
        assert!(f.is_empty());
        assert!(f.matches(&sample_metadata()));
        assert!(f.matches(&Metadata::new()));
    }

    #[test]
    fn filter_matches_all_value_types() {
        let mut map = BTreeMap::new();
        map.insert(
            "author".to_string(),
            MetadataValue::Str("roger".to_string()),
        );
        map.insert("pages".to_string(), MetadataValue::Int(3));
        map.insert("ratio".to_string(), MetadataValue::Float(0.5));
        map.insert("draft".to_string(), MetadataValue::Bool(false));
        let f = MetadataFilter(map);
        assert!(f.matches(&sample_metadata()));
    }

    #[test]
    fn filter_rejects_missing_key_and_wrong_value_and_wrong_type() {
        let meta = sample_metadata();
        let one = |k: &str, v: MetadataValue| {
            let mut map = BTreeMap::new();
            map.insert(k.to_string(), v);
            MetadataFilter(map)
        };
        // Missing key.
        assert!(!one("nope", MetadataValue::Str("x".to_string())).matches(&meta));
        // Wrong value.
        assert!(!one("author", MetadataValue::Str("ana".to_string())).matches(&meta));
        // Wrong type (3 != 3.0 as metadata values are a closed set).
        assert!(!one("pages", MetadataValue::Float(3.0)).matches(&meta));
        // Int vs float on the float field.
        assert!(!one("ratio", MetadataValue::Int(0)).matches(&meta));
    }

    #[test]
    fn filter_serde_is_a_plain_object() {
        let mut map = BTreeMap::new();
        map.insert(
            "author".to_string(),
            MetadataValue::Str("roger".to_string()),
        );
        let json = serde_json::to_string(&MetadataFilter(map)).unwrap();
        assert_eq!(json, r#"{"author":"roger"}"#);
    }

    #[test]
    fn score_kind_and_path_serde() {
        assert_eq!(serde_json::to_string(&ScoreKind::Rrf).unwrap(), "\"rrf\"");
        assert_eq!(
            serde_json::to_string(&RetrievalPath::Lexical).unwrap(),
            "\"lexical\""
        );
        assert_eq!(
            serde_json::to_string(&ScoreKind::Relationship).unwrap(),
            "\"relationship\""
        );
        assert_eq!(
            serde_json::to_string(&RetrievalPath::Relationship).unwrap(),
            "\"relationship\""
        );
        assert_eq!(
            "relationship"
                .parse::<RetrievalMode>()
                .unwrap_err()
                .to_string(),
            "invalid value: unknown retrieval mode 'relationship'; expected 'vector', 'lexical', or 'hybrid'"
        );
    }

    #[test]
    fn expand_config_defaults() {
        let cfg = ExpandConfig::new(None, None, None).unwrap();
        assert_eq!(cfg.depth, 3);
        assert_eq!(cfg.max_nodes, 100);
        assert!(cfg.relation_types.is_none());
        assert!(cfg.allows(RelationType::References));
    }

    #[test]
    fn expand_config_rejects_absurd_values() {
        assert!(ExpandConfig::new(Some(0), None, None).is_err());
        assert!(ExpandConfig::new(Some(6), None, None).is_err());
        assert!(ExpandConfig::new(None, Some(0), None).is_err());
        assert!(ExpandConfig::new(None, Some(1001), None).is_err());
        assert!(ExpandConfig::new(Some(1), Some(1), None).is_ok());
        assert!(ExpandConfig::new(Some(5), Some(1000), None).is_ok());
    }

    #[test]
    fn expand_config_type_filter() {
        let cfg = ExpandConfig::new(None, None, Some(vec![RelationType::References])).unwrap();
        assert!(cfg.allows(RelationType::References));
        assert!(!cfg.allows(RelationType::Supersedes));
    }
}
