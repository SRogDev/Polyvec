//! Phase 2 RED tests: core domain contracts.
//!
//! These tests define the intended API. They FAIL until the domain modules
//! (ids, metric, config, vector, document, metadata, state) are implemented.

use polyvec_core::{
    Chunk, ChunkId, CollectionConfig, CollectionId, ContentHash, DistanceMetric, Document,
    DocumentId, Error, HnswParams, Metadata, MetadataValue, ModelTag, Provenance, Vector, VectorId,
};

// ---------------------------------------------------------------------------
// IDs: validated, deterministic identity rules
// ---------------------------------------------------------------------------

#[test]
fn collection_id_accepts_slug_and_rejects_garbage() {
    assert!(CollectionId::new("kb-main").is_ok());
    assert!(CollectionId::new("KB-MAIN").is_err()); // lowercase only
    assert!(CollectionId::new("").is_err());
    assert!(CollectionId::new("has space").is_err());
    assert!(CollectionId::new("x".repeat(65).as_str()).is_err()); // > 64 chars
}

#[test]
fn document_id_is_deterministic_from_collection_and_source() {
    let a = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "s3://bucket/doc.pdf");
    let b = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "s3://bucket/doc.pdf");
    assert_eq!(a, b, "same source must yield the same document id");
    let c = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "s3://bucket/other.pdf");
    assert_ne!(a, c, "different source must yield a different document id");
}

#[test]
fn chunk_id_is_deterministic_from_document_and_index() {
    let doc = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src");
    let c0 = ChunkId::for_index(&doc, 0);
    assert_eq!(c0, ChunkId::for_index(&doc, 0));
    assert_ne!(c0, ChunkId::for_index(&doc, 1));
    assert!(c0.as_str().contains(doc.as_str()));
}

#[test]
fn vector_id_is_deterministic_from_chunk() {
    let doc = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src");
    let chunk = ChunkId::for_index(&doc, 3);
    assert_eq!(VectorId::for_chunk(&chunk), VectorId::for_chunk(&chunk));
}

// ---------------------------------------------------------------------------
// DistanceMetric
// ---------------------------------------------------------------------------

#[test]
fn metric_parses_and_displays() {
    assert_eq!(
        "cosine".parse::<DistanceMetric>().unwrap(),
        DistanceMetric::Cosine
    );
    assert_eq!("l2".parse::<DistanceMetric>().unwrap(), DistanceMetric::L2);
    assert!("manhattan".parse::<DistanceMetric>().is_err());
    assert_eq!(DistanceMetric::Cosine.to_string(), "cosine");
    assert_eq!(DistanceMetric::L2.to_string(), "l2");
}

#[test]
fn metric_exact_distance_is_correct() {
    let a = [1.0f32, 0.0];
    let b = [0.0f32, 1.0];
    // cosine distance between orthogonal unit vectors = 1 - 0 = 1
    assert!((DistanceMetric::Cosine.exact(&a, &b) - 1.0).abs() < 1e-6);
    // l2 distance between (1,0) and (0,1) = sqrt(2)
    assert!((DistanceMetric::L2.exact(&a, &b) - 2.0f32.sqrt()).abs() < 1e-6);
    // identical vectors: distance 0 for both
    assert!(DistanceMetric::Cosine.exact(&a, &a) < 1e-6);
    assert!(DistanceMetric::L2.exact(&a, &a) < 1e-6);
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

#[test]
fn collection_config_validates_dimension_and_params() {
    let ok = CollectionConfig::new(
        CollectionId::new("kb").unwrap(),
        128,
        DistanceMetric::Cosine,
    );
    assert!(ok.is_ok());

    assert!(
        CollectionConfig::new(CollectionId::new("kb").unwrap(), 0, DistanceMetric::Cosine).is_err()
    );
    assert!(
        CollectionConfig::new(
            CollectionId::new("kb").unwrap(),
            1_000_000,
            DistanceMetric::Cosine
        )
        .is_err()
    );
}

#[test]
fn hnsw_params_have_sane_validated_defaults() {
    let p = HnswParams::default();
    assert!(p.m >= 4 && p.m <= 64);
    assert!(p.ef_construction >= 16);
    assert!(p.ef_search >= 8);
    let bad = HnswParams {
        m: 0,
        ef_construction: 200,
        ef_search: 64,
        max_elements: 10_000,
    };
    assert!(bad.validate().is_err());
}

#[test]
fn model_tag_is_optional_but_validated_when_present() {
    let cfg = CollectionConfig::new(CollectionId::new("kb").unwrap(), 64, DistanceMetric::L2)
        .unwrap()
        .with_model(ModelTag::new("bge-m3", "1.0").unwrap());
    assert_eq!(cfg.model.as_ref().unwrap().name, "bge-m3");
    assert!(ModelTag::new("", "1.0").is_err());
}

// ---------------------------------------------------------------------------
// Vector validation: dim match, non-empty, finite
// ---------------------------------------------------------------------------

#[test]
fn vector_validation_rejects_mismatches_nan_inf_empty() {
    let dim = 4;
    assert!(
        Vector::new(
            VectorId::for_chunk(&ChunkId::for_index(
                &DocumentId::for_source(&CollectionId::new("k").unwrap(), "s"),
                0
            )),
            vec![1.0; 4],
            dim
        )
        .is_ok()
    );
    let bad_dim = Vector::new(
        VectorId::for_chunk(&ChunkId::for_index(
            &DocumentId::for_source(&CollectionId::new("k").unwrap(), "s"),
            0,
        )),
        vec![1.0; 3],
        dim,
    );
    assert!(matches!(bad_dim, Err(Error::Validation { .. })));
    let nan = Vector::new(
        VectorId::for_chunk(&ChunkId::for_index(
            &DocumentId::for_source(&CollectionId::new("k").unwrap(), "s"),
            0,
        )),
        vec![f32::NAN, 0.0, 0.0, 0.0],
        dim,
    );
    assert!(matches!(nan, Err(Error::Validation { .. })));
    let inf = Vector::new(
        VectorId::for_chunk(&ChunkId::for_index(
            &DocumentId::for_source(&CollectionId::new("k").unwrap(), "s"),
            0,
        )),
        vec![f32::INFINITY, 0.0, 0.0, 0.0],
        dim,
    );
    assert!(matches!(inf, Err(Error::Validation { .. })));
    let empty = Vector::new(
        VectorId::for_chunk(&ChunkId::for_index(
            &DocumentId::for_source(&CollectionId::new("k").unwrap(), "s"),
            0,
        )),
        vec![],
        0,
    );
    assert!(matches!(empty, Err(Error::Validation { .. })));
}

// ---------------------------------------------------------------------------
// ContentHash: deterministic change detection
// ---------------------------------------------------------------------------

#[test]
fn content_hash_is_deterministic_and_sensitive() {
    let h1 = ContentHash::of(b"hello");
    assert_eq!(h1, ContentHash::of(b"hello"));
    assert_ne!(h1, ContentHash::of(b"hellp"));
    assert_eq!(h1.as_bytes().len(), 32);
}

// ---------------------------------------------------------------------------
// Document / Chunk / Metadata
// ---------------------------------------------------------------------------

#[test]
fn document_carries_metadata_version_and_provenance() {
    let collection = CollectionId::new("kb").unwrap();
    let doc_id = DocumentId::for_source(&collection, "s3://b/d.pdf");
    let mut meta = Metadata::new();
    meta.insert("author".to_string(), MetadataValue::Str("ada".to_string()));
    meta.insert("pages".to_string(), MetadataValue::Int(12));
    let doc = Document::new(
        doc_id.clone(),
        collection,
        "s3://b/d.pdf".to_string(),
        ContentHash::of(b"bytes"),
        meta,
    );
    assert_eq!(doc.version, 1);
    assert_eq!(
        doc.metadata.get("author"),
        Some(&MetadataValue::Str("ada".to_string()))
    );
    assert!(matches!(
        doc.provenance,
        Provenance::Manual { .. } | Provenance::Ingested { .. }
    ));

    let updated = doc.with_content(ContentHash::of(b"new bytes"));
    assert_eq!(updated.version, 2);
    assert_eq!(updated.id, doc_id, "identity is stable across versions");
}

#[test]
fn chunk_orders_and_hashes_text() {
    let doc_id = DocumentId::for_source(&CollectionId::new("k").unwrap(), "s");
    let c = Chunk::new(doc_id, 2, "some text", 10, 19);
    assert_eq!(c.index, 2);
    assert_eq!((c.char_start, c.char_end), (10, 19));
    assert_eq!(c.hash, ContentHash::of(b"some text"));
}

// ---------------------------------------------------------------------------
// Error variants exist and display
// ---------------------------------------------------------------------------

#[test]
fn new_error_variants_display() {
    assert!(
        format!(
            "{}",
            Error::NotFound {
                what: "collection 'x'".to_string()
            }
        )
        .contains("not found")
    );
    assert!(
        format!(
            "{}",
            Error::AlreadyExists {
                what: "collection 'x'".to_string()
            }
        )
        .contains("already exists")
    );
    assert!(
        format!(
            "{}",
            Error::Corrupt {
                detail: "checksum mismatch".to_string()
            }
        )
        .contains("checksum mismatch")
    );
    assert!(
        format!(
            "{}",
            Error::Validation {
                detail: "bad dim".to_string()
            }
        )
        .contains("bad dim")
    );
    assert!(
        format!(
            "{}",
            Error::Storage {
                detail: "redb io".to_string()
            }
        )
        .contains("redb io")
    );
}
