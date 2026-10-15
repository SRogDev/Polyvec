//! Phase 2 RED tests: durable storage layer.
//!
//! Defines the `CollectionStore` API: lifecycle CRUD, restart recovery,
//! interrupted-write recovery, corrupt-dump errors, tombstone rebuild.
//! FAILS until implemented.

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, ContentHash, DistanceMetric, Document, DocumentId,
    Error, IndexState, Metadata, SyncStatus, Vector, VectorId,
};
use polyvec_storage::{CollectionStore, Recovery};

fn test_config(dim: u32) -> CollectionConfig {
    CollectionConfig::new(
        CollectionId::new("kb").unwrap(),
        dim,
        DistanceMetric::Cosine,
    )
    .unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("polyvec-store-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[allow(clippy::cast_precision_loss)] // test RNG: float conversion is the recipe
fn unit_vec(seed: u64, dim: usize) -> Vec<f32> {
    // deterministic pseudo-random unit vector from a seed
    let mut x = seed;
    let mut v = Vec::with_capacity(dim);
    for _ in 0..dim {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        v.push((x >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0);
    }
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt().max(1e-12);
    for a in &mut v {
        *a /= n;
    }
    v
}

fn vid(n: u64) -> VectorId {
    VectorId::parse(&format!("vec-{n:06}")).unwrap()
}

#[test]
fn lifecycle_insert_search_update_delete() {
    let dir = tmp("lifecycle");
    let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();

    // insert
    for i in 0..50u64 {
        store
            .insert_vector(Vector::new(vid(i), unit_vec(i, 16), 16).unwrap())
            .unwrap();
    }
    assert_eq!(store.stats().live_vectors, 50);

    // search finds an inserted vector at rank 1
    let q = unit_vec(7, 16);
    let hits = store.search(&q, 5, 64).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].vector_id, vid(7));
    assert!(hits[0].distance < 1e-4);

    // update: same VectorId, new values
    let new_vals = unit_vec(999, 16);
    store
        .update_vector(Vector::new(vid(7), new_vals.clone(), 16).unwrap())
        .unwrap();
    let hits = store.search(&new_vals, 5, 64).unwrap();
    assert_eq!(hits[0].vector_id, vid(7));
    // old values no longer match it at rank 1
    let hits = store.search(&q, 5, 64).unwrap();
    assert_ne!(hits[0].vector_id, vid(7));

    // get_vector reflects the update
    let rec = store.get_vector(&vid(7)).unwrap();
    assert_eq!(rec.values, new_vals);

    // delete hides immediately (no flush needed). Note: the earlier update
    // left one tombstone behind as well.
    store.delete_vector(&vid(3)).unwrap();
    let hits = store.search(&unit_vec(3, 16), 5, 64).unwrap();
    assert!(hits.iter().all(|h| h.vector_id != vid(3)));
    assert_eq!(store.stats().tombstoned, 2);

    // update/delete of missing -> NotFound
    assert!(matches!(
        store.update_vector(Vector::new(vid(4242), unit_vec(1, 16), 16).unwrap()),
        Err(Error::NotFound { .. })
    ));
    assert!(matches!(
        store.delete_vector(&vid(4242)),
        Err(Error::NotFound { .. })
    ));

    // dimension mismatch -> Validation
    assert!(matches!(
        store.insert_vector(Vector::new(vid(100), unit_vec(1, 8), 8).unwrap()),
        Err(Error::Validation { .. })
    ));
}

#[test]
fn restart_recovery_is_identical() {
    let dir = tmp("restart");
    let before: Vec<(VectorId, f32)>;
    {
        let mut store = CollectionStore::create(&dir, test_config(32)).unwrap();
        for i in 0..200u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i * 31 + 7, 32), 32).unwrap())
                .unwrap();
        }
        store.delete_vector(&vid(11)).unwrap();
        store.flush().unwrap();
        assert!(matches!(store.index_state().sync, SyncStatus::Clean));
        let q = unit_vec(12345, 32);
        before = store
            .search(&q, 10, 100)
            .unwrap()
            .into_iter()
            .map(|h| (h.vector_id, h.distance))
            .collect();
        // store dropped here: close
    }
    {
        let (store, recovery) = CollectionStore::open(&dir).unwrap();
        assert!(
            matches!(recovery, Recovery::Clean),
            "expected clean recovery, got {recovery:?}"
        );
        let q = unit_vec(12345, 32);
        let after: Vec<(VectorId, f32)> = store
            .search(&q, 10, 100)
            .unwrap()
            .into_iter()
            .map(|h| (h.vector_id, h.distance))
            .collect();
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(after.iter()) {
            assert_eq!(a.0, b.0, "vector ids must survive restart");
            assert!((a.1 - b.1).abs() < 1e-6, "distances must survive restart");
        }
        // tombstone survived too
        assert_eq!(store.stats().tombstoned, 1);
    }
}

#[test]
fn interrupted_write_recovers_from_catalog() {
    let dir = tmp("interrupted");
    let before: Vec<(VectorId, f32)>;
    {
        let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
        for i in 0..100u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i + 500, 16), 16).unwrap())
                .unwrap();
        }
        // NOTE: no flush — simulates a crash after the redb commit but
        // before the index dump. The dump on disk is stale/empty.
        let q = unit_vec(77, 16);
        before = store
            .search(&q, 10, 64)
            .unwrap()
            .into_iter()
            .map(|h| (h.vector_id, h.distance))
            .collect();
        // drop without flush
    }
    {
        let (store, recovery) = CollectionStore::open(&dir).unwrap();
        assert!(
            matches!(recovery, Recovery::RebuiltFromCatalog { .. }),
            "expected catalog rebuild, got {recovery:?}"
        );
        let q = unit_vec(77, 16);
        let after: Vec<(VectorId, f32)> = store
            .search(&q, 10, 64)
            .unwrap()
            .into_iter()
            .map(|h| (h.vector_id, h.distance))
            .collect();
        assert_eq!(
            before.len(),
            after.len(),
            "all writes must survive the crash"
        );
        for (a, b) in before.iter().zip(after.iter()) {
            assert_eq!(a.0, b.0);
            assert!((a.1 - b.1).abs() < 1e-5);
        }
        assert!(matches!(store.index_state().sync, SyncStatus::Clean));
    }
}

#[test]
fn corrupt_dump_is_a_clear_error_not_a_panic() {
    let dir = tmp("corrupt");
    {
        let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
        for i in 0..20u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i, 16), 16).unwrap())
                .unwrap();
        }
        store.flush().unwrap();
    }
    // Corrupt the graph dump on disk.
    let graph = dir.join("index.hnsw.graph");
    let mut bytes = std::fs::read(&graph).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    bytes[mid + 1] ^= 0xFF;
    std::fs::write(&graph, &bytes).unwrap();

    let result = CollectionStore::open(&dir);
    match result {
        Err(Error::Corrupt { detail }) => {
            assert!(
                detail.contains("checksum"),
                "error should name the failed check: {detail}"
            );
        }
        _ => panic!("expected Error::Corrupt"),
    }
}

#[test]
fn sidecar_version_mismatch_is_a_clear_error() {
    let dir = tmp("version");
    {
        let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
        store
            .insert_vector(Vector::new(vid(0), unit_vec(0, 16), 16).unwrap())
            .unwrap();
        store.flush().unwrap();
    }
    // Tamper with the sidecar version.
    let sidecar = dir.join("sidecar.json");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    json["format_version"] = serde_json::json!(999);
    std::fs::write(&sidecar, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    assert!(matches!(
        CollectionStore::open(&dir),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn missing_dump_files_rebuild_from_catalog() {
    let dir = tmp("missing");
    {
        let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
        for i in 0..30u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i * 3, 16), 16).unwrap())
                .unwrap();
        }
        store.flush().unwrap();
    }
    std::fs::remove_file(dir.join("index.hnsw.graph")).unwrap();
    std::fs::remove_file(dir.join("index.hnsw.data")).unwrap();
    std::fs::remove_file(dir.join("sidecar.json")).unwrap();

    let (store, recovery) = CollectionStore::open(&dir).unwrap();
    assert!(matches!(recovery, Recovery::RebuiltFromCatalog { .. }));
    assert_eq!(store.stats().live_vectors, 30);
    let hits = store.search(&unit_vec(9, 16), 5, 64).unwrap();
    assert_eq!(hits[0].vector_id, vid(3));
}

#[test]
fn tombstone_rebuild_compacts_and_preserves_results() {
    // Part 1: explicit rebuild below the auto-compact threshold.
    let dir = tmp("rebuild");
    let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
    for i in 0..200u64 {
        store
            .insert_vector(Vector::new(vid(i), unit_vec(i * 13, 16), 16).unwrap())
            .unwrap();
    }
    for i in (0..200u64).step_by(10) {
        store.delete_vector(&vid(i)).unwrap();
    }
    store.flush().unwrap();
    // 20/180 = 11% < 20% threshold: flush dumps without compacting.
    assert_eq!(store.stats().tombstoned, 20);

    let q = unit_vec(424_242, 16);
    let before: Vec<VectorId> = store
        .search(&q, 10, 128)
        .unwrap()
        .into_iter()
        .map(|h| h.vector_id)
        .collect();

    store.rebuild().unwrap();
    let st = store.stats();
    assert_eq!(st.tombstoned, 0, "rebuild must compact tombstones");
    assert_eq!(st.live_vectors, 180);

    let after: Vec<VectorId> = store
        .search(&q, 10, 128)
        .unwrap()
        .into_iter()
        .map(|h| h.vector_id)
        .collect();
    assert_eq!(before, after, "rebuild must preserve search results");
    assert!(
        after.iter().all(|id| {
            let n: u64 = id.to_string().trim_start_matches("vec-").parse().unwrap();
            n % 10 != 0
        }),
        "only live vectors may appear"
    );
}

#[test]
fn flush_auto_compacts_past_tombstone_ratio() {
    // Part 2: flush() compacts automatically past the tombstone ratio.
    let dir = tmp("autocompact");
    let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
    for i in 0..200u64 {
        store
            .insert_vector(Vector::new(vid(i), unit_vec(i * 13, 16), 16).unwrap())
            .unwrap();
    }
    for i in (0..200u64).step_by(2) {
        store.delete_vector(&vid(i)).unwrap();
    }
    let q = unit_vec(424_242, 16);
    let before: Vec<VectorId> = store
        .search(&q, 10, 128)
        .unwrap()
        .into_iter()
        .map(|h| h.vector_id)
        .collect();

    store.flush().unwrap(); // 100/100 = 100% > 20%: auto-compact
    assert_eq!(
        store.stats().tombstoned,
        0,
        "flush past the ratio must compact"
    );
    assert_eq!(store.stats().live_vectors, 100);

    let after: Vec<VectorId> = store
        .search(&q, 10, 128)
        .unwrap()
        .into_iter()
        .map(|h| h.vector_id)
        .collect();
    assert_eq!(before, after, "auto-compact must preserve search results");
}

#[test]
fn documents_chunks_and_cascade_delete() {
    let dir = tmp("docs");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();

    let collection = CollectionId::new("kb").unwrap();
    let doc_id = DocumentId::for_source(&collection, "s3://b/d.pdf");
    let doc = Document::new(
        doc_id.clone(),
        collection,
        "s3://b/d.pdf".to_string(),
        ContentHash::of(b"v1"),
        Metadata::new(),
    );
    let chunks: Vec<Chunk> = (0..3u32)
        .map(|i| Chunk::new(doc_id.clone(), i, &format!("chunk text {i}"), 0, 12))
        .collect();
    store.put_document(&doc, &chunks).unwrap();

    // link vectors to chunks
    for c in &chunks {
        let vid = polyvec_core::VectorId::for_chunk(&c.id);
        store
            .insert_vector_for_chunk(&c.id, unit_vec(u64::from(c.index), 8))
            .unwrap();
        assert_eq!(polyvec_core::VectorId::for_chunk(&c.id), vid);
    }

    let (got_doc, got_chunks) = store.get_document(&doc_id).unwrap();
    assert_eq!(got_doc.version, 1);
    assert_eq!(got_chunks.len(), 3);
    assert_eq!(store.stats().documents, 1);

    // vectors are searchable
    let hits = store.search(&unit_vec(1, 8), 3, 32).unwrap();
    assert_eq!(
        hits[0].vector_id,
        polyvec_core::VectorId::for_chunk(&chunks[1].id)
    );

    // cascade delete removes doc, chunks, and hides their vectors
    store.delete_document(&doc_id).unwrap();
    assert!(matches!(
        store.get_document(&doc_id),
        Err(Error::NotFound { .. })
    ));
    let hits = store.search(&unit_vec(1, 8), 3, 32).unwrap();
    assert!(hits.is_empty());
    assert_eq!(store.stats().documents, 0);
}

#[test]
fn open_and_create_reject_bad_states() {
    let dir = tmp("badstates");
    // open on empty dir -> NotFound
    assert!(matches!(
        CollectionStore::open(&dir),
        Err(Error::NotFound { .. })
    ));
    // create twice -> AlreadyExists
    let _ = CollectionStore::create(&dir, test_config(8)).unwrap();
    assert!(matches!(
        CollectionStore::create(&dir, test_config(8)),
        Err(Error::AlreadyExists { .. })
    ));
    // bad dimension -> Config error at construction
    assert!(CollectionConfig::new(CollectionId::new("k").unwrap(), 0, DistanceMetric::L2).is_err());
}

#[test]
fn index_state_tracks_sync_status() {
    let dir = tmp("syncstate");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let s: IndexState = store.index_state();
    assert!(matches!(s.sync, SyncStatus::Clean));
    assert_eq!(s.vector_count, 0);

    store
        .insert_vector(Vector::new(vid(0), unit_vec(0, 8), 8).unwrap())
        .unwrap();
    assert!(matches!(store.index_state().sync, SyncStatus::Dirty));
    assert_eq!(store.index_state().vector_count, 1);

    store.flush().unwrap();
    let s = store.index_state();
    assert!(matches!(s.sync, SyncStatus::Clean));
    assert!(s.generation >= 1);
}

#[test]
fn read_collection_config_without_opening_store() {
    use polyvec_storage::read_collection_config;

    let dir = tmp("readcfg");
    let cfg = test_config(24);
    let store = CollectionStore::create(&dir, cfg.clone());
    assert!(store.is_ok());
    drop(store);

    // Reads the config without loading the ANN index.
    let read = read_collection_config(&dir).unwrap();
    assert_eq!(read.dimension, 24);
    assert_eq!(read.name.as_str(), "kb");
    assert_eq!(read, cfg);

    // A directory that holds no collection is NotFound, not a panic.
    let missing = tmp("readcfg-missing");
    let err = read_collection_config(&missing.join("nope")).unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "got {err:?}");
}

#[test]
fn put_document_overwrite_keeps_counters_honest() {
    let dir = tmp("docoverwrite");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let collection = CollectionId::new("kb").unwrap();
    let doc_id = DocumentId::for_source(&collection, "src://a");

    let put = |store: &mut CollectionStore, n_chunks: u32| {
        let doc = Document::new(
            doc_id.clone(),
            collection.clone(),
            "src://a".to_string(),
            ContentHash::of(b"x"),
            Metadata::new(),
        );
        let chunks: Vec<Chunk> = (0..n_chunks)
            .map(|i| Chunk::new(doc_id.clone(), i, "text", 0, 4))
            .collect();
        store.put_document(&doc, &chunks).unwrap();
    };

    put(&mut store, 2);
    let s = store.stats();
    assert_eq!(s.documents, 1);
    assert_eq!(s.chunks, 2);

    // Re-putting the same document (new version) must not double-count.
    put(&mut store, 3);
    let s = store.stats();
    assert_eq!(s.documents, 1, "documents counter inflated: {s:?}");
    assert_eq!(s.chunks, 3, "chunks counter wrong: {s:?}");
}

// ---------------------------------------------------------------------------
// Phase 4: atomic document ingest + model-tag stamping
// ---------------------------------------------------------------------------

fn make_versioned_doc(
    version: u64,
    texts: &[&str],
    dim: usize,
) -> (Document, Vec<Chunk>, Vec<Vector>) {
    let collection = CollectionId::new("kb").unwrap();
    let document_id = DocumentId::for_source(&collection, "src://phase4");
    let doc = Document {
        id: document_id.clone(),
        collection: collection.clone(),
        source: "src://phase4".to_string(),
        metadata: Metadata::default(),
        content_hash: ContentHash::of(format!("content-v{version}").as_bytes()),
        version,
        provenance: polyvec_core::Provenance::Ingested {
            source: "phase4-test".to_string(),
            at_secs: 0,
        },
    };
    let chunks: Vec<Chunk> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Chunk::new_versioned(
                document_id.clone(),
                version,
                u32::try_from(i).unwrap(),
                t,
                0,
                t.len(),
            )
        })
        .collect();
    let vectors: Vec<Vector> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| {
            Vector::new(
                VectorId::for_chunk(&c.id),
                unit_vec(u64::try_from(i).unwrap() + version * 100, dim),
                u32::try_from(dim).unwrap(),
            )
            .unwrap()
        })
        .collect();
    (doc, chunks, vectors)
}

#[test]
fn ingest_document_stores_doc_chunks_and_vectors() {
    let dir = tmp("ingest-basic");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let (doc, chunks, vectors) = make_versioned_doc(1, &["alpha one", "alpha two"], 8);
    store.ingest_document(&doc, &chunks, &vectors).unwrap();

    let (got, got_chunks) = store.get_document(&doc.id).unwrap();
    assert_eq!(got.version, 1);
    assert_eq!(got_chunks.len(), 2);
    assert_eq!(got_chunks[0].id, chunks[0].id);

    // The chunk vectors are searchable immediately.
    let hits = store.search(&vectors[0].values, 5, 32).unwrap();
    assert!(
        hits.iter().any(|h| h.vector_id == vectors[0].id),
        "ingested vector must be searchable"
    );
    let stats = store.stats();
    assert_eq!(stats.documents, 1);
}

#[test]
fn ingest_document_new_version_tombstones_old_vectors() {
    let dir = tmp("ingest-version");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let (v1, chunks_v1, vectors_v1) = make_versioned_doc(1, &["alpha one", "alpha two"], 8);
    store.ingest_document(&v1, &chunks_v1, &vectors_v1).unwrap();
    let old_chunk_ids: Vec<_> = chunks_v1.iter().map(|c| c.id.clone()).collect();

    // v2 with changed content: same document id, bumped version.
    let (v2, chunks_v2, vectors_v2) = make_versioned_doc(2, &["beta one", "beta two"], 8);
    assert_eq!(v2.id, v1.id);
    store.ingest_document(&v2, &chunks_v2, &vectors_v2).unwrap();

    let (got, got_chunks) = store.get_document(&v2.id).unwrap();
    assert_eq!(got.version, 2, "version must bump");
    assert_eq!(got_chunks.len(), 2);
    assert!(
        got_chunks.iter().all(|c| !old_chunk_ids.contains(&c.id)),
        "old chunks must be replaced"
    );

    // Old vectors are tombstoned: unsearchable immediately.
    let hits = store.search(&vectors_v1[0].values, 10, 32).unwrap();
    assert!(
        !hits.iter().any(|h| h.vector_id == vectors_v1[0].id),
        "old version vectors must be tombstoned, got {:?}",
        hits.iter().map(|h| &h.vector_id).collect::<Vec<_>>()
    );
    assert!(
        !hits.iter().any(|h| h.vector_id == vectors_v1[1].id),
        "all old version vectors must be tombstoned"
    );
    // New vectors are searchable.
    let hits = store.search(&vectors_v2[0].values, 10, 32).unwrap();
    assert!(hits.iter().any(|h| h.vector_id == vectors_v2[0].id));
}

#[test]
fn stamp_model_tag_claims_collection_once() {
    let dir = tmp("stamp-tag");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    assert!(store.config().model.is_none());

    let tag_a = polyvec_core::ModelTag::new("e5", "1.0").unwrap();
    store.stamp_model_tag(&tag_a).unwrap();
    assert_eq!(store.config().model.as_ref().unwrap().name, "e5");

    // Same tag again: no-op.
    store.stamp_model_tag(&tag_a).unwrap();

    // Different tag: refused — never silently mix models.
    let tag_b = polyvec_core::ModelTag::new("other", "1.0").unwrap();
    let err = store.stamp_model_tag(&tag_b).unwrap_err();
    assert!(matches!(err, Error::AlreadyExists { .. }), "got {err:?}");

    // The claim survives reopen.
    drop(store);
    let (store, _) = CollectionStore::open(&dir).unwrap();
    assert_eq!(store.config().model.as_ref().unwrap().name, "e5");
}

// ---------------------------------------------------------------------------
// Phase 5: chunk lookup + document scan (retrieval enrichment, lexical rebuild)
// ---------------------------------------------------------------------------

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

#[test]
fn get_chunk_returns_text_for_enrichment() {
    let dir = tmp("get-chunk");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let collection = CollectionId::new("kb").unwrap();
    let (doc, chunks) = make_doc(&collection, "s3://b/d.pdf", 2);
    store.put_document(&doc, &chunks).unwrap();

    let got = store
        .get_chunk(&chunks[1].id)
        .unwrap()
        .expect("chunk exists");
    assert_eq!(got.text, "s3://b/d.pdf chunk 1");
    assert_eq!(got.document_id, doc.id);

    // Unknown chunk id -> None, not an error.
    let missing = polyvec_core::ChunkId::parse("kb#deadbeef#c000000").unwrap();
    assert!(store.get_chunk(&missing).unwrap().is_none());

    // Cascade delete removes the chunk from lookup too.
    store.delete_document(&doc.id).unwrap();
    assert!(store.get_chunk(&chunks[0].id).unwrap().is_none());
}

#[test]
fn all_documents_scans_docs_with_chunks_in_key_order() {
    let dir = tmp("all-docs");
    let mut store = CollectionStore::create(&dir, test_config(8)).unwrap();
    let collection = CollectionId::new("kb").unwrap();

    assert!(store.all_documents().unwrap().is_empty());

    let (doc_b, chunks_b) = make_doc(&collection, "src://b", 2);
    let (doc_a, chunks_a) = make_doc(&collection, "src://a", 1);
    store.put_document(&doc_b, &chunks_b).unwrap();
    store.put_document(&doc_a, &chunks_a).unwrap();

    let all = store.all_documents().unwrap();
    assert_eq!(all.len(), 2);
    // Key order (document ids sort by source hash, not insertion order);
    // what matters is completeness, not order.
    let mut sources: Vec<_> = all.iter().map(|(d, _)| d.source.clone()).collect();
    sources.sort();
    assert_eq!(sources, vec!["src://a".to_string(), "src://b".to_string()]);
    for (doc, chunks) in &all {
        let want = if doc.source == "src://a" { 1 } else { 2 };
        assert_eq!(chunks.len(), want, "doc {}", doc.source);
        assert!(chunks.windows(2).all(|w| w[0].index < w[1].index));
    }
}

// ---------------------------------------------------------------------------
// Phase 7 reliability: concurrency soak, write failure, restart-during-writes
// ---------------------------------------------------------------------------

use std::sync::{Arc, Barrier, Mutex};

/// Deterministic xorshift64* step for the soak workers.
fn soak_step(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    *state = x;
    x
}

/// Eight threads hammer one store (the same `Arc<Mutex<…>>` shape the server
/// uses) with interleaved inserts, searches, updates and deletes. Asserts
/// no panic/deadlock and a coherent final state: every search hit is
/// gettable and the live count is internally consistent.
#[test]
fn concurrent_read_write_soak() {
    let dir = tmp("concurrent-soak");
    let store = Arc::new(Mutex::new(
        CollectionStore::create(&dir, test_config(16)).unwrap(),
    ));
    {
        let mut s = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for i in 0..64u64 {
            s.insert_vector(Vector::new(vid(i), unit_vec(i, 16), 16).unwrap())
                .unwrap();
        }
    }

    let barrier = Arc::new(Barrier::new(9));
    let mut handles = Vec::new();
    for t in 0..8u64 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let mut rng =
                0x9E37_79B9_7F4A_7C15u64.wrapping_add(t.wrapping_mul(0xBF58_476D_1CE4_E5B9));
            let mut inserts = 0u64;
            let mut deletes = 0u64;
            for i in 0..60u64 {
                let mut s = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match soak_step(&mut rng) % 4 {
                    0 => {
                        let id = 10_000 + t * 1_000 + i; // thread-unique
                        if s
                            .insert_vector(Vector::new(vid(id), unit_vec(id, 16), 16).unwrap())
                            .is_ok()
                        {
                            inserts += 1;
                        }
                    }
                    1 => {
                        let q = unit_vec(t.wrapping_mul(31).wrapping_add(i), 16);
                        let _ = s.search(&q, 5, 32);
                    }
                    2 => {
                        let id = 10_000 + t * 1_000 + (i % 60); // may not exist yet
                        // Net-zero on success (tombstone + reinsert); NotFound
                        // when the id was never inserted — both fine.
                        let _ = s.update_vector(
                            Vector::new(vid(id), unit_vec(id.wrapping_add(7), 16), 16).unwrap(),
                        );
                    }
                    _ => {
                        // Delete a seed vector; count only effective deletes
                        // (live → tombstoned) so the final count is exact.
                        let id = vid((t + i) % 64);
                        if s.get_vector(&id).is_ok() {
                            s.delete_vector(&id).unwrap();
                            deletes += 1;
                        }
                    }
                }
            }
            (inserts, deletes)
        }));
    }
    barrier.wait();
    let mut total_inserts = 0u64;
    let mut total_deletes = 0u64;
    for h in handles {
        let (inserts, deletes) = h.join().expect("soak worker panicked");
        total_inserts += inserts;
        total_deletes += deletes;
    }

    let s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stats = s.stats();
    let hits = s.search(&unit_vec(3, 16), 10, 32).unwrap();
    assert!(hits.len() <= 10);
    for hit in &hits {
        assert!(
            s.get_vector(&hit.vector_id).is_ok(),
            "search hit must be gettable"
        );
    }
    // Inserts were thread-unique; updates are net-zero (tombstone +
    // reinsert); deletes counted only when effective. The final live
    // count is exact — no lost updates, no phantom vectors.
    let expected_live = 64 + total_inserts - total_deletes;
    assert_eq!(
        stats.live_vectors, expected_live,
        "live_vectors={} != 64 + {inserts} - {deletes}",
        stats.live_vectors,
        inserts = total_inserts,
        deletes = total_deletes,
    );
}

/// A file where the data dir should be: open/create must fail with a
/// storage error, never a panic. (True ENOSPC is not simulatable in a unit
/// test — recorded as a known limitation.)
#[test]
fn write_failure_when_data_dir_is_a_file() {
    let dir = tmp("write-failure");
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();

    let Err(err) = CollectionStore::create(&blocker, test_config(16)) else {
        panic!("create on a file path must fail")
    };
    assert!(
        matches!(err, Error::Storage { .. }),
        "create on a file path must be Storage, got: {err:?}"
    );
    let Err(err) = CollectionStore::open(&blocker) else {
        panic!("open on a file path must fail")
    };
    assert!(
        matches!(
            err,
            Error::Storage { .. } | Error::Corrupt { .. } | Error::NotFound { .. }
        ),
        "open on a file path must be a clean error, got: {err:?}"
    );
}

/// Writes happen, the store is dropped without a flush ("restart"), more
/// writes happen after reopen, then a final reopen. The catalog is the
/// source of truth (ADR-004): no vector is lost across the restarts.
#[test]
fn restart_between_write_batches_recovers() {
    let dir = tmp("restart-writes");
    {
        let mut store = CollectionStore::create(&dir, test_config(16)).unwrap();
        for i in 0..30u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i, 16), 16).unwrap())
                .unwrap();
        }
        store.flush().unwrap();
        // dropped here without a second flush — like a restart mid-batch
    }
    {
        let (mut store, _) = CollectionStore::open(&dir).unwrap();
        for i in 30..60u64 {
            store
                .insert_vector(Vector::new(vid(i), unit_vec(i, 16), 16).unwrap())
                .unwrap();
        }
        // dropped without flush again
    }
    let (store, _) = CollectionStore::open(&dir).unwrap();
    assert_eq!(store.stats().live_vectors, 60);
    let hits = store.search(&unit_vec(45, 16), 5, 100).unwrap();
    assert!(!hits.is_empty());
}
