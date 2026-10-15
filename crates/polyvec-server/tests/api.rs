//! Phase 3 API tests: drive the HTTP layer through actix-web test clients.
//!
//! Covers successful AND invalid requests. Written before the implementation
//! (strict TDD): this file is the RED step.

use actix_web::test as awtest;
use actix_web::{App, http::StatusCode, web};
use polyvec_embeddings::{EmbeddingProvider, StubEmbeddingProvider};
use polyvec_server::{AppState, EmbeddingKind, ServerConfig, configure_routes};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Small JSON limit so the 413 path is cheap to exercise.
const TEST_JSON_LIMIT: usize = 8 * 1024;

fn test_config(data_dir: PathBuf) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        data_dir,
        json_limit: TEST_JSON_LIMIT,
        workers: None,
        allow_nonlocal_bind: false,
        embedding: EmbeddingKind::None,
        job_queue_capacity: 16,
        job_concurrency: 2,
        api_key: None,
        // Disabled in the test harness for determinism (no timing flakes);
        // rate limiting is covered by dedicated tests below.
        rate_limit_rps: 0,
    }
}

fn tmp_data_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("polyvec-server-test-{name}-{}", std::process::id()));
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
    test_app_with_limit(data_dir, TEST_JSON_LIMIT).await
}

async fn test_app_with_limit(
    data_dir: PathBuf,
    json_limit: usize,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
> {
    let mut config = test_config(data_dir);
    config.json_limit = json_limit;
    test_app_with_config(config).await
}

/// Test app from a fully-specified config (auth / rate-limit tests).
async fn test_app_with_config(
    config: ServerConfig,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
> {
    let state = AppState::new(&config).expect("test app state");
    awtest::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(|cfg| configure_routes(cfg, &config)),
    )
    .await
}

/// Test app with an API key and/or rate limit enabled.
async fn test_app_with_security(
    data_dir: PathBuf,
    api_key: Option<&str>,
    rate_limit_rps: u32,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
> {
    let mut config = test_config(data_dir);
    config.api_key = api_key.map(str::to_string);
    config.rate_limit_rps = rate_limit_rps;
    test_app_with_config(config).await
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().expect("error.code present")
}

fn error_message(body: &Value) -> &str {
    body["error"]["message"]
        .as_str()
        .expect("error.message present")
}

async fn create_collection(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    id: &str,
    dimension: u32,
) -> Value {
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({"id": id, "dimension": dimension, "metric": "cosine"}))
        .to_request();
    let resp = awtest::call_service(app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create {id}");
    awtest::read_body_json(resp).await
}

// ---------------------------------------------------------------------------
// health / ready
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn healthz_returns_ok() {
    let app = test_app(tmp_data_dir("healthz")).await;
    let req = awtest::TestRequest::get().uri("/healthz").to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["status"], "ok");
}

#[actix_web::test]
async fn readyz_returns_ready_when_data_dir_exists() {
    let app = test_app(tmp_data_dir("readyz")).await;
    let req = awtest::TestRequest::get().uri("/readyz").to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["status"], "ready");
}

#[actix_web::test]
async fn readyz_is_503_when_data_dir_missing() {
    let dir = tmp_data_dir("readyz-gone");
    let app = test_app(dir.clone()).await;
    // The data dir vanishes after startup: readiness must report it.
    std::fs::remove_dir_all(&dir).unwrap();
    let req = awtest::TestRequest::get().uri("/readyz").to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "service_unavailable");
}

#[actix_web::test]
async fn error_body_has_structured_shape() {
    let app = test_app(tmp_data_dir("errshape")).await;
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/does-not-exist")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "not_found");
    assert!(!error_message(&body).is_empty());
    assert!(body["error"]["detail"].is_null());
}

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn create_and_get_collection_roundtrip() {
    let app = test_app(tmp_data_dir("col-rt")).await;
    let created = create_collection(&app, "kb-main", 8).await;
    assert_eq!(created["id"], "kb-main");
    assert_eq!(created["dimension"], 8);
    assert_eq!(created["metric"], "cosine");
    assert_eq!(created["lifecycle"], "active");

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-main")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["id"], "kb-main");
    assert_eq!(body["dimension"], 8);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["collections"][0]["id"], "kb-main");
}

#[actix_web::test]
async fn create_collection_with_model_and_hnsw() {
    let app = test_app(tmp_data_dir("col-full")).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({
            "id": "kb-full",
            "dimension": 16,
            "metric": "l2",
            "model": {"name": "bge-m3", "version": "1.0"},
            "hnsw": {"m": 8, "ef_construction": 100, "ef_search": 32, "max_elements": 1000},
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["metric"], "l2");
    assert_eq!(body["model"]["name"], "bge-m3");
    assert_eq!(body["hnsw"]["m"], 8);
}

#[actix_web::test]
async fn create_collection_duplicate_is_409() {
    let app = test_app(tmp_data_dir("col-dup")).await;
    create_collection(&app, "kb-dup", 8).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({"id": "kb-dup", "dimension": 8, "metric": "cosine"}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "already_exists");
}

#[actix_web::test]
async fn create_collection_invalid_id_is_400() {
    let app = test_app(tmp_data_dir("col-badid")).await;
    for bad in ["KB-UPPER", "has space", "", "../evil"] {
        let req = awtest::TestRequest::post()
            .uri("/v1/collections")
            .set_json(json!({"id": bad, "dimension": 8, "metric": "cosine"}))
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "id={bad:?}");
        let body: Value = awtest::read_body_json(resp).await;
        assert_eq!(error_code(&body), "validation_error", "id={bad:?}");
    }
}

#[actix_web::test]
async fn create_collection_bad_dimension_is_400() {
    let app = test_app(tmp_data_dir("col-baddim")).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({"id": "kb-dim", "dimension": 0, "metric": "cosine"}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "config_error");
}

#[actix_web::test]
async fn create_collection_unknown_metric_is_400() {
    let app = test_app(tmp_data_dir("col-badmetric")).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({"id": "kb-m", "dimension": 8, "metric": "manhattan"}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "config_error");
}

#[actix_web::test]
async fn create_collection_bad_hnsw_is_400() {
    let app = test_app(tmp_data_dir("col-badhnsw")).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .set_json(json!({
            "id": "kb-h", "dimension": 8, "metric": "cosine",
            "hnsw": {"m": 2, "ef_construction": 100, "ef_search": 32, "max_elements": 1000},
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "config_error");
}

#[actix_web::test]
async fn delete_collection_roundtrip() {
    let data_dir = tmp_data_dir("col-del");
    let app = test_app(data_dir.clone()).await;
    create_collection(&app, "kb-gone", 8).await;

    let req = awtest::TestRequest::delete()
        .uri("/v1/collections/kb-gone")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-gone")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["count"], 0);

    // The collection directory is really gone.
    assert!(!data_dir.join("kb-gone").exists());

    // Deleting again is 404.
    let req = awtest::TestRequest::delete()
        .uri("/v1/collections/kb-gone")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// vectors
// ---------------------------------------------------------------------------

fn ones(dim: usize) -> Vec<f64> {
    vec![0.5; dim]
}

#[actix_web::test]
async fn insert_vectors_batch_and_get() {
    let app = test_app(tmp_data_dir("vec-batch")).await;
    create_collection(&app, "kb-vec", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-vec/vectors")
        .set_json(json!({"vectors": [
            {"id": "vec-1", "values": ones(4)},
            {"id": "vec-2", "values": [1.0, 0.0, 0.0, 0.0]},
        ]}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["inserted"], 2);
    assert_eq!(body["ids"], json!(["vec-1", "vec-2"]));

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-vec/vectors/vec-1")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["id"], "vec-1");
    assert_eq!(body["values"].as_array().unwrap().len(), 4);
    assert!(body["chunk_id"].is_null());
}

#[actix_web::test]
async fn insert_vector_single_object_shape() {
    let app = test_app(tmp_data_dir("vec-single")).await;
    create_collection(&app, "kb-single", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-single/vectors")
        .set_json(json!({"id": "solo", "values": ones(4)}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["inserted"], 1);
    assert_eq!(body["ids"], json!(["solo"]));
}

#[actix_web::test]
async fn insert_vector_dimension_mismatch_is_400() {
    let app = test_app(tmp_data_dir("vec-dimm")).await;
    create_collection(&app, "kb-dim", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-dim/vectors")
        .set_json(json!({"vectors": [{"id": "bad", "values": ones(3)}]}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn insert_vector_nonfinite_is_400() {
    let app = test_app(tmp_data_dir("vec-inf")).await;
    create_collection(&app, "kb-inf", 4).await;

    // 1e300 parses as a finite f64 but overflows f32 -> +inf at the DTO
    // boundary; the domain rejects non-finite components. (Sent as raw JSON
    // text because Rust denies the 1e999 literal at compile time and
    // serde_json rejects it while parsing.)
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-inf/vectors")
        .insert_header(("content-type", "application/json"))
        .set_payload(r#"{"vectors": [{"id": "inf", "values": [1e300, 0.0, 0.0, 0.0]}]}"#)
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn insert_vector_duplicate_is_409() {
    let app = test_app(tmp_data_dir("vec-dup")).await;
    create_collection(&app, "kb-vdup", 4).await;

    let payload = json!({"vectors": [{"id": "twice", "values": ones(4)}]});
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-vdup/vectors")
        .set_json(payload.clone())
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-vdup/vectors")
        .set_json(payload)
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "already_exists");
}

#[actix_web::test]
async fn insert_vectors_duplicate_within_batch_is_400() {
    let app = test_app(tmp_data_dir("vec-batchdup")).await;
    create_collection(&app, "kb-bdup", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-bdup/vectors")
        .set_json(json!({"vectors": [
            {"id": "same", "values": ones(4)},
            {"id": "same", "values": ones(4)},
        ]}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn insert_vector_into_missing_collection_is_404() {
    let app = test_app(tmp_data_dir("vec-nocol")).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/nope/vectors")
        .set_json(json!({"vectors": [{"id": "v", "values": ones(4)}]}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn get_vector_missing_is_404() {
    let app = test_app(tmp_data_dir("vec-get404")).await;
    create_collection(&app, "kb-g404", 4).await;
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-g404/vectors/ghost")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "not_found");
}

#[actix_web::test]
async fn search_returns_nearest_first() {
    let app = test_app(tmp_data_dir("vec-search")).await;
    create_collection(&app, "kb-search", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-search/vectors")
        .set_json(json!({"vectors": [
            {"id": "far", "values": [1.0, 0.0, 0.0, 0.0]},
            {"id": "near", "values": [0.0, 1.0, 0.0, 0.0]},
        ]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-search/vectors/search")
        .set_json(json!({"vector": [0.0, 1.0, 0.0, 0.0], "k": 2}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["id"], "near");
    assert!(hits[0]["distance"].as_f64().unwrap() < 1e-6);
    assert_eq!(hits[1]["id"], "far");
    assert_eq!(body["k"], 2);
    assert!(body["ef"].as_u64().unwrap() >= 8);
}

#[actix_web::test]
async fn search_k_zero_is_400() {
    let app = test_app(tmp_data_dir("vec-k0")).await;
    create_collection(&app, "kb-k0", 4).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-k0/vectors/search")
        .set_json(json!({"vector": ones(4), "k": 0}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn search_k_too_large_is_400() {
    let app = test_app(tmp_data_dir("vec-kbig")).await;
    create_collection(&app, "kb-kbig", 4).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-kbig/vectors/search")
        .set_json(json!({"vector": ones(4), "k": 1001}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn search_ef_out_of_range_is_400() {
    let app = test_app(tmp_data_dir("vec-ef")).await;
    create_collection(&app, "kb-ef", 4).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-ef/vectors/search")
        .set_json(json!({"vector": ones(4), "k": 2, "ef": 4}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn search_dimension_mismatch_is_400() {
    let app = test_app(tmp_data_dir("vec-sdim")).await;
    create_collection(&app, "kb-sdim", 4).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-sdim/vectors/search")
        .set_json(json!({"vector": ones(5), "k": 2}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn update_vector_roundtrip() {
    let app = test_app(tmp_data_dir("vec-upd")).await;
    create_collection(&app, "kb-upd", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-upd/vectors")
        .set_json(json!({"vectors": [{"id": "chg", "values": ones(4)}]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-upd/vectors/chg")
        .set_json(json!({"values": [1.0, 0.0, 0.0, 0.0]}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["id"], "chg");
    assert_eq!(body["values"], json!([1.0, 0.0, 0.0, 0.0]));

    // The new values are what search sees.
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-upd/vectors/search")
        .set_json(json!({"vector": [1.0, 0.0, 0.0, 0.0], "k": 1}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["hits"][0]["id"], "chg");
    assert!(body["hits"][0]["distance"].as_f64().unwrap() < 1e-6);
}

#[actix_web::test]
async fn update_vector_missing_is_404() {
    let app = test_app(tmp_data_dir("vec-upd404")).await;
    create_collection(&app, "kb-u404", 4).await;
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-u404/vectors/ghost")
        .set_json(json!({"values": ones(4)}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn delete_vector_is_idempotent() {
    let app = test_app(tmp_data_dir("vec-del")).await;
    create_collection(&app, "kb-vdel", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-vdel/vectors")
        .set_json(json!({"vectors": [{"id": "bye", "values": ones(4)}]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    for _ in 0..2 {
        let req = awtest::TestRequest::delete()
            .uri("/v1/collections/kb-vdel/vectors/bye")
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-vdel/vectors/bye")
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );

    // Unknown id is also 404 on delete (never existed).
    let req = awtest::TestRequest::delete()
        .uri("/v1/collections/kb-vdel/vectors/never-existed")
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );
}

// ---------------------------------------------------------------------------
// documents
// ---------------------------------------------------------------------------

fn doc_payload() -> Value {
    json!({
        "source": "https://example.com/guide",
        "metadata": {"author": "roger", "pages": 3, "draft": false, "score": 0.9},
        "chunks": [
            {"text": "hello world", "char_start": 0, "char_end": 11},
            {"text": "second chunk", "char_start": 12, "char_end": 24},
        ],
    })
}

#[actix_web::test]
async fn put_get_delete_document_cascade() {
    let app = test_app(tmp_data_dir("doc-cascade")).await;
    create_collection(&app, "kb-doc", 4).await;

    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-doc/documents")
        .set_json(doc_payload())
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    let doc_id = body["id"].as_str().unwrap().to_string();
    assert!(doc_id.starts_with("kb-doc#"), "derived id: {doc_id}");
    assert_eq!(body["version"], 1);
    assert_eq!(body["chunk_ids"].as_array().unwrap().len(), 2);

    // Chunk embeddings attach to derived chunk ids; the API exposes them.
    let chunk_vec_id = format!("{doc_id}#c000000#v");
    let encoded: String = urlencoding(&chunk_vec_id);
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-doc/vectors")
        .set_json(json!({"vectors": [{"id": chunk_vec_id, "values": ones(4)}]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );
    let req = awtest::TestRequest::get()
        .uri(&format!("/v1/collections/kb-doc/vectors/{encoded}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    // Vectors inserted through the API are standalone: only the ingestion
    // layer (Phase 4) links a vector to its chunk via insert_vector_for_chunk.
    assert!(body["chunk_id"].is_null());

    // GET the document back with chunks and metadata.
    let encoded_doc: String = urlencoding(&doc_id);
    let req = awtest::TestRequest::get()
        .uri(&format!("/v1/collections/kb-doc/documents/{encoded_doc}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["source"], "https://example.com/guide");
    assert_eq!(body["metadata"]["author"], "roger");
    assert_eq!(body["metadata"]["pages"], 3);
    assert_eq!(body["chunks"].as_array().unwrap().len(), 2);
    assert_eq!(body["chunks"][0]["text"], "hello world");
    assert_eq!(body["content_hash"].as_str().unwrap().len(), 64);

    // Re-putting the same source bumps the version without double counting.
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-doc/documents")
        .set_json(doc_payload())
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["id"], doc_id);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-doc/stats")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["stats"]["documents"], 1);
    assert_eq!(body["stats"]["chunks"], 2);

    // DELETE cascades: document, chunks, and their vectors are gone.
    let req = awtest::TestRequest::delete()
        .uri(&format!("/v1/collections/kb-doc/documents/{encoded_doc}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let req = awtest::TestRequest::get()
        .uri(&format!("/v1/collections/kb-doc/documents/{encoded_doc}"))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );
    let req = awtest::TestRequest::get()
        .uri(&format!("/v1/collections/kb-doc/vectors/{encoded}"))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );
}

/// Minimal percent-encoding for `#` in path segments (no extra dependency).
fn urlencoding(s: &str) -> String {
    s.replace('#', "%23")
}

#[actix_web::test]
async fn put_document_chunk_offsets_invalid_is_400() {
    let app = test_app(tmp_data_dir("doc-badchunk")).await;
    create_collection(&app, "kb-dc", 4).await;
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-dc/documents")
        .set_json(json!({
            "source": "src://x",
            "chunks": [{"text": "abc", "char_start": 5, "char_end": 2}],
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn put_document_bad_hash_is_400() {
    let app = test_app(tmp_data_dir("doc-badhash")).await;
    create_collection(&app, "kb-dh", 4).await;
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-dh/documents")
        .set_json(json!({
            "source": "src://x",
            "content_hash": "not-a-hash",
            "chunks": [{"text": "abc", "char_start": 0, "char_end": 3}],
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn put_document_bad_metadata_is_400() {
    let app = test_app(tmp_data_dir("doc-badmeta")).await;
    create_collection(&app, "kb-dm", 4).await;
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb-dm/documents")
        .set_json(json!({
            "source": "src://x",
            "metadata": {"nested": {"a": 1}},
            "chunks": [{"text": "abc", "char_start": 0, "char_end": 3}],
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "validation_error");
}

#[actix_web::test]
async fn get_document_missing_is_404() {
    let app = test_app(tmp_data_dir("doc-404")).await;
    create_collection(&app, "kb-d404", 4).await;
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-d404/documents/ghost")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// stats / flush
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn stats_reflects_writes() {
    let app = test_app(tmp_data_dir("stats")).await;
    create_collection(&app, "kb-stats", 4).await;

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-stats/stats")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["id"], "kb-stats");
    assert_eq!(body["stats"]["live_vectors"], 0);
    assert_eq!(body["index_state"]["sync"], "clean");
    assert_eq!(body["index_state"]["index_type"], "hnsw_rs");

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-stats/vectors")
        .set_json(json!({"vectors": [{"id": "v1", "values": ones(4)}]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-stats/stats")
        .to_request();
    let body: Value = awtest::read_body_json(awtest::call_service(&app, req).await).await;
    assert_eq!(body["stats"]["live_vectors"], 1);
    assert_eq!(body["stats"]["generation"], 1);
    assert_eq!(body["index_state"]["sync"], "dirty");
}

#[actix_web::test]
async fn flush_endpoint_checkpoints() {
    let app = test_app(tmp_data_dir("flush")).await;
    create_collection(&app, "kb-flush", 4).await;

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-flush/vectors")
        .set_json(json!({"vectors": [{"id": "v1", "values": ones(4)}]}))
        .to_request();
    assert_eq!(
        awtest::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-flush/flush")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(body["flushed"], true);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb-flush/stats")
        .to_request();
    let body: Value = awtest::read_body_json(awtest::call_service(&app, req).await).await;
    assert_eq!(body["index_state"]["sync"], "clean");
}

// ---------------------------------------------------------------------------
// request limits & path safety
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn payload_too_large_is_413() {
    let app = test_app(tmp_data_dir("limit413")).await;
    create_collection(&app, "kb-lim", 4).await;

    // ~6KB of vectors, over the 8KB test limit once serialized with overhead.
    let vectors: Vec<Value> = (0..300)
        .map(|i| json!({"id": format!("v-{i:05}"), "values": ones(4)}))
        .collect();
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-lim/vectors")
        .set_json(json!({"vectors": vectors}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "payload_too_large");
}

#[actix_web::test]
async fn malformed_json_is_400() {
    let app = test_app(tmp_data_dir("badjson")).await;
    create_collection(&app, "kb-json", 4).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-json/vectors")
        .insert_header(("content-type", "application/json"))
        .set_payload("{not valid json")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "invalid_json");
}

#[actix_web::test]
async fn path_traversal_in_collection_id_cannot_escape() {
    let data_dir = tmp_data_dir("traversal");
    // A tempting target outside the data dir.
    let outside = data_dir.join("..").join(format!(
        "polyvec-server-test-traversal-outside-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&outside);

    let app = test_app(data_dir.clone()).await;
    for uri in [
        "/v1/collections/../evil/stats",
        "/v1/collections/%2e%2e/evil/stats",
    ] {
        let req = awtest::TestRequest::get().uri(uri).to_request();
        let resp = awtest::call_service(&app, req).await;
        assert!(
            resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::BAD_REQUEST,
            "uri={uri} status={}",
            resp.status()
        );
    }
    assert!(
        !outside.exists(),
        "path traversal created a directory outside the data dir"
    );
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

#[test]
fn config_defaults_bind_loopback() {
    let cfg = ServerConfig::from_env_in(&[]).unwrap();
    assert_eq!(cfg.bind.to_string(), "127.0.0.1:8080");
    assert!(!cfg.allow_nonlocal_bind);
    assert_eq!(cfg.json_limit, 16 * 1024 * 1024);
}

#[test]
fn config_refuses_nonlocal_bind_without_opt_in() {
    let err = ServerConfig::from_env_in(&[("POLYVEC_BIND", "0.0.0.0:8080")]).unwrap_err();
    assert!(
        err.contains("POLYVEC_ALLOW_NONLOCAL_BIND"),
        "unexpected error: {err}"
    );
}

#[test]
fn config_allows_nonlocal_bind_with_explicit_opt_in() {
    let cfg = ServerConfig::from_env_in(&[
        ("POLYVEC_BIND", "0.0.0.0:8080"),
        ("POLYVEC_ALLOW_NONLOCAL_BIND", "1"),
    ])
    .unwrap();
    assert_eq!(cfg.bind.to_string(), "0.0.0.0:8080");
}

#[test]
fn config_parses_knobs() {
    let cfg = ServerConfig::from_env_in(&[
        ("POLYVEC_DATA_DIR", "/tmp/polyvec-data"),
        ("POLYVEC_JSON_LIMIT_BYTES", "1048576"),
        ("POLYVEC_WORKERS", "4"),
    ])
    .unwrap();
    assert_eq!(cfg.data_dir, PathBuf::from("/tmp/polyvec-data"));
    assert_eq!(cfg.json_limit, 1_048_576);
    assert_eq!(cfg.workers, Some(4));
}

#[test]
fn config_rejects_bad_values() {
    assert!(ServerConfig::from_env_in(&[("POLYVEC_BIND", "not-an-addr")]).is_err());
    assert!(ServerConfig::from_env_in(&[("POLYVEC_JSON_LIMIT_BYTES", "0")]).is_err());
    assert!(ServerConfig::from_env_in(&[("POLYVEC_WORKERS", "0")]).is_err());
}

// ---------------------------------------------------------------------------
// blocking-pool isolation: a slow insert must not stall /healthz
// ---------------------------------------------------------------------------

/// A slow batch insert runs on the blocking pool while `/healthz` is served
/// by the async workers. Healthz must answer quickly even mid-insert.
///
/// Bound: p50 healthz latency < 1s while a multi-second insert is in flight.
/// (Calibrated against a real run; the insert below takes ~2-4s in debug.)
#[actix_web::test]
async fn slow_insert_does_not_stall_healthz() {
    // The insert body is ~300KB; the default 8KB test limit would 413 it.
    let app = test_app_with_limit(tmp_data_dir("concurrent"), 2 * 1024 * 1024).await;
    create_collection(&app, "kb-slow", 32).await;

    let vectors: Vec<Value> = (0..1500)
        .map(|i| {
            let vals: Vec<f64> = (0..32)
                .map(|d| f64::from((i * 32 + d) % 100) / 100.0)
                .collect();
            json!({"id": format!("slow-{i:05}"), "values": vals})
        })
        .collect();

    let insert_req = awtest::TestRequest::post()
        .uri("/v1/collections/kb-slow/vectors")
        .set_json(json!({"vectors": vectors}))
        .to_request();
    let health_req = awtest::TestRequest::get().uri("/healthz").to_request();

    let insert = awtest::call_service(&app, insert_req);
    let health = async {
        let start = std::time::Instant::now();
        let resp = awtest::call_service(&app, health_req).await;
        (resp.status(), start.elapsed())
    };
    let (insert_resp, (health_status, health_latency)) = tokio::join!(insert, health);

    assert_eq!(insert_resp.status(), StatusCode::CREATED);
    assert_eq!(health_status, StatusCode::OK);
    assert!(
        health_latency.as_secs() < 1,
        "healthz stalled by concurrent insert: {health_latency:?}"
    );
}

// ---------------------------------------------------------------------------
// ingestion (Phase 4)
// ---------------------------------------------------------------------------

/// Test app with an explicit embedding provider.
async fn test_app_with_provider(
    data_dir: PathBuf,
    provider: Option<Arc<dyn polyvec_embeddings::EmbeddingProvider>>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse,
    Error = actix_web::Error,
> {
    let config = test_config(data_dir);
    let state = AppState::with_provider(&config, provider).expect("test app state");
    awtest::init_service(
        App::new()
            .app_data(web::Data::new(state))
            .configure(|cfg| configure_routes(cfg, &config)),
    )
    .await
}

fn stub_provider() -> Arc<dyn polyvec_embeddings::EmbeddingProvider> {
    Arc::new(StubEmbeddingProvider::new(64).expect("stub provider"))
}

/// Polls `GET /v1/jobs/{id}` until the job is terminal.
async fn wait_job_done(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    job_id: &str,
) -> Value {
    let start = std::time::Instant::now();
    loop {
        let req = awtest::TestRequest::get()
            .uri(&format!("/v1/jobs/{job_id}"))
            .to_request();
        let resp = awtest::call_service(app, req).await;
        assert_eq!(resp.status(), StatusCode::OK, "get job {job_id}");
        let body: Value = awtest::read_body_json(resp).await;
        let state = body["state"].as_str().unwrap();
        if state == "done" || state == "failed" {
            return body;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "job {job_id} stuck in {state}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn submit_ingest(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    collection: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let req = awtest::TestRequest::post()
        .uri(&format!("/v1/collections/{collection}/ingest"))
        .set_json(payload)
        .to_request();
    let resp = awtest::call_service(app, req).await;
    let status = resp.status();
    let body: Value = awtest::read_body_json(resp).await;
    (status, body)
}

#[actix_web::test]
async fn ingest_and_query_end_to_end() {
    let dir = tmp_data_dir("ingest-e2e");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    // 1. Ingest HTML bytes (base64) → 202 with a job id.
    let html =
        "<html><body><h1>Rust guide</h1><p>Ownership, borrowing, lifetimes.</p></body></html>";
    let payload = json!({
        "source": "https://example.com/rust",
        "filename": "rust.html",
        "mime": "text/html",
        "content": {"base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, html)},
        "metadata": {"topic": "rust"},
    });
    let (status, body) = submit_ingest(&app, "kb", payload).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    assert_eq!(body["state"], "pending");

    // 2. Poll until done; the job converted HTML → markdown → chunks → vectors.
    let job = wait_job_done(&app, &job_id).await;
    assert_eq!(job["state"], "done", "error: {}", job["error"]);
    assert_eq!(job["affected"]["version"], 1);
    assert_eq!(job["affected"]["skipped"], false);
    assert!(job["affected"]["chunks"].as_u64().unwrap() > 0);

    // 3. Text query finds it (stub provider embeds the query).
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb/query")
        .set_json(json!({"text": "rust ownership borrowing", "k": 5}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert!(!body["hits"].as_array().unwrap().is_empty(), "body: {body}");
    assert!(body["hits"][0]["chunk_id"].is_string());

    // 4. Re-ingest unchanged content → done + skipped, no duplicate version.
    let payload = json!({
        "source": "https://example.com/rust",
        "filename": "rust.html",
        "mime": "text/html",
        "content": {"base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, html)},
    });
    let (status, body) = submit_ingest(&app, "kb", payload).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = wait_job_done(&app, body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done");
    assert_eq!(job["affected"]["skipped"], true);
    assert_eq!(job["affected"]["version"], 1);

    // 5. Update with changed content → version 2.
    let html2 = "<html><body><h1>Rust guide</h1><p>Ownership, borrowing, lifetimes, async.</p></body></html>";
    let payload = json!({
        "source": "https://example.com/rust",
        "filename": "rust.html",
        "mime": "text/html",
        "content": {"base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, html2)},
    });
    let (status, body) = submit_ingest(&app, "kb", payload).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = wait_job_done(&app, body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done");
    assert_eq!(job["affected"]["version"], 2);
    assert_eq!(job["affected"]["skipped"], false);
    let document_id = job["affected"]["document_id"].as_str().unwrap().to_string();

    // 6. Job listing shows the jobs.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/jobs?limit=10")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert!(body["count"].as_u64().unwrap() >= 3, "body: {body}");

    // 7. Delete the document → its vectors are tombstoned: the text query
    //    finds nothing. (`#` is URL-encoded: it would start a fragment.)
    let encoded_id = document_id.replace('#', "%23");
    let req = awtest::TestRequest::delete()
        .uri(&format!("/v1/collections/kb/documents/{encoded_id}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "delete {document_id}"
    );
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb/query")
        .set_json(json!({"text": "rust ownership borrowing", "k": 5}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert!(
        body["hits"].as_array().unwrap().is_empty(),
        "stale vectors still searchable: {body}"
    );
}

#[actix_web::test]
async fn query_without_provider_is_501() {
    let dir = tmp_data_dir("query-501");
    let app = test_app(dir).await;
    create_collection(&app, "kb", 64).await;
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb/query")
        .set_json(json!({"text": "hello", "k": 5}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "unsupported");
}

#[actix_web::test]
async fn ingest_without_provider_or_vectors_is_501() {
    let dir = tmp_data_dir("ingest-501");
    let app = test_app(dir).await;
    create_collection(&app, "kb", 64).await;
    let (status, body) = submit_ingest(
        &app,
        "kb",
        json!({
            "source": "https://example.com/x",
            "content": {"text": "hello"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "body: {body}");
    assert_eq!(error_code(&body), "unsupported");
}

#[actix_web::test]
async fn ingest_supplied_vectors_needs_no_provider() {
    // Supplied vectors bypass the provider entirely.
    let dir = tmp_data_dir("ingest-supplied");
    let app = test_app(dir).await;
    create_collection(&app, "kb", 4).await;
    let (status, body) = submit_ingest(
        &app,
        "kb",
        json!({
            "source": "doc-1",
            "filename": "a.txt",
            "mime": "text/plain",
            "content": {"text": "hello world"},
            "vectors": [[0.1, 0.2, 0.3, 0.4]],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let job = wait_job_done(&app, body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "error: {}", job["error"]);
    assert_eq!(job["affected"]["vectors"], 1);
}

#[actix_web::test]
async fn ingest_unknown_collection_is_404() {
    let dir = tmp_data_dir("ingest-404");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    let (status, body) = submit_ingest(
        &app,
        "nope",
        json!({
            "source": "https://example.com/x",
            "content": {"text": "hello"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(error_code(&body), "not_found");
}

#[actix_web::test]
async fn get_unknown_job_is_404() {
    let dir = tmp_data_dir("job-404");
    let app = test_app(dir).await;
    let req = awtest::TestRequest::get()
        .uri("/v1/jobs/job-99999999")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "not_found");
}

#[actix_web::test]
async fn ingest_rejects_bad_content() {
    let dir = tmp_data_dir("ingest-400");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    // Empty source.
    let (status, _) = submit_ingest(
        &app,
        "kb",
        json!({"source": "  ", "content": {"text": "x"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Bad base64.
    let (status, body) = submit_ingest(
        &app,
        "kb",
        json!({"source": "s", "content": {"base64": "!!!not-base64!!!"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Both text and base64.
    let (status, _) = submit_ingest(
        &app,
        "kb",
        json!({"source": "s", "content": {"text": "x", "base64": "eA=="}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Empty content.
    let (status, _) =
        submit_ingest(&app, "kb", json!({"source": "s", "content": {"text": ""}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Phase 5: retrieval modes (vector / lexical / hybrid)
// ---------------------------------------------------------------------------

/// POSTs a query body and returns (status, parsed JSON).
async fn post_query(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    collection: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let req = awtest::TestRequest::post()
        .uri(&format!("/v1/collections/{collection}/query"))
        .set_json(payload)
        .to_request();
    let resp = awtest::call_service(app, req).await;
    let status = resp.status();
    let body: Value = awtest::read_body_json(resp).await;
    (status, body)
}

/// Ingests one short plain-text doc (exactly 1 chunk) and waits for the
/// job. Returns the job body.
async fn ingest_text_doc(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    collection: &str,
    source: &str,
    text: &str,
    metadata: Value,
    vectors: Option<Value>,
) -> Value {
    let mut payload = json!({
        "source": source,
        "filename": "doc.txt",
        "mime": "text/plain",
        "content": {"text": text},
        "metadata": metadata,
    });
    if let Some(v) = vectors {
        payload["vectors"] = v;
    }
    let (status, body) = submit_ingest(app, collection, payload).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let job_id = body["job_id"].as_str().unwrap().to_string();
    let job = wait_job_done(app, &job_id).await;
    assert_eq!(job["state"], "done", "error: {}", job["error"]);
    assert_eq!(
        job["affected"]["chunks"], 1,
        "fixture needs exactly 1 chunk per doc"
    );
    job
}

fn hit_sources(body: &Value) -> Vec<String> {
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["source"].as_str().unwrap().to_string())
        .collect()
}

/// Lexical-mode query shorthand for the stale-content test.
async fn lex_query(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    text: &str,
) -> (StatusCode, Value) {
    post_query(app, "kb", json!({"text": text, "k": 5, "mode": "lexical"})).await
}

#[actix_web::test]
async fn query_defaults_to_vector_mode_with_chunk_text() {
    let dir = tmp_data_dir("q5-default");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;
    ingest_text_doc(
        &app,
        "kb",
        "src://guide",
        "ownership and borrowing in rust",
        json!({"author": "roger"}),
        None,
    )
    .await;

    // No mode → Phase 4 behavior (vector), but with the Phase 5 schema.
    let (status, body) = post_query(&app, "kb", json!({"text": "rust ownership", "k": 5})).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mode"], "vector");
    let hits = body["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    let first = &hits[0];
    assert_eq!(first["rank"], 1);
    assert_eq!(first["score_kind"], "distance");
    assert_eq!(first["path"], json!(["vector"]));
    assert!(first["id"].is_string(), "vector mode keeps the vector id");
    assert!(first["chunk_id"].is_string());
    assert!(first["distance"].is_number());
    // Phase 5: hits carry chunk text + provenance. (Canonicalization
    // appends a trailing newline — that is the stored chunk text.)
    assert_eq!(first["text"], "ownership and borrowing in rust\n");
    assert_eq!(first["source"], "src://guide");
    assert_eq!(first["metadata"]["author"], "roger");
    assert!(body["ef"].is_number(), "vector mode reports ef");
    assert!(body.get("rrf_k").is_none() || body["rrf_k"].is_null());
}

#[actix_web::test]
async fn query_lexical_mode_needs_no_provider() {
    // EmbeddingKind::None: no provider at all — lexical must still work.
    let dir = tmp_data_dir("q5-lexical-noprovider");
    let app = test_app(dir).await;
    create_collection(&app, "kb", 64).await;

    // Direct document upsert (no vectors involved).
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb/documents")
        .set_json(json!({
            "source": "src://music",
            "metadata": {"author": "roger"},
            "chunks": [{"text": "the xylophone orchestra plays nightly", "char_start": 0, "char_end": 37}],
        }))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "xylophone", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mode"], "lexical");
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    let first = &hits[0];
    assert_eq!(first["score_kind"], "bm25");
    assert_eq!(first["path"], json!(["lexical"]));
    assert!(first["score"].as_f64().unwrap() > 0.0);
    assert_eq!(first["text"], "the xylophone orchestra plays nightly");
    assert_eq!(first["source"], "src://music");
    assert!(first.get("distance").is_none() || first["distance"].is_null());
    assert!(first.get("vector_id").is_none() || first["vector_id"].is_null());
    assert!(body.get("ef").is_none() || body["ef"].is_null());

    // Absent term → 200 with no hits, not an error.
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "no-such-term-zzz", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["hits"].as_array().unwrap().is_empty());
}

#[actix_web::test]
async fn query_rejects_unknown_mode_and_bad_rrf_k() {
    let dir = tmp_data_dir("q5-baddmode");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    let (status, body) =
        post_query(&app, "kb", json!({"text": "x", "k": 5, "mode": "semantic"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(error_code(&body), "validation_error");

    let (status, _) = post_query(
        &app,
        "kb",
        json!({"text": "x", "k": 5, "mode": "hybrid", "rrf_k": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The repeatable hybrid fixture: doc A wins lexically (it alone carries
/// the rare query term), doc B wins on vectors (its supplied vector equals
/// the query embedding exactly), and RRF combines them sensibly — the
/// chunk present in *both* rankings outranks the one present in only one.
#[actix_web::test]
async fn hybrid_fixture_lexical_wins_vector_wins_rrf_combines() {
    let dir = tmp_data_dir("q5-hybrid-fixture");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    // The query embedding, computed with the same deterministic stub the
    // server uses — doc B's supplied vector equals it (distance 0).
    let stub = StubEmbeddingProvider::new(64).unwrap();
    let query_vec = stub.embed(&["xylophone"]).unwrap().remove(0);
    let far_vec = vec![1.0f32; 64];

    ingest_text_doc(
        &app,
        "kb",
        "src://a",
        "the xylophone orchestra plays nightly",
        json!({"author": "roger"}),
        Some(json!([far_vec])),
    )
    .await;
    ingest_text_doc(
        &app,
        "kb",
        "src://b",
        "quantum foam dynamics in plain words",
        json!({"author": "ana"}),
        Some(json!([query_vec])),
    )
    .await;

    // Lexical-only: the rare term decides — doc A first.
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "xylophone", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(hit_sources(&body), vec!["src://a".to_string()]);

    // Vector-only: geometry decides — doc B (distance 0) first.
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "xylophone", "k": 5, "mode": "vector"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let sources = hit_sources(&body);
    assert_eq!(sources[0], "src://b");
    assert_eq!(body["hits"][0]["distance"], 0.0);

    // Hybrid: RRF — doc A is in both rankings (lex 1/61 + vec 1/62),
    // doc B only in the vector one (1/61). A outranks B.
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "xylophone", "k": 5, "mode": "hybrid"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mode"], "hybrid");
    assert_eq!(body["rrf_k"], 60.0);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["source"], "src://a");
    assert_eq!(hits[0]["score_kind"], "rrf");
    assert_eq!(hits[0]["path"], json!(["vector", "lexical"]));
    assert!(hits[0]["score"].as_f64().unwrap() > hits[1]["score"].as_f64().unwrap());
    assert_eq!(hits[1]["source"], "src://b");
    assert_eq!(hits[1]["path"], json!(["vector"]));
    // Hybrid hits carry vector evidence when they have a vector origin.
    assert_eq!(hits[1]["distance"], 0.0);
}

#[actix_web::test]
async fn metadata_filter_is_consistent_across_modes() {
    let dir = tmp_data_dir("q5-filter");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;
    ingest_text_doc(
        &app,
        "kb",
        "src://roger-doc",
        "sharedterm alpha content",
        json!({"author": "roger"}),
        None,
    )
    .await;
    ingest_text_doc(
        &app,
        "kb",
        "src://ana-doc",
        "sharedterm beta content",
        json!({"author": "ana"}),
        None,
    )
    .await;

    for mode in ["vector", "lexical", "hybrid"] {
        let (status, body) = post_query(
            &app,
            "kb",
            json!({"text": "sharedterm", "k": 5, "mode": mode, "filter": {"author": "roger"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "mode {mode}: {body}");
        let sources = hit_sources(&body);
        assert_eq!(
            sources,
            vec!["src://roger-doc".to_string()],
            "mode {mode} must honor the filter"
        );
    }

    // No filter → both docs.
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "sharedterm", "k": 5, "mode": "hybrid"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hits"].as_array().unwrap().len(), 2);
}

#[actix_web::test]
async fn updated_and_deleted_content_absent_from_lexical_results() {
    let dir = tmp_data_dir("q5-stale");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;
    ingest_text_doc(
        &app,
        "kb",
        "src://changing",
        "the old text mentions blorpt",
        json!({}),
        None,
    )
    .await;

    let (_, body) = lex_query(&app, "blorpt").await;
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);

    // Update: old term must vanish, new term must appear.
    ingest_text_doc(
        &app,
        "kb",
        "src://changing",
        "the new text mentions quux",
        json!({}),
        None,
    )
    .await;
    let (_, body) = lex_query(&app, "blorpt").await;
    assert!(
        body["hits"].as_array().unwrap().is_empty(),
        "updated-away term must be absent: {body}"
    );
    let (_, body) = lex_query(&app, "quux").await;
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);

    // Delete: nothing lexical left.
    let (_, job_body) = post_query(
        &app,
        "kb",
        json!({"text": "quux", "k": 1, "mode": "lexical"}),
    )
    .await;
    let chunk_id = job_body["hits"][0]["chunk_id"].as_str().unwrap();
    // Chunk ids look like {doc}#v{version}#c{index}; the doc id is the prefix.
    let doc_id = chunk_id.split("#v").next().unwrap();
    assert!(
        doc_id.starts_with("kb#"),
        "unexpected chunk id shape: {chunk_id}"
    );
    let enc: String = doc_id.replace('#', "%23");
    let req = awtest::TestRequest::delete()
        .uri(&format!("/v1/collections/kb/documents/{enc}"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let (_, body) = lex_query(&app, "quux").await;
    assert!(
        body["hits"].as_array().unwrap().is_empty(),
        "deleted content must be absent: {body}"
    );
}

/// Builds an app + its state on `dir` (state is returned so the test can
/// shut the runner down before reopening the data dir).
async fn build_app_with_state(
    dir: std::path::PathBuf,
) -> (
    AppState,
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
) {
    let config = test_config(dir);
    let state = AppState::with_provider(&config, Some(stub_provider())).expect("test app state");
    let app = awtest::init_service(
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(|cfg| configure_routes(cfg, &config)),
    )
    .await;
    (state, app)
}

#[actix_web::test]
async fn lexical_index_rebuilt_on_reopen() {
    let dir = tmp_data_dir("q5-reopen");

    {
        let (state, app) = build_app_with_state(dir.clone()).await;
        create_collection(&app, "kb", 64).await;
        ingest_text_doc(
            &app,
            "kb",
            "src://persist",
            "persistent wombat knowledge",
            json!({}),
            None,
        )
        .await;
        // Shut down the runner: its accept-loop task holds jobs.redb open,
        // and redb forbids opening the same file twice in one process.
        state.runner.shutdown().await;
        // state + app drop here, releasing every handle on the data dir.
    }
    // A fresh server on the same data dir rebuilds the lexical index from
    // the catalog on open — the term must still be searchable.
    let (_state, app) = build_app_with_state(dir).await;
    let (status, body) = post_query(
        &app,
        "kb",
        json!({"text": "wombat", "k": 5, "mode": "lexical"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(hit_sources(&body), vec!["src://persist".to_string()]);
}

// ---------------------------------------------------------------------------
// Phase 7 reliability: races, deep nesting, duplicate ingest, URL-as-label
// ---------------------------------------------------------------------------

/// Barrier-synchronized concurrent creates of the same collection id
/// (the Phase 3 PLAUSIBLE "create-vs-create" race): `Registry::create`
/// holds the handles mutex across check+create, so exactly one request
/// wins with 201 and the rest get 409 — no panic, no 500, no corruption.
/// The eight requests are in flight at once and interleave at await points.
#[actix_web::test]
async fn concurrent_create_same_id_single_winner() {
    let app = test_app(tmp_data_dir("race-create")).await;
    let mk = || {
        let req = awtest::TestRequest::post()
            .uri("/v1/collections")
            .set_json(json!({"id": "race", "dimension": 16, "metric": "cosine"}))
            .to_request();
        awtest::call_service(&app, req)
    };
    let (r1, r2, r3, r4, r5, r6, r7, r8) =
        tokio::join!(mk(), mk(), mk(), mk(), mk(), mk(), mk(), mk());
    let (mut created, mut conflict) = (0, 0);
    for r in [r1, r2, r3, r4, r5, r6, r7, r8] {
        match r.status() {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => conflict += 1,
            s => panic!("unexpected status {s} in create race"),
        }
    }
    assert_eq!(created, 1, "exactly one create must win");
    assert_eq!(conflict, 7, "losers must get 409");
}

/// Concurrent delete-vs-insert on the same collection (the Phase 3 PLAUSIBLE
/// "delete-vs-write" race): the loser holds its `Arc` and finishes against
/// the unlinked directory; nothing panics, nothing hangs, and the final
/// state is coherent (the collection is gone — delete always unlinks last).
#[actix_web::test]
async fn concurrent_delete_and_insert_no_panic() {
    let app = test_app(tmp_data_dir("race-delete")).await;
    create_collection(&app, "race", 16).await;

    let del = async {
        let req = awtest::TestRequest::delete()
            .uri("/v1/collections/race")
            .to_request();
        awtest::call_service(&app, req).await.status()
    };
    let write = async {
        let vectors: Vec<Value> = (0..10)
            .map(|i| json!({"id": format!("w-{i}"), "values": vec![0.5f32; 16]}))
            .collect();
        let req = awtest::TestRequest::post()
            .uri("/v1/collections/race/vectors")
            .set_json(json!({"vectors": vectors}))
            .to_request();
        awtest::call_service(&app, req).await.status()
    };
    let (del_status, write_status) = tokio::join!(del, write);
    assert!(
        matches!(del_status, StatusCode::NO_CONTENT | StatusCode::NOT_FOUND),
        "delete status: {del_status}"
    );
    assert!(
        matches!(
            write_status,
            StatusCode::CREATED | StatusCode::NOT_FOUND | StatusCode::INTERNAL_SERVER_ERROR
        ),
        "write status: {write_status}"
    );

    // Final state: the collection is gone, and the server is still healthy.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections/race/stats")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let req = awtest::TestRequest::get().uri("/healthz").to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 200-deep nested metadata: `serde_json`'s default recursion limit (128)
/// must reject the payload with 400, not exhaust the stack.
#[actix_web::test]
async fn deeply_nested_metadata_rejected() {
    let app = test_app(tmp_data_dir("deep-nest")).await;
    create_collection(&app, "kb", 16).await;

    let mut nested = String::from("{\"leaf\": true}");
    for _ in 0..200 {
        nested = format!("{{\"a\": {nested}}}");
    }
    let body = format!(
        "{{\"source\": \"src://deep\", \"content\": {{\"text\": \"hello\"}}, \"metadata\": {nested}}}"
    );
    let req = awtest::TestRequest::put()
        .uri("/v1/collections/kb/documents")
        .insert_header(("content-type", "application/json"))
        .set_payload(body)
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "200-deep nesting must be 400"
    );
}

/// Two concurrent submits with the same idempotency key: one job is
/// created, both submitters get that job id (no duplicate work).
#[actix_web::test]
async fn duplicate_ingest_submission_dedups() {
    let dir = tmp_data_dir("ingest-dedup");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    let payload = || {
        json!({
            "source": "src://dedup",
            "content": {"text": "deduplicated content here"},
            "idempotency_key": "dup-key-1",
        })
    };
    let (s1, b1) = submit_ingest(&app, "kb", payload()).await;
    let (s2, b2) = submit_ingest(&app, "kb", payload()).await;
    assert_eq!(s1, StatusCode::ACCEPTED, "body: {b1}");
    assert_eq!(s2, StatusCode::ACCEPTED, "body: {b2}");
    assert_eq!(
        b1["job_id"], b2["job_id"],
        "duplicate submit must return the same job"
    );

    let job = wait_job_done(&app, b1["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "error: {}", job["error"]);

    let req = awtest::TestRequest::get()
        .uri("/v1/collections/kb/jobs")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(
        body["count"], 1,
        "exactly one job row for the duplicated submit"
    );
}

/// `source` is an opaque label, never fetched: a link-local URL as source
/// completes offline using the supplied content bytes. (The vendored
/// markit's HTTP fetcher is stubbed to fail loudly — there is no code path
/// that performs a request, so there is no SSRF surface to test beyond
/// this.)
#[actix_web::test]
async fn url_source_is_label_never_fetched() {
    let dir = tmp_data_dir("url-label");
    let app = test_app_with_provider(dir, Some(stub_provider())).await;
    create_collection(&app, "kb", 64).await;

    let payload = json!({
        "source": "http://169.254.169.254/latest/meta-data/",
        "content": {"text": "the quick brown fox jumps over the lazy dog"},
    });
    let (status, body) = submit_ingest(&app, "kb", payload).await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let job = wait_job_done(&app, body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "error: {}", job["error"]);

    // The content bytes were ingested; nothing was fetched.
    let req = awtest::TestRequest::post()
        .uri("/v1/collections/kb/query")
        .set_json(json!({"text": "quick brown fox", "k": 5}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(resp).await;
    let hits = body["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "body: {body}");
    assert_eq!(
        hits[0]["source"],
        "http://169.254.169.254/latest/meta-data/"
    );
}

// ---------------------------------------------------------------------------
// Phase 7: bearer-token auth + rate limiting
// ---------------------------------------------------------------------------

/// With `POLYVEC_API_KEY` set, every `/v1` route requires
/// `Authorization: Bearer <key>`: missing/wrong → 401 + `WWW-Authenticate`,
/// correct → through. `/healthz` and `/readyz` stay open (probes must not
/// need credentials).
#[actix_web::test]
async fn auth_required_when_key_set() {
    let app = test_app_with_security(tmp_data_dir("auth"), Some("s3cret-key"), 0).await;

    // No credential → 401 with WWW-Authenticate, and the key leaks nowhere.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers().contains_key("www-authenticate"),
        "401 must carry WWW-Authenticate"
    );
    let body: Value = awtest::read_body_json(resp).await;
    assert_eq!(error_code(&body), "unauthorized");
    assert!(
        !body.to_string().contains("s3cret-key"),
        "key must not leak into the 401 body"
    );

    // Wrong credential → 401 as well.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .insert_header(("authorization", "Bearer wrong-key"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Correct credential → through, including a mutating route.
    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .insert_header(("authorization", "Bearer s3cret-key"))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let req = awtest::TestRequest::post()
        .uri("/v1/collections")
        .insert_header(("authorization", "Bearer s3cret-key"))
        .set_json(json!({"id": "secure", "dimension": 16, "metric": "cosine"}))
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Probes stay open without a credential.
    for uri in ["/healthz", "/readyz"] {
        let req = awtest::TestRequest::get().uri(uri).to_request();
        let resp = awtest::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::OK, "{uri} must stay open");
    }
}

/// Without a key, behavior is exactly the Phase 3–6 default: no 401s.
#[actix_web::test]
async fn auth_disabled_without_key() {
    let app = test_app(tmp_data_dir("auth-off")).await;
    let req = awtest::TestRequest::get()
        .uri("/v1/collections")
        .to_request();
    let resp = awtest::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Rate limiting: with 2 rps (burst 16) the 17th+ rapid request gets 429
/// with a `Retry-After` header and the shared error shape.
#[actix_web::test]
async fn rate_limit_returns_429_with_retry_after() {
    let app = test_app_with_security(tmp_data_dir("ratelimit"), None, 2).await;
    let mut too_many = 0;
    let mut saw_retry_after = false;
    for _ in 0..24 {
        let req = awtest::TestRequest::get()
            .uri("/v1/collections")
            .to_request();
        let resp = awtest::call_service(&app, req).await;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::TOO_MANY_REQUESTS => {
                too_many += 1;
                saw_retry_after |= resp.headers().contains_key("retry-after");
                let body: Value = awtest::read_body_json(resp).await;
                assert_eq!(error_code(&body), "rate_limited");
            }
            s => panic!("unexpected status {s} under rate limiting"),
        }
    }
    assert!(
        too_many >= 1,
        "burst+1 rapid requests must trip the limiter"
    );
    assert!(saw_retry_after, "429 must carry Retry-After");
}

/// The API key is redacted from `Debug`: a logged config can never leak it.
#[test]
fn config_debug_redacts_api_key() {
    let config = ServerConfig::from_env_in(&[
        ("POLYVEC_API_KEY", "s3cret-key"),
        ("POLYVEC_RATE_LIMIT_RPS", "10"),
    ])
    .expect("config parses");
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("s3cret-key"),
        "Debug must not leak the key: {rendered}"
    );
    assert!(
        rendered.contains("<redacted>"),
        "Debug must mark the key redacted: {rendered}"
    );
    assert_eq!(config.rate_limit_rps, 10);
}

/// Env parsing for the security knobs: defaults, empty-means-unset, and
/// malformed values.
#[test]
fn config_parses_security_knobs() {
    // Defaults: no key, 1000 rps.
    let config = ServerConfig::from_env_in(&[]).expect("defaults parse");
    assert_eq!(config.api_key, None);
    assert_eq!(config.rate_limit_rps, 1000);

    // Key is trimmed; empty counts as unset.
    let config = ServerConfig::from_env_in(&[("POLYVEC_API_KEY", "  k  ")]).unwrap();
    assert_eq!(config.api_key.as_deref(), Some("k"));
    let config = ServerConfig::from_env_in(&[("POLYVEC_API_KEY", "   ")]).unwrap();
    assert_eq!(config.api_key, None);

    // 0 disables rate limiting.
    let config = ServerConfig::from_env_in(&[("POLYVEC_RATE_LIMIT_RPS", "0")]).unwrap();
    assert_eq!(config.rate_limit_rps, 0);

    // Malformed values are hard errors, not silent defaults.
    assert!(ServerConfig::from_env_in(&[("POLYVEC_RATE_LIMIT_RPS", "fast")]).is_err());
}
