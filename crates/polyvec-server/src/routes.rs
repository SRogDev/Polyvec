//! HTTP handlers: every route in the `/v1` API plus `/healthz` and `/readyz`.
//!
//! ## Blocking strategy
//!
//! `CollectionStore` is a synchronous API and HNSW insert/search are
//! CPU-bound, so **no handler touches a store on an async worker thread**.
//! Each handler moves its work into [`web::block`], which runs it on Tokio's
//! blocking thread pool. The per-collection [`Mutex`] is locked only inside
//! the blocking closure — never across `.await` — so a slow insert or search
//! cannot stall request workers (or `/healthz`).
//!
//! Logging discipline: ids and counts are logged; vector *values* and
//! request bodies never are.

use actix_web::{HttpResponse, Responder, web};
use polyvec_core::{CollectionId, Document, DocumentId, Error as CoreError, Vector, VectorId};
use polyvec_storage::CollectionStore;
use std::sync::Arc;

use crate::error::ApiError;
use crate::registry::{SharedStore, lock_store};
use crate::schemas::{
    CollectionStatsResponse, CollectionSummary, CreateCollectionRequest, DocumentResponse,
    DocumentUpsertRequest, DocumentUpsertResponse, FlushResponse, HealthResponse, IngestRequestDto,
    IngestResponseDto, JobDto, JobListResponse, ListCollectionsResponse, QueryHitDto,
    QueryRequestDto, QueryResponseDto, ReadyResponse, SearchHitDto, SearchRequest, SearchResponse,
    StatsDto, UpdateVectorRequest, VectorItemDto, VectorResponse, VectorsUpsertRequest,
    VectorsUpsertResponse, chunks_from, collection_config_from, content_hash_from,
    metadata_from_json, parse_path_id,
};
use crate::{AppState, MAX_K, MAX_SEARCH_EF, MIN_SEARCH_EF};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Runs `f` on the blocking pool with the collection's store locked.
///
/// The closure receives `&mut CollectionStore` and runs entirely off the
/// async workers. `f` must be `Send + 'static`, like everything `web::block`
/// requires.
async fn with_store<T, F>(
    registry: Arc<crate::registry::Registry>,
    id: CollectionId,
    f: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&mut CollectionStore) -> Result<T, CoreError> + Send + 'static,
{
    let store: SharedStore = web::block(move || registry.get_or_open(&id))
        .await?
        .map_err(ApiError::from)?;
    web::block(move || {
        let mut guard = lock_store(&store)?;
        f(&mut guard)
    })
    .await?
    .map_err(ApiError::from)
}

/// Validates the `{id}` path segment as a collection slug.
fn collection_id(raw: &str) -> Result<CollectionId, ApiError> {
    parse_path_id::<CollectionId>(raw, "collection")
}

// ---------------------------------------------------------------------------
// health / readiness
// ---------------------------------------------------------------------------

/// `GET /healthz` — liveness. Always 200 when the process answers.
async fn healthz() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse {
        status: "ok",
        service: "polyvec",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// `GET /readyz` — readiness: the data dir must exist and be a directory.
/// 503 otherwise (the binary creates it on startup; a 503 means the
/// operator should look at the mount/permissions).
async fn readyz(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let data_dir = state.registry.data_dir().to_path_buf();
    let ready = web::block(move || std::fs::metadata(&data_dir).is_ok_and(|m| m.is_dir()))
        .await
        .map_err(ApiError::from)?;
    if ready {
        Ok(HttpResponse::Ok().json(ReadyResponse { status: "ready" }))
    } else {
        Err(ApiError::unavailable("data directory is not accessible"))
    }
}

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

/// `POST /v1/collections` — creates a collection. 201 on success.
async fn create_collection(
    state: web::Data<AppState>,
    body: web::Json<CreateCollectionRequest>,
) -> Result<impl Responder, ApiError> {
    let config = collection_config_from(&body.into_inner())?;
    let id = config.name.as_str().to_string();
    let summary = CollectionSummary::from_config(&config);
    web::block(move || state.registry.create(config))
        .await?
        .map_err(ApiError::from)?;
    log::info!("created collection '{id}'");
    Ok(HttpResponse::Created().json(summary))
}

/// `GET /v1/collections` — lists collections without opening their indexes.
async fn list_collections(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let configs = web::block(move || state.registry.list().map_err(ApiError::from)).await??;
    let collections: Vec<CollectionSummary> =
        configs.iter().map(CollectionSummary::from_config).collect();
    let count = collections.len();
    Ok(HttpResponse::Ok().json(ListCollectionsResponse { collections, count }))
}

/// `GET /v1/collections/{id}` — collection summary (opens the store lazily).
async fn get_collection(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let summary = with_store(Arc::clone(&state.registry), id, |store| {
        Ok(CollectionSummary::from_config(store.config()))
    })
    .await?;
    Ok(HttpResponse::Ok().json(summary))
}

/// `DELETE /v1/collections/{id}` — removes the collection and its data. 204.
async fn delete_collection(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let label = id.as_str().to_string();
    web::block(move || state.registry.remove(&id).map_err(ApiError::from)).await??;
    log::info!("deleted collection '{label}'");
    Ok(HttpResponse::NoContent().finish())
}

/// `GET /v1/collections/{id}/stats` — index state + aggregate counts.
async fn collection_stats(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let label = id.as_str().to_string();
    let response = with_store(Arc::clone(&state.registry), id, move |store| {
        Ok(CollectionStatsResponse {
            id: label.clone(),
            index_state: store.index_state(),
            stats: StatsDto::from_stats(&store.stats()),
        })
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

/// `POST /v1/collections/{id}/flush` — checkpoints the index to disk.
async fn flush_collection(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let label = id.as_str().to_string();
    let generation = with_store(Arc::clone(&state.registry), id, |store| {
        store.flush()?;
        Ok(store.stats().generation)
    })
    .await?;
    log::info!("flushed collection '{label}' (generation {generation})");
    Ok(HttpResponse::Ok().json(FlushResponse {
        id: label,
        flushed: true,
        generation,
    }))
}

// ---------------------------------------------------------------------------
// vectors
// ---------------------------------------------------------------------------

/// `POST /v1/collections/{id}/vectors` — inserts one vector or a batch
/// (atomic). 201 with the inserted ids.
async fn insert_vectors(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<VectorsUpsertRequest>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let items: Vec<VectorItemDto> = match body.into_inner() {
        VectorsUpsertRequest::Batch { vectors } => vectors,
        VectorsUpsertRequest::Single(item) => vec![item],
    };
    if items.is_empty() {
        return Err(ApiError::validation(
            "vectors: at least one vector is required",
        ));
    }
    let label = id.as_str().to_string();
    let n = items.len();
    let ids = with_store(Arc::clone(&state.registry), id, move |store| {
        let dim = store.config().dimension;
        let mut vectors = Vec::with_capacity(items.len());
        let mut ids = Vec::with_capacity(items.len());
        for item in &items {
            let vector_id = VectorId::parse(&item.id)?;
            let vector = Vector::new(vector_id.clone(), item.values.clone(), dim)?;
            ids.push(vector_id.as_str().to_string());
            vectors.push(vector);
        }
        store.insert_vectors(vectors)?;
        Ok(ids)
    })
    .await?;
    log::info!("inserted {n} vector(s) into '{label}'");
    Ok(HttpResponse::Created().json(VectorsUpsertResponse { inserted: n, ids }))
}

/// `POST /v1/collections/{id}/vectors/search` — top-k ANN search.
async fn search_vectors(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<SearchRequest>,
) -> Result<impl Responder, ApiError> {
    let id = collection_id(&path.into_inner())?;
    let req = body.into_inner();
    if req.k == 0 || req.k > MAX_K {
        return Err(ApiError::validation(format!(
            "k must be 1..={MAX_K}, got {}",
            req.k
        )));
    }
    if let Some(ef) = req.ef {
        if !(MIN_SEARCH_EF..=MAX_SEARCH_EF).contains(&ef) {
            return Err(ApiError::validation(format!(
                "ef must be {MIN_SEARCH_EF}..={MAX_SEARCH_EF}, got {ef}"
            )));
        }
    }
    let k = req.k;
    let response = with_store(Arc::clone(&state.registry), id, move |store| {
        // Default search width comes from the collection's own tuning.
        let ef = req.ef.unwrap_or(store.config().hnsw.ef_search);
        if !(MIN_SEARCH_EF..=MAX_SEARCH_EF).contains(&ef) {
            return Err(CoreError::Validation {
                detail: format!(
                    "collection ef_search {ef} is outside {MIN_SEARCH_EF}..={MAX_SEARCH_EF}"
                ),
            });
        }
        let hits = store
            .search(&req.vector, k, ef)?
            .into_iter()
            .map(|hit| SearchHitDto {
                id: hit.vector_id.as_str().to_string(),
                distance: hit.distance,
            })
            .collect();
        Ok(SearchResponse { hits, k, ef })
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

/// `GET /v1/collections/{id}/vectors/{vector_id}` — reads a stored vector.
async fn get_vector(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (collection_raw, vector_raw) = path.into_inner();
    let id = collection_id(&collection_raw)?;
    let vector_id = parse_path_id::<VectorId>(&vector_raw, "vector")?;
    let response = with_store(Arc::clone(&state.registry), id, move |store| {
        let stored = store.get_vector(&vector_id)?;
        Ok(VectorResponse::from_stored(&vector_id, &stored))
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

/// `PUT /v1/collections/{id}/vectors/{vector_id}` — replaces a vector's
/// values (same id). 200 with the updated vector.
async fn update_vector(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateVectorRequest>,
) -> Result<impl Responder, ApiError> {
    let (collection_raw, vector_raw) = path.into_inner();
    let id = collection_id(&collection_raw)?;
    let vector_id = parse_path_id::<VectorId>(&vector_raw, "vector")?;
    let values = body.into_inner().values;
    let response = with_store(Arc::clone(&state.registry), id, move |store| {
        let dim = store.config().dimension;
        let vector = Vector::new(vector_id.clone(), values, dim)?;
        store.update_vector(vector)?;
        let stored = store.get_vector(&vector_id)?;
        Ok(VectorResponse::from_stored(&vector_id, &stored))
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

/// `DELETE /v1/collections/{id}/vectors/{vector_id}` — deletes a vector.
/// Idempotent: deleting a deleted vector is 204; deleting an unknown id is 404.
async fn delete_vector(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (collection_raw, vector_raw) = path.into_inner();
    let id = collection_id(&collection_raw)?;
    let vector_id = parse_path_id::<VectorId>(&vector_raw, "vector")?;
    with_store(Arc::clone(&state.registry), id, move |store| {
        store.delete_vector(&vector_id)?;
        Ok(())
    })
    .await?;
    Ok(HttpResponse::NoContent().finish())
}

// ---------------------------------------------------------------------------
// documents
// ---------------------------------------------------------------------------

/// `PUT /v1/collections/{id}/documents` — upserts a document (keyed by
/// `source`) with its chunks. Chunk/vector ids are derived server-side.
/// 200 with the derived ids and version.
async fn put_document(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<DocumentUpsertRequest>,
) -> Result<impl Responder, ApiError> {
    let collection_id = collection_id(&path.into_inner())?;
    let req = body.into_inner();
    if req.source.is_empty() {
        return Err(ApiError::validation("source must not be empty"));
    }
    if req.chunks.is_empty() {
        return Err(ApiError::validation(
            "chunks: at least one chunk is required",
        ));
    }
    let document_id = DocumentId::for_source(&collection_id, &req.source);
    let texts: Vec<&str> = req.chunks.iter().map(|c| c.text.as_str()).collect();
    let content_hash = content_hash_from(req.content_hash.as_deref(), &texts)?;
    let metadata = metadata_from_json(&req.metadata)?;
    let chunks = chunks_from(&document_id, &req.chunks)?;
    let chunk_ids: Vec<String> = chunks.iter().map(|c| c.id.as_str().to_string()).collect();
    // The id is deterministic (collection + source); keep a copy for the
    // response before the values move into the blocking closure.
    let response_id = document_id.as_str().to_string();
    let label = collection_id.as_str().to_string();
    let n_chunks = chunks.len();
    let lookup_id = collection_id.clone();
    let version = with_store(Arc::clone(&state.registry), lookup_id, move |store| {
        let doc = Document::new(
            document_id.clone(),
            collection_id.clone(),
            req.source.clone(),
            content_hash,
            metadata,
        );
        store.put_document(&doc, &chunks)?;
        let (stored, _) = store.get_document(&document_id)?;
        Ok(stored.version)
    })
    .await?;
    log::info!("upserted document in '{label}' ({n_chunks} chunks, version {version})");
    Ok(HttpResponse::Ok().json(DocumentUpsertResponse {
        id: response_id,
        version,
        chunk_ids,
    }))
}

/// `GET /v1/collections/{id}/documents/{document_id}` — document + chunks.
async fn get_document(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (collection_raw, document_raw) = path.into_inner();
    let id = collection_id(&collection_raw)?;
    let document_id = parse_path_id::<DocumentId>(&document_raw, "document")?;
    let response = with_store(Arc::clone(&state.registry), id, move |store| {
        let (document, chunks) = store.get_document(&document_id)?;
        Ok(DocumentResponse::from_stored(&document, &chunks))
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}

/// `DELETE /v1/collections/{id}/documents/{document_id}` — deletes the
/// document, its chunks, and their vectors (cascade). 204.
async fn delete_document(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (collection_raw, document_raw) = path.into_inner();
    let id = collection_id(&collection_raw)?;
    let document_id = parse_path_id::<DocumentId>(&document_raw, "document")?;
    with_store(Arc::clone(&state.registry), id, move |store| {
        store.delete_document(&document_id)?;
        Ok(())
    })
    .await?;
    Ok(HttpResponse::NoContent().finish())
}

// ---------------------------------------------------------------------------
// route registration
// ---------------------------------------------------------------------------

/// Registers every route on `cfg`. `json_limit` sizes the JSON extractor
/// (bodies beyond it get 413 with the structured error shape).
pub fn configure_routes(cfg: &mut web::ServiceConfig, json_limit: usize) {
    cfg.app_data(
        web::JsonConfig::default()
            .limit(json_limit)
            .error_handler(crate::error::json_error_handler),
    )
    .route("/healthz", web::get().to(healthz))
    .route("/readyz", web::get().to(readyz))
    .service(
        web::scope("/v1")
            .route("/collections", web::post().to(create_collection))
            .route("/collections", web::get().to(list_collections))
            .route("/collections/{id}", web::get().to(get_collection))
            .route("/collections/{id}", web::delete().to(delete_collection))
            .route("/collections/{id}/stats", web::get().to(collection_stats))
            .route("/collections/{id}/flush", web::post().to(flush_collection))
            .route("/collections/{id}/vectors", web::post().to(insert_vectors))
            .route(
                "/collections/{id}/vectors/search",
                web::post().to(search_vectors),
            )
            .route(
                "/collections/{id}/vectors/{vector_id}",
                web::get().to(get_vector),
            )
            .route(
                "/collections/{id}/vectors/{vector_id}",
                web::put().to(update_vector),
            )
            .route(
                "/collections/{id}/vectors/{vector_id}",
                web::delete().to(delete_vector),
            )
            .route("/collections/{id}/documents", web::put().to(put_document))
            .route(
                "/collections/{id}/documents/{document_id}",
                web::get().to(get_document),
            )
            .route(
                "/collections/{id}/documents/{document_id}",
                web::delete().to(delete_document),
            )
            .route(
                "/collections/{id}/ingest",
                web::post().to(ingest_collection),
            )
            .route("/collections/{id}/jobs", web::get().to(list_jobs))
            .route("/collections/{id}/query", web::post().to(query_text))
            .route("/jobs/{job_id}", web::get().to(get_job)),
    );
}

// ---------------------------------------------------------------------------
// ingestion jobs
// ---------------------------------------------------------------------------

/// `POST /v1/collections/{id}/ingest` — submits an ingest job.
///
/// Returns `202 Accepted` with the job id; the pipeline runs in the
/// background. `404` for unknown collections, `429` (+ `Retry-After`) when
/// the job queue is full, `501` when text ingest needs an embedding
/// provider but none is configured.
async fn ingest_collection(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<IngestRequestDto>,
) -> Result<impl Responder, ApiError> {
    let collection_id = collection_id(path.into_inner().as_str())?;
    let dto = body.into_inner();
    if dto.source.trim().is_empty() {
        return Err(ApiError::validation("source must not be empty"));
    }
    let content = dto.content.into_bytes()?;
    if content.is_empty() {
        return Err(ApiError::validation("content must not be empty"));
    }
    let metadata = metadata_from_json(&dto.metadata)?;
    // Fail fast: text ingest without a provider and without supplied
    // vectors can never succeed — refuse with 501 rather than queueing a
    // doomed job.
    if dto.vectors.is_none() && state.provider.is_none() {
        return Err(ApiError::new(
            actix_web::http::StatusCode::NOT_IMPLEMENTED,
            "unsupported",
            "no embedding provider configured: set POLYVEC_EMBEDDING_PROVIDER=stub|remote, or supply vectors",
        ));
    }

    let runner = state.runner.clone();
    let registry = Arc::clone(&state.registry);
    let req = polyvec_ingestion::IngestRequest {
        collection: collection_id.clone(),
        source: dto.source,
        filename: dto.filename,
        mime: dto.mime,
        content,
        metadata,
        vectors: dto.vectors,
        idempotency_key: dto.idempotency_key,
        max_attempts: None,
    };
    // The collection must exist (404 here, not a failed job); the submit
    // itself is a quick redb transaction. Both run in `web::block`.
    let job_id = web::block(move || {
        registry
            .get_or_open(&collection_id)
            .map_err(ApiError::from)?;
        runner.try_submit_ingest(req).map_err(ApiError::from)
    })
    .await?
    .map_err(|e: ApiError| e)?;
    Ok(HttpResponse::Accepted().json(IngestResponseDto {
        job_id: job_id.to_string(),
        state: "pending".to_string(),
    }))
}

/// `GET /v1/jobs/{job_id}` — inspects one job's state and error detail.
async fn get_job(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let raw = path.into_inner();
    if raw.trim().is_empty() {
        return Err(ApiError::validation("job id must not be empty"));
    }
    let job_id = polyvec_ingestion::JobId::new(raw.clone());
    let runner = state.runner.clone();
    let job = web::block(move || runner.get_job(&job_id).map_err(ApiError::from))
        .await?
        .map_err(|e: ApiError| e)?;
    match job {
        Some(job) => Ok(HttpResponse::Ok().json(JobDto::from_job(&job))),
        None => Err(ApiError::new(
            actix_web::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("not found: job '{raw}'"),
        )),
    }
}

/// Query params for `GET /v1/collections/{id}/jobs`.
#[derive(Debug, serde::Deserialize)]
struct JobsQuery {
    limit: Option<usize>,
}

/// `GET /v1/collections/{id}/jobs` — lists recent jobs for a collection.
async fn list_jobs(
    state: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<JobsQuery>,
) -> Result<impl Responder, ApiError> {
    let collection_id = collection_id(path.into_inner().as_str())?;
    let limit = query.limit.unwrap_or(100).min(1000);
    let runner = state.runner.clone();
    let jobs = web::block(move || {
        runner
            .list_jobs(&collection_id, limit)
            .map_err(ApiError::from)
    })
    .await?
    .map_err(|e: ApiError| e)?;
    let dtos: Vec<JobDto> = jobs.iter().map(JobDto::from_job).collect();
    Ok(HttpResponse::Ok().json(JobListResponse {
        count: dtos.len(),
        jobs: dtos,
    }))
}

/// `POST /v1/collections/{id}/query` — embeds text with the configured
/// provider and searches. `501` when no provider is configured (text query
/// has no supplied-vector path).
async fn query_text(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<QueryRequestDto>,
) -> Result<impl Responder, ApiError> {
    let collection_id = collection_id(path.into_inner().as_str())?;
    let dto = body.into_inner();
    if dto.text.trim().is_empty() {
        return Err(ApiError::validation("text must not be empty"));
    }
    if dto.k == 0 || dto.k > MAX_K {
        return Err(ApiError::validation(format!("k must be in 1..={MAX_K}")));
    }
    let provider = state.provider.clone().ok_or_else(|| {
        ApiError::new(
            actix_web::http::StatusCode::NOT_IMPLEMENTED,
            "unsupported",
            "text query needs an embedding provider: set POLYVEC_EMBEDDING_PROVIDER=stub|remote",
        )
    })?;
    // The remote adapter is blocking HTTP: embed on the blocking pool.
    let text = dto.text.clone();
    let mut vectors = web::block(move || provider.embed(&[text.as_str()]).map_err(ApiError::from))
        .await?
        .map_err(|e: ApiError| e)?;
    let query_vector = vectors.pop().expect("embed returns one vector per text");
    let ef = dto.ef.unwrap_or(0);
    if ef != 0 && !(MIN_SEARCH_EF..=MAX_SEARCH_EF).contains(&ef) {
        return Err(ApiError::validation(format!(
            "ef must be in {MIN_SEARCH_EF}..={MAX_SEARCH_EF}"
        )));
    }
    let k = dto.k;
    let response = with_store(Arc::clone(&state.registry), collection_id, move |store| {
        let ef = if ef == 0 {
            store.config().hnsw.ef_search
        } else {
            ef
        };
        let hits = store.search(&query_vector, k, ef)?;
        let dtos = hits
            .into_iter()
            .map(|hit| QueryHitDto {
                id: hit.vector_id.to_string(),
                distance: hit.distance,
                chunk_id: hit.vector_id.chunk_id().map(|c| c.to_string()),
            })
            .collect();
        Ok(QueryResponseDto { hits: dtos, k, ef })
    })
    .await?;
    Ok(HttpResponse::Ok().json(response))
}
