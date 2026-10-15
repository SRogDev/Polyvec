//! Relation storage: typed edges between documents/chunks (Phase 6).
//!
//! [`CollectionStore`] methods for relation CRUD plus the cascade hook.
//! Relations live in the redb catalog (see [`crate::catalog`]): one
//! `relations` table plus `rel_by_source` / `rel_by_target` secondary
//! indexes, all maintained in the same write transaction.
//!
//! Consistency rule: **referential integrity**. A relation's endpoints
//! must name existing documents or chunks at creation time; deleting a
//! document removes every relation touching it or its chunks, in both
//! directions. There are no dangling relations by construction — no
//! background repair needed.

use std::time::{SystemTime, UNIX_EPOCH};

use polyvec_core::{Error, Metadata, Relation, RelationId, RelationType, Result};

use crate::CollectionStore;

/// Filter + pagination for [`CollectionStore::list_relations`].
#[derive(Clone, Debug)]
pub struct RelationQuery {
    /// Only relations with this source endpoint.
    pub source: Option<String>,
    /// Only relations with this target endpoint.
    pub target: Option<String>,
    /// Only relations of this type.
    pub relation_type: Option<RelationType>,
    /// Max relations returned (after `offset`).
    pub limit: usize,
    /// Relations skipped before `limit` applies.
    pub offset: usize,
}

impl RelationQuery {
    /// Default query: everything, first 100, id order.
    #[must_use]
    pub fn all() -> Self {
        Self {
            source: None,
            target: None,
            relation_type: None,
            limit: 100,
            offset: 0,
        }
    }
}

impl Default for RelationQuery {
    fn default() -> Self {
        Self::all()
    }
}

/// Unix seconds; 0 when the clock is unavailable (never in practice).
fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl CollectionStore {
    // ------------------------------------------------------------------
    // relation CRUD
    // ------------------------------------------------------------------

    /// Creates a relation between two existing documents/chunks.
    ///
    /// Referential integrity: both endpoints must name an existing
    /// document or chunk ([`Error::NotFound`] otherwise). `id` is
    /// caller-chosen when `Some` ([`Error::AlreadyExists`] on collision);
    /// when `None` the store allocates `rel-{n:08}` from a durable
    /// counter. `created_at` is stamped by the store.
    ///
    /// # Errors
    /// [`Error::NotFound`] for unknown endpoints, [`Error::AlreadyExists`]
    /// for a taken id, [`Error::Validation`] for a self-loop (enforced by
    /// [`Relation::new`]), [`Error::Storage`] on redb failures.
    pub fn create_relation(
        &mut self,
        relation_type: RelationType,
        source: &str,
        target: &str,
        metadata: Metadata,
        id: Option<RelationId>,
    ) -> Result<Relation> {
        for endpoint in [source, target] {
            if !self.catalog.endpoint_exists(endpoint)? {
                return Err(Error::NotFound {
                    what: format!("relation endpoint '{endpoint}'"),
                });
            }
        }
        let id = match id {
            Some(id) => {
                if self.catalog.relation_get(&id)?.is_some() {
                    return Err(Error::AlreadyExists {
                        what: format!("relation '{id}'"),
                    });
                }
                id
            }
            None => RelationId::new(&format!("rel-{:08}", self.catalog.next_relation_id()?))?,
        };
        let rel = Relation::new(
            id,
            relation_type,
            source.to_string(),
            target.to_string(),
            metadata,
            now_unix_secs(),
        )?;
        self.catalog.relation_put(&rel)?;
        Ok(rel)
    }

    /// Reads a relation by id (`None` when unknown).
    ///
    /// # Errors
    /// [`Error::Storage`] on redb failures.
    pub fn get_relation(&self, id: &RelationId) -> Result<Option<Relation>> {
        self.catalog.relation_get(id)
    }

    /// Lists relations with optional endpoint/type filters and pagination.
    ///
    /// Results are in relation-id order (deterministic across pages).
    ///
    /// # Errors
    /// [`Error::Storage`] on redb failures.
    pub fn list_relations(&self, query: &RelationQuery) -> Result<Vec<Relation>> {
        let mut out: Vec<Relation> = self
            .catalog
            .relations_all()?
            .into_iter()
            .filter(|rel| {
                query.source.as_ref().is_none_or(|s| rel.source == *s)
                    && query.target.as_ref().is_none_or(|t| rel.target == *t)
                    && query.relation_type.is_none_or(|t| rel.relation_type == t)
            })
            .collect();
        // relations_all is already id-ordered; the filter preserves it.
        let start = query.offset.min(out.len());
        out.drain(..start);
        out.truncate(query.limit);
        Ok(out)
    }

    /// Deletes a relation. Idempotent: returns `false` when unknown.
    ///
    /// # Errors
    /// [`Error::Storage`] on redb failures.
    pub fn delete_relation(&mut self, id: &RelationId) -> Result<bool> {
        self.catalog.relation_remove(id)
    }

    /// Every relation touching `endpoint`, outgoing and incoming
    /// (deduped by id, in id order). Used by traversal and cascade.
    ///
    /// # Errors
    /// [`Error::Storage`] on redb failures.
    pub fn relations_incident(&self, endpoint: &str) -> Result<Vec<Relation>> {
        let mut out = self.catalog.relations_by_source(endpoint)?;
        for rel in self.catalog.relations_by_target(endpoint)? {
            if !out.iter().any(|r| r.id == rel.id) {
                out.push(rel);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Removes every relation touching any of `endpoints` (both
    /// directions). Returns the number removed.
    ///
    /// Called by [`CollectionStore::delete_document`]; also the primitive
    /// a future chunk-level delete would use.
    ///
    /// # Errors
    /// [`Error::Storage`] on redb failures.
    pub(crate) fn remove_relations_of(&mut self, endpoints: &[&str]) -> Result<usize> {
        let mut removed = 0;
        for endpoint in endpoints {
            for rel in self.relations_incident(endpoint)? {
                if self.catalog.relation_remove(&rel.id)? {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::Catalog;

    // The integration tests in tests/relations.rs cover the public API
    // end to end; this module tests the migration path that needs
    // crate-internal access: a pre-Phase-6 catalog (no relation tables)
    // must open and gain working relation tables.

    #[test]
    fn catalog_open_migrates_pre_phase6_catalog() {
        use redb::{Database, TableDefinition};

        let dir = std::env::temp_dir().join(format!("polyvec-rel-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Build a "Phase 5 era" catalog by hand: the old tables only, no
        // relation tables, format version still 1.
        {
            let db = Database::create(dir.join("catalog.redb")).unwrap();
            let txn = db.begin_write().unwrap();
            for name in ["collections", "documents", "chunks", "vectors", "meta"] {
                let _ = txn
                    .open_table(TableDefinition::<&str, &str>::new(name))
                    .unwrap();
            }
            {
                let mut meta = txn
                    .open_table(TableDefinition::<&str, &str>::new("meta"))
                    .unwrap();
                meta.insert("format_version", "1").unwrap();
                meta.insert("generation", "0").unwrap();
                meta.insert("synced_generation", "0").unwrap();
                meta.insert("next_node_id", "0").unwrap();
            }
            txn.commit().unwrap();
        }

        // Opening must not fail, and the relation tables must work.
        let catalog = Catalog::open(&dir).unwrap();
        assert!(!catalog.endpoint_exists("whatever").unwrap());
        let n = catalog.next_relation_id().unwrap();
        assert_eq!(n, 1, "missing counter starts at 1");
    }
}
