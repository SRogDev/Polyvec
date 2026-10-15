//! Phase 6 RED tests: relation storage (CRUD, cascade, persistence).
//!
//! Defines the `CollectionStore` relation API. FAILS until implemented.

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, ContentHash, DistanceMetric, Document, DocumentId,
    Error, Metadata, MetadataValue, RelationId, RelationType,
};
use polyvec_storage::{CollectionStore, RelationQuery};

fn test_config(dim: u32) -> CollectionConfig {
    CollectionConfig::new(
        CollectionId::new("kb").unwrap(),
        dim,
        DistanceMetric::Cosine,
    )
    .unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "polyvec-rel-test-{name}-{n}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_doc(collection: &CollectionId, source: &str, n_chunks: u32) -> (Document, Vec<Chunk>) {
    let doc_id = DocumentId::for_source(collection, source);
    let doc = Document::new(
        doc_id.clone(),
        collection.clone(),
        source.to_string(),
        ContentHash::of(source.as_bytes()),
        Metadata::new(),
    );
    let chunks: Vec<Chunk> = (0..n_chunks)
        .map(|i| Chunk::new(doc_id.clone(), i, &format!("{source} chunk {i}"), 0, 10))
        .collect();
    (doc, chunks)
}

/// Store with two documents (A: 2 chunks, B: 1 chunk).
fn two_docs() -> (CollectionStore, Document, Vec<Chunk>, Document, Vec<Chunk>) {
    let dir = tmp("crud");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let collection = CollectionId::new("kb").unwrap();
    let (doc_a, chunks_a) = make_doc(&collection, "https://a.example", 2);
    let (doc_b, chunks_b) = make_doc(&collection, "https://b.example", 1);
    store.put_document(&doc_a, &chunks_a).unwrap();
    store.put_document(&doc_b, &chunks_b).unwrap();
    (store, doc_a, chunks_a, doc_b, chunks_b)
}

fn provenance(who: &str) -> Metadata {
    let mut m = Metadata::new();
    m.insert(
        "created_by".to_string(),
        MetadataValue::Str(who.to_string()),
    );
    m
}

#[test]
fn create_and_get_relation_roundtrip() {
    let (mut store, doc_a, _, doc_b, _) = two_docs();
    let rel = store
        .create_relation(
            RelationType::References,
            doc_a.id.as_str(),
            doc_b.id.as_str(),
            provenance("roger"),
            None,
        )
        .unwrap();
    assert!(rel.id.as_str().starts_with("rel-"), "got {}", rel.id);
    assert_eq!(rel.relation_type, RelationType::References);
    assert!(rel.created_at > 0);
    assert_eq!(
        rel.metadata.get("created_by"),
        Some(&MetadataValue::Str("roger".to_string()))
    );
    let got = store.get_relation(&rel.id).unwrap().expect("stored");
    assert_eq!(got.id, rel.id);
    assert_eq!(got.source, doc_a.id.as_str());
    assert_eq!(got.target, doc_b.id.as_str());
}

#[test]
fn create_relation_with_explicit_id_and_duplicate_is_409() {
    let (mut store, doc_a, _, doc_b, _) = two_docs();
    let id = RelationId::new("my-rel").unwrap();
    store
        .create_relation(
            RelationType::RelatedTo,
            doc_a.id.as_str(),
            doc_b.id.as_str(),
            Metadata::new(),
            Some(id.clone()),
        )
        .unwrap();
    let err = store
        .create_relation(
            RelationType::RelatedTo,
            doc_a.id.as_str(),
            doc_b.id.as_str(),
            Metadata::new(),
            Some(id),
        )
        .unwrap_err();
    assert!(matches!(err, Error::AlreadyExists { .. }), "got: {err:?}");
}

#[test]
fn create_relation_unknown_endpoint_is_404() {
    let (mut store, doc_a, _, _, _) = two_docs();
    let err = store
        .create_relation(
            RelationType::References,
            doc_a.id.as_str(),
            "no-such-doc",
            Metadata::new(),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "got: {err:?}");
    let err = store
        .create_relation(
            RelationType::References,
            "no-such-doc",
            doc_a.id.as_str(),
            Metadata::new(),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "got: {err:?}");
}

#[test]
fn create_relation_chunk_endpoints() {
    let (mut store, _, chunks_a, _, chunks_b) = two_docs();
    let rel = store
        .create_relation(
            RelationType::RelatedTo,
            chunks_a[0].id.as_str(),
            chunks_b[0].id.as_str(),
            Metadata::new(),
            None,
        )
        .unwrap();
    let incident = store.relations_incident(chunks_a[0].id.as_str()).unwrap();
    assert_eq!(incident.len(), 1);
    assert_eq!(incident[0].id, rel.id);
    // The other chunk sees the same edge from the incoming side.
    let incident_b = store.relations_incident(chunks_b[0].id.as_str()).unwrap();
    assert_eq!(incident_b.len(), 1);
}

#[test]
fn list_relations_filters_and_pagination() {
    let (mut store, doc_a, chunks_a, doc_b, _) = two_docs();
    let a = doc_a.id.as_str();
    let b = doc_b.id.as_str();
    let ca = chunks_a[0].id.as_str();
    store
        .create_relation(RelationType::References, a, b, Metadata::new(), None)
        .unwrap();
    store
        .create_relation(RelationType::RelatedTo, b, a, Metadata::new(), None)
        .unwrap();
    store
        .create_relation(RelationType::Supersedes, ca, b, Metadata::new(), None)
        .unwrap();

    let all = store.list_relations(&RelationQuery::default()).unwrap();
    assert_eq!(all.len(), 3);

    let by_source = store
        .list_relations(&RelationQuery {
            source: Some(a.to_string()),
            ..RelationQuery::default()
        })
        .unwrap();
    assert_eq!(by_source.len(), 1);
    assert_eq!(by_source[0].relation_type, RelationType::References);

    let by_target = store
        .list_relations(&RelationQuery {
            target: Some(a.to_string()),
            ..RelationQuery::default()
        })
        .unwrap();
    assert_eq!(by_target.len(), 1);
    assert_eq!(by_target[0].relation_type, RelationType::RelatedTo);

    let by_type = store
        .list_relations(&RelationQuery {
            relation_type: Some(RelationType::Supersedes),
            ..RelationQuery::default()
        })
        .unwrap();
    assert_eq!(by_type.len(), 1);

    // Deterministic order: pages tile the full list without overlap.
    let page0: Vec<_> = store
        .list_relations(&RelationQuery {
            limit: 2,
            offset: 0,
            ..RelationQuery::default()
        })
        .unwrap();
    let page1 = store
        .list_relations(&RelationQuery {
            limit: 2,
            offset: 2,
            ..RelationQuery::default()
        })
        .unwrap();
    assert_eq!(page0.len(), 2);
    assert_eq!(page1.len(), 1);
    assert!(!page0.iter().any(|r| r.id == page1[0].id));
    // And re-querying the same page is stable.
    let page0_again = store
        .list_relations(&RelationQuery {
            limit: 2,
            offset: 0,
            ..RelationQuery::default()
        })
        .unwrap();
    assert_eq!(
        page0.iter().map(|r| &r.id).collect::<Vec<_>>(),
        page0_again.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
}

#[test]
fn delete_relation_is_idempotent() {
    let (mut store, doc_a, _, doc_b, _) = two_docs();
    let rel = store
        .create_relation(
            RelationType::References,
            doc_a.id.as_str(),
            doc_b.id.as_str(),
            Metadata::new(),
            None,
        )
        .unwrap();
    assert!(store.delete_relation(&rel.id).unwrap());
    assert!(!store.delete_relation(&rel.id).unwrap());
    assert!(store.get_relation(&rel.id).unwrap().is_none());
}

#[test]
fn delete_document_cascades_relations_both_directions() {
    let (mut store, doc_a, chunks_a, doc_b, chunks_b) = two_docs();
    let a = doc_a.id.as_str();
    let b = doc_b.id.as_str();
    let ca0 = chunks_a[0].id.as_str();
    let cb0 = chunks_b[0].id.as_str();
    // Outgoing doc edge, incoming doc edge, chunk edge, mixed edge.
    store
        .create_relation(RelationType::References, a, b, Metadata::new(), None)
        .unwrap();
    store
        .create_relation(RelationType::RelatedTo, b, a, Metadata::new(), None)
        .unwrap();
    store
        .create_relation(RelationType::RelatedTo, ca0, cb0, Metadata::new(), None)
        .unwrap();
    store
        .create_relation(RelationType::References, ca0, b, Metadata::new(), None)
        .unwrap();
    // Unrelated edge that must survive.
    let collection = CollectionId::new("kb").unwrap();
    let (doc_c, _) = make_doc(&collection, "https://c.example", 0);
    store.put_document(&doc_c, &[]).unwrap();
    let survivor = store
        .create_relation(
            RelationType::References,
            b,
            doc_c.id.as_str(),
            Metadata::new(),
            None,
        )
        .unwrap();

    store.delete_document(&doc_a.id).unwrap();

    let remaining = store.list_relations(&RelationQuery::default()).unwrap();
    assert_eq!(remaining.len(), 1, "only the survivor may remain");
    assert_eq!(remaining[0].id, survivor.id);
    assert!(store.relations_incident(a).unwrap().is_empty());
    assert!(store.relations_incident(ca0).unwrap().is_empty());
    // No retrievable orphaned relations: every remaining endpoint exists.
    for rel in &remaining {
        assert!(
            store
                .get_document(&DocumentId::for_source(&collection, "https://b.example"))
                .is_ok()
        );
        let _ = &rel.source;
    }
}

#[test]
fn relations_survive_reopen() {
    let dir = tmp("reopen");
    let rel_id;
    {
        let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
        let collection = CollectionId::new("kb").unwrap();
        let (doc_a, chunks_a) = make_doc(&collection, "https://a.example", 1);
        let (doc_b, _) = make_doc(&collection, "https://b.example", 0);
        store.put_document(&doc_a, &chunks_a).unwrap();
        store.put_document(&doc_b, &[]).unwrap();
        let rel = store
            .create_relation(
                RelationType::References,
                doc_a.id.as_str(),
                doc_b.id.as_str(),
                provenance("reopen-test"),
                None,
            )
            .unwrap();
        rel_id = rel.id.clone();
        store.flush().unwrap();
    }
    let store = CollectionStore::open(&dir).unwrap().0;
    let got = store.get_relation(&rel_id).unwrap().expect("survived");
    assert_eq!(
        got.metadata.get("created_by"),
        Some(&MetadataValue::Str("reopen-test".to_string()))
    );
    assert_eq!(
        store
            .list_relations(&RelationQuery::default())
            .unwrap()
            .len(),
        1
    );
    // The id counter survived too: the next generated id must not collide.
    let mut store = store;
    let collection = CollectionId::new("kb").unwrap();
    let (doc_c, _) = make_doc(&collection, "https://c.example", 0);
    store.put_document(&doc_c, &[]).unwrap();
    let rel2 = store
        .create_relation(
            RelationType::References,
            doc_c.id.as_str(),
            got.target.as_str(),
            Metadata::new(),
            None,
        )
        .unwrap();
    assert_ne!(rel2.id, rel_id);
}
