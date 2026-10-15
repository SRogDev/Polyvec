//! Phase 6 RED tests: relation endpoints + relationship-aware retrieval.
//!
//! FAILS until implemented. Drives the HTTP layer through actix-web test
//! clients (same harness as tests/api.rs).

use actix_web::test as awtest;
use actix_web::{App, http::StatusCode, web};
use polyvec_server::{AppState, EmbeddingKind, ServerConfig, configure_routes};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_config(data_dir: PathBuf) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        data_dir,
        json_limit: 8 * 1024,
        workers: None,
        allow_nonlocal_bind: false,
        embedding: EmbeddingKind::None,
        job_queue_capacity: 16,
        job_concurrency: 2,
    }
}

fn tmp_data_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("polyvec-rel-api-test-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn test_app(
    data_dir: PathBuf,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
> {
    let state = AppState::new(&test_config(data_dir)).expect("test app state");
    awtest::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(move |cfg| configure_routes(cfg, 8 * 1024)),
    )
    .await
}

async fn create_collection(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    id: &str,
) {
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({"id": id, "dimension": 8, "metric": "cosine"}))
        .to_request();
    let resp = awtest::call_service(app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create {id}");
}

/// PUTs a document; returns (`document_id`, `chunk_ids`).
async fn put_doc(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    collection: &str,
    source: &str,
    texts: &[&str],
) -> (String, Vec<String>) {
    let chunks: Vec<Value> = texts
        .iter()
        .map(|t| json!({"text": t, "char_start": 0, "char_end": t.len()}))
        .collect();
    let req = awtest::TestRequest::put()
        .uri(&format!("/v1/collections/{collection}/documents"))
        .set_json(json!({"source": source, "chunks": chunks}))
        .to_request();
    let resp = awtest::call_service(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "put {source}");
    let body: Value = awtest::read_body_json(resp).await;
    let doc_id = body["id"].as_str().unwrap().to_string();
    let chunk_ids: Vec<String> = body["chunk_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    (doc_id, chunk_ids)
}

async fn create_relation(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    collection: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = awtest::TestRequest::post()
        .uri(&format!("/v1/collections/{collection}/relations"))
        .set_json(body)
        .to_request();
    let resp = awtest::call_service(app, req).await;
    let status = resp.status();
    let body: Value = awtest::read_body_json(resp).await;
    (status, body)
}

fn rel_body(rel_type: &str, source: &str, target: &str) -> Value {
    json!({
        "type": rel_type,
        "source": source,
        "target": target,
        "metadata": {"created_by": "test"}
    })
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().expect("error.code present")
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn relation_crud_roundtrip() {
    let app = test_app(tmp_data_dir()).await;
    create_collection(&app, "kb").await;
    let (doc_a, _) = put_doc(&app, "kb", "https://a.example", &["alpha"]).await;
    let (doc_b, _) = put_doc(&app, "kb", "https://b.example", &["beta"]).await;

    // Create with explicit id.
    let (status, body) = create_relation(&app, "kb", rel_body("references", &doc_a, &doc_b)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["type"], "references");
    assert_eq!(body["source"], doc_a);
    assert_eq!(body["target"], doc_b);
    assert_eq!(body["metadata"]["created_by"], "test");
    assert!(body["created_at"].as_i64().unwrap() > 0);
    let rel_id = body["id"].as_str().unwrap().to_string();
    assert!(rel_id.starts_with("rel-"), "generated id, got {rel_id}");

    // List: the relation is there.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/relations")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["relations"][0]["id"], rel_id);

    // Delete is idempotent.
    let req = awtest::TestRequest::delete()
        .uri(&format!("/v1/collections/kb/relations/{rel_id}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let req = awtest::TestRequest::delete()
        .uri(&format!("/v1/collections/kb/relations/{rel_id}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // List is empty again.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/relations")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["count"], 0);
}

#[actix_web::test]
async fn relation_create_validation_errors() {
    let app = test_app(tmp_data_dir()).await;
    create_collection(&app, "kb").await;
    let (doc_a, chunks_a) = put_doc(&app, "kb", "https://a.example", &["alpha"]).await;
    let (doc_b, _) = put_doc(&app, "kb", "https://b.example", &["beta"]).await;

    // Unknown relation type → 400.
    let (status, body) = create_relation(&app, "kb", rel_body("cites", &doc_a, &doc_b)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "validation_error");

    // Unknown endpoint → 404.
    let (status, body) =
        create_relation(&app, "kb", rel_body("references", &doc_a, "no-such-doc")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "not_found");

    // Self-loop → 400.
    let (status, body) = create_relation(&app, "kb", rel_body("references", &doc_a, &doc_a)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "validation_error");

    // Duplicate id → 409.
    let (status, _) = create_relation(
        &app,
        "kb",
        json!({"id": "dup", "type": "references", "source": doc_a, "target": doc_b}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = create_relation(
        &app,
        "kb",
        json!({"id": "dup", "type": "references", "source": doc_a, "target": doc_b}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&body), "already_exists");

    // Chunk endpoints work.
    let (status, body) =
        create_relation(&app, "kb", rel_body("related_to", &chunks_a[0], &doc_b)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["source"], chunks_a[0]);

    // Unknown collection → 404.
    let (status, _) = create_relation(&app, "nope", rel_body("references", &doc_a, &doc_b)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn relation_list_filters_and_pagination() {
    let app = test_app(tmp_data_dir()).await;
    create_collection(&app, "kb").await;
    let (doc_a, _) = put_doc(&app, "kb", "https://a.example", &["alpha"]).await;
    let (doc_b, _) = put_doc(&app, "kb", "https://b.example", &["beta"]).await;
    let (doc_c, _) = put_doc(&app, "kb", "https://c.example", &["gamma"]).await;

    for (t, s, d) in [
        ("references", &doc_a, &doc_b),
        ("related_to", &doc_b, &doc_a),
        ("supersedes", &doc_b, &doc_c),
    ] {
        let (status, _) = create_relation(&app, "kb", rel_body(t, s, d)).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    assert_eq!(get_list(&app, "").await["count"], 3);
    let by_source = get_list(&app, &format!("?source={}", urlencode(&doc_b))).await;
    assert_eq!(by_source["count"], 2);
    let by_target = get_list(&app, &format!("?target={}", urlencode(&doc_a))).await;
    assert_eq!(by_target["count"], 1);
    assert_eq!(by_target["relations"][0]["type"], "related_to");
    let by_type = get_list(&app, "?type=supersedes").await;
    assert_eq!(by_type["count"], 1);
    // Unknown type filter → 400.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/relations?type=bogus")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let page0 = get_list(&app, "?limit=2&offset=0").await;
    let page1 = get_list(&app, "?limit=2&offset=2").await;
    assert_eq!(page0["relations"].as_array().unwrap().len(), 2);
    assert_eq!(page1["relations"].as_array().unwrap().len(), 1);
    assert_eq!(page0["limit"], 2);
    assert_eq!(page1["offset"], 2);
    let id0: Vec<&str> = page0["relations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    let id1 = page1["relations"][0]["id"].as_str().unwrap();
    assert!(!id0.contains(&id1));
}

fn urlencode(s: &str) -> String {
    // Document ids contain '#'; the test client needs it encoded.
    s.replace('#', "%23")
}

async fn get_list(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    qs: &str,
) -> Value {
    let req = awtest::TestRequest::get()
        .uri(&format!("/v1/collections/kb/relations{qs}"))
        .to_request();
    let resp = awtest::call_service(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "GET {qs}");
    awtest::read_body_json(resp).await
}

// ---------------------------------------------------------------------------
// relationship-aware retrieval
// ---------------------------------------------------------------------------

/// The multi-hop fixture: three documents where only A mentions the query
/// term. A --references--> B --references--> C.
async fn fixture_chain(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
) -> (String, String, String) {
    create_collection(app, "kb").await;
    let (doc_a, _) = put_doc(
        app,
        "kb",
        "https://a.example/guide",
        &["the xylophone manual"],
    )
    .await;
    let (doc_b, _) = put_doc(
        app,
        "kb",
        "https://b.example/advanced",
        &["advanced techniques"],
    )
    .await;
    let (doc_c, _) = put_doc(
        app,
        "kb",
        "https://c.example/reference",
        &["reference tables"],
    )
    .await;
    let (status, _) = create_relation(app, "kb", rel_body("references", &doc_a, &doc_b)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = create_relation(app, "kb", rel_body("references", &doc_b, &doc_c)).await;
    assert_eq!(status, StatusCode::CREATED);
    (doc_a, doc_b, doc_c)
}

async fn query(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    body: Value,
) -> (StatusCode, Value) {
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb/query")
        .set_json(body)
        .to_request();
    let resp = awtest::call_service(app, req).await;
    let status = resp.status();
    let body: Value = awtest::read_body_json(resp).await;
    (status, body)
}

#[actix_web::test]
async fn expand_returns_multi_hop_hits_with_evidence() {
    let app = test_app(tmp_data_dir()).await;
    let (doc_a, doc_b, doc_c) = fixture_chain(&app).await;

    // Baseline: lexical alone finds only A (the only doc with the term).
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);
    assert_eq!(body["hits"][0]["document_id"], doc_a);
    assert!(body["hits"][0].get("evidence").is_none());

    // With expand: A (base) + B (1 hop) + C (2 hops), evidence on the
    // expanded hits.
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical",
               "expand": {"depth": 2, "max_nodes": 100}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 3, "full response: {body}");
    // Base hit first, unchanged.
    assert_eq!(hits[0]["document_id"], doc_a);
    assert!(hits[0].get("evidence").is_none());
    // Expanded hits follow in hop order.
    assert_eq!(hits[1]["document_id"], doc_b);
    assert_eq!(hits[1]["path"], json!(["relationship"]));
    assert_eq!(hits[1]["score_kind"], "relationship");
    assert_eq!(hits[1]["evidence"]["hops"], 1);
    assert_eq!(
        hits[1]["evidence"]["path"][0]["relation_type"],
        "references"
    );
    assert_eq!(hits[1]["evidence"]["path"][0]["direction"], "outgoing");
    assert_eq!(hits[2]["document_id"], doc_c);
    assert_eq!(hits[2]["evidence"]["hops"], 2);
    let path = hits[2]["evidence"]["path"].as_array().unwrap();
    assert_eq!(path.len(), 2);
    assert!(
        path.iter()
            .all(|h| h["relation_type"] == "references" && h["direction"] == "outgoing")
    );
    // Scores: hops as f64, lower is better.
    assert_eq!(hits[1]["score"], 1.0);
    assert_eq!(hits[2]["score"], 2.0);
}

#[actix_web::test]
async fn expand_depth_limit_and_type_filter() {
    let app = test_app(tmp_data_dir()).await;
    let (_doc_a, doc_b, _doc_c) = fixture_chain(&app).await;

    // depth 1: C is out of reach.
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical",
               "expand": {"depth": 1}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[1]["document_id"], doc_b);

    // relation_types filter excluding the chain's type: no expansion.
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical",
               "expand": {"relation_types": ["supersedes"]}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);

    // Invalid expand params → 400.
    for bad in [
        json!({"depth": 0}),
        json!({"depth": 6}),
        json!({"max_nodes": 0}),
        json!({"max_nodes": 1001}),
        json!({"relation_types": ["bogus"]}),
    ] {
        let (status, body) = query(
            &app,
            json!({"text": "xylophone", "k": 5, "mode": "lexical", "expand": bad}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "expand={bad}");
        assert_eq!(error_code(&body), "validation_error");
    }
}

#[actix_web::test]
async fn expand_handles_cycle_and_fanout() {
    let app = test_app(tmp_data_dir()).await;
    create_collection(&app, "kb").await;
    // Cycle: A → B → C → A.
    let (doc_a, _) = put_doc(&app, "kb", "https://a.example", &["xylophone"]).await;
    let (doc_b, _) = put_doc(&app, "kb", "https://b.example", &["other"]).await;
    let (doc_c, _) = put_doc(&app, "kb", "https://c.example", &["more"]).await;
    for (s, t) in [(&doc_a, &doc_b), (&doc_b, &doc_c), (&doc_c, &doc_a)] {
        let (status, _) = create_relation(&app, "kb", rel_body("references", s, t)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical", "expand": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    // Base A + B + C exactly once each: the cycle terminates.
    assert_eq!(hits.len(), 3);
    let mut ids: Vec<&str> = hits
        .iter()
        .map(|h| h["document_id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![doc_a.as_str(), doc_b.as_str(), doc_c.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected);

    // Fan-out: D cites 150 docs; max_nodes caps the expansion.
    let (doc_d, _) = put_doc(&app, "kb", "https://d.example", &["xylophone hub"]).await;
    for i in 0..150 {
        let (doc_i, _) = put_doc(&app, "kb", &format!("https://{i}.example"), &["leaf"]).await;
        let (status, _) = create_relation(&app, "kb", rel_body("references", &doc_d, &doc_i)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, body) = query(
        &app,
        json!({"text": "hub", "k": 5, "mode": "lexical",
               "expand": {"max_nodes": 10}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    // 1 base hit (D's chunk) + 10 expanded = 11, not 151.
    assert_eq!(hits.len(), 11, "bounded: {}", hits.len());
}

#[actix_web::test]
async fn delete_document_cascades_relations_via_api() {
    let app = test_app(tmp_data_dir()).await;
    create_collection(&app, "kb").await;
    let (doc_a, chunks_a) = put_doc(&app, "kb", "https://a.example", &["xylophone alpha"]).await;
    let (doc_b, _) = put_doc(&app, "kb", "https://b.example", &["beta"]).await;
    let (status, _) = create_relation(&app, "kb", rel_body("references", &doc_a, &doc_b)).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) =
        create_relation(&app, "kb", rel_body("related_to", &chunks_a[0], &doc_b)).await;
    assert_eq!(status, StatusCode::CREATED);

    // Expand sees B through A before the delete.
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical", "expand": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"].as_array().unwrap().len(), 2);

    // DELETE the document.
    let req = awtest::TestRequest::delete()
        .uri(&format!(
            "/v1/collections/kb/documents/{}",
            urlencode(&doc_a)
        ))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // No relations remain retrievable.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/relations")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["count"], 0);

    // And expansion from B no longer reaches the deleted A.
    let (status, body) = query(
        &app,
        json!({"text": "beta", "k": 5, "mode": "lexical", "expand": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["document_id"], doc_b);
}

#[actix_web::test]
async fn expand_absent_keeps_phase5_behavior() {
    let app = test_app(tmp_data_dir()).await;
    let (doc_a, _, _) = fixture_chain(&app).await;
    // No expand key at all: exactly the Phase 5 response shape.
    let (status, body) = query(
        &app,
        json!({"text": "xylophone", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["document_id"], doc_a);
    assert!(body.get("expand").is_none());
    // expand on an unknown collection → 404.
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/nope/query")
        .set_json(json!({"text": "xylophone", "k": 5, "mode": "lexical", "expand": {}}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
