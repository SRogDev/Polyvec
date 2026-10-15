//! API request/response schemas (serde DTOs).
//!
//! These are the wire shapes only — every value is re-validated through the
//! `polyvec-core` domain constructors before it touches storage. The DTOs
//! never reimplement validation rules.

use std::collections::HashMap;

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, CollectionLifecycle, ContentHash, DistanceMetric,
    Document, DocumentId, HnswParams, IndexState, Metadata, MetadataValue, ModelTag, Provenance,
    VectorId,
};
use polyvec_storage::{Stats, StoredVector};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

// ---------------------------------------------------------------------------
// collections
// ---------------------------------------------------------------------------

/// `POST /v1/collections` body.
#[derive(Debug, Deserialize)]
pub struct CreateCollectionRequest {
    /// Collection slug: `[a-z0-9-_]`, 1–64 chars (validated by `CollectionId`).
    pub id: String,
    /// Vector dimension, 1–4096.
    pub dimension: u32,
    /// `"l2"` or `"cosine"` (case-insensitive; `"euclidean"` accepted too).
    pub metric: String,
    /// Embedding-model tag, if the vectors' origin is known.
    #[serde(default)]
    pub model: Option<ModelTagDto>,
    /// HNSW tuning; defaults are used for omitted fields.
    #[serde(default)]
    pub hnsw: Option<HnswParamsDto>,
}

/// Wire form of [`ModelTag`].
#[derive(Debug, Deserialize)]
pub struct ModelTagDto {
    /// Model name, e.g. `"bge-m3"`.
    pub name: String,
    /// Model version, e.g. `"1.0"`.
    pub version: String,
}

/// Wire form of [`HnswParams`] (all fields required when present).
#[derive(Debug, Deserialize)]
pub struct HnswParamsDto {
    /// Max neighbors per node per layer, 4–64.
    pub m: usize,
    /// Search width during construction, ≥ 16.
    pub ef_construction: usize,
    /// Search width at query time, ≥ 8.
    pub ef_search: usize,
    /// Expected element count (allocation hint), ≥ 1.
    pub max_elements: usize,
}

/// Collection summary returned by create/get/list.
#[derive(Debug, Serialize)]
pub struct CollectionSummary {
    /// Collection slug.
    pub id: String,
    /// Vector dimension.
    pub dimension: u32,
    /// `"l2"` or `"cosine"`.
    pub metric: String,
    /// Embedding-model tag, if set.
    pub model: Option<ModelTag>,
    /// Effective HNSW parameters.
    pub hnsw: HnswParams,
    /// Lifecycle (`"active"` for live collections).
    pub lifecycle: CollectionLifecycle,
}

impl CollectionSummary {
    /// Builds the summary from a validated config.
    #[must_use]
    pub fn from_config(config: &CollectionConfig) -> Self {
        Self {
            id: config.name.as_str().to_string(),
            dimension: config.dimension,
            metric: config.metric.to_string(),
            model: config.model.clone(),
            hnsw: config.hnsw.clone(),
            lifecycle: CollectionLifecycle::Active,
        }
    }
}

/// `GET /v1/collections` response.
#[derive(Debug, Serialize)]
pub struct ListCollectionsResponse {
    /// Collection summaries, sorted by id.
    pub collections: Vec<CollectionSummary>,
    /// Number of collections.
    pub count: usize,
}

// ---------------------------------------------------------------------------
// vectors
// ---------------------------------------------------------------------------

/// One vector in an upsert body.
#[derive(Debug, Deserialize)]
pub struct VectorItemDto {
    /// Vector id (1–256 chars, no control characters).
    pub id: String,
    /// Embedding components; must match the collection dimension and be finite.
    pub values: Vec<f32>,
}

/// `POST /v1/collections/{id}/vectors` body: a single vector object or a
/// `{"vectors": [...]}` batch.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum VectorsUpsertRequest {
    /// Batch form.
    Batch {
        /// Vectors to insert (atomic: all or nothing).
        vectors: Vec<VectorItemDto>,
    },
    /// Single-vector form.
    Single(VectorItemDto),
}

/// Upsert response.
#[derive(Debug, Serialize)]
pub struct VectorsUpsertResponse {
    /// How many vectors were inserted.
    pub inserted: usize,
    /// Inserted ids, in request order.
    pub ids: Vec<String>,
}

/// `PUT /v1/collections/{id}/vectors/{vector_id}` body.
#[derive(Debug, Deserialize)]
pub struct UpdateVectorRequest {
    /// New embedding components (same dimension, finite).
    pub values: Vec<f32>,
}

/// A stored vector.
#[derive(Debug, Serialize)]
pub struct VectorResponse {
    /// Vector id.
    pub id: String,
    /// Embedding components.
    pub values: Vec<f32>,
    /// Owning chunk, if the vector was attached to one.
    pub chunk_id: Option<String>,
}

impl VectorResponse {
    /// Builds the response from a stored vector.
    #[must_use]
    pub fn from_stored(id: &VectorId, stored: &StoredVector) -> Self {
        Self {
            id: id.as_str().to_string(),
            values: stored.values.clone(),
            chunk_id: stored.chunk_id.as_ref().map(|c| c.as_str().to_string()),
        }
    }
}

/// `POST /v1/collections/{id}/vectors/search` body.
#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    /// Query embedding (collection dimension, finite components).
    pub vector: Vec<f32>,
    /// Neighbors to return, 1–1000.
    pub k: usize,
    /// Search width override, 8–10000. Defaults to the collection's
    /// `hnsw.ef_search` when omitted.
    #[serde(default)]
    pub ef: Option<usize>,
}

/// One search hit.
#[derive(Debug, Serialize)]
pub struct SearchHitDto {
    /// Vector id.
    pub id: String,
    /// Distance under the collection's metric (smaller is closer).
    pub distance: f32,
}

/// Search response.
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    /// Hits ordered by increasing distance.
    pub hits: Vec<SearchHitDto>,
    /// Echo of the requested `k`.
    pub k: usize,
    /// Effective search width used.
    pub ef: usize,
}

// ---------------------------------------------------------------------------
// documents
// ---------------------------------------------------------------------------

/// One chunk in a document upsert body. The chunk id is derived server-side
/// (`{document}#c{index:06}`) from the chunk's position in the array.
#[derive(Debug, Deserialize)]
pub struct ChunkDto {
    /// The text slice.
    pub text: String,
    /// Character offset of the slice start.
    pub char_start: usize,
    /// Character offset of the slice end (exclusive); must be ≥ `char_start`.
    pub char_end: usize,
}

/// `PUT /v1/collections/{id}/documents` body (upsert by `source`).
#[derive(Debug, Deserialize)]
pub struct DocumentUpsertRequest {
    /// Durable source identity (URI, path, external key). The document id
    /// is derived deterministically from collection + source.
    pub source: String,
    /// SHA-256 of the source bytes as 64 hex chars. Defaults to the hash of
    /// the concatenated chunk texts when omitted.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Flat JSON metadata (string/number/boolean values only).
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    /// Ordered chunks.
    pub chunks: Vec<ChunkDto>,
}

/// Document upsert response.
#[derive(Debug, Serialize)]
pub struct DocumentUpsertResponse {
    /// Derived document id.
    pub id: String,
    /// Document version (starts at 1; re-ingesting changed bytes bumps it).
    pub version: u64,
    /// Derived chunk ids, in order.
    pub chunk_ids: Vec<String>,
}

/// One chunk in a document response.
#[derive(Debug, Serialize)]
pub struct ChunkResponse {
    /// Derived chunk id.
    pub id: String,
    /// Position within the document (0-based).
    pub index: u32,
    /// The text slice.
    pub text: String,
    /// Character offset of the slice start.
    pub char_start: usize,
    /// Character offset of the slice end (exclusive).
    pub char_end: usize,
    /// SHA-256 of the text, hex.
    pub hash: String,
}

impl ChunkResponse {
    /// Builds the response from a stored chunk.
    #[must_use]
    pub fn from_chunk(chunk: &Chunk) -> Self {
        Self {
            id: chunk.id.as_str().to_string(),
            index: chunk.index,
            text: chunk.text.clone(),
            char_start: chunk.char_start,
            char_end: chunk.char_end,
            hash: chunk.hash.to_hex(),
        }
    }
}

/// `GET /v1/collections/{id}/documents/{document_id}` response.
#[derive(Debug, Serialize)]
pub struct DocumentResponse {
    /// Document id.
    pub id: String,
    /// Owning collection.
    pub collection: String,
    /// Durable source identity.
    pub source: String,
    /// User metadata as plain JSON.
    pub metadata: HashMap<String, serde_json::Value>,
    /// SHA-256 of the source bytes, hex.
    pub content_hash: String,
    /// Document version.
    pub version: u64,
    /// How the document entered the database.
    pub provenance: Provenance,
    /// Chunks ordered by index.
    pub chunks: Vec<ChunkResponse>,
}

impl DocumentResponse {
    /// Builds the response from a stored document and its chunks.
    #[must_use]
    pub fn from_stored(document: &Document, chunks: &[Chunk]) -> Self {
        Self {
            id: document.id.as_str().to_string(),
            collection: document.collection.as_str().to_string(),
            source: document.source.clone(),
            metadata: metadata_to_json(&document.metadata),
            content_hash: document.content_hash.to_hex(),
            version: document.version,
            provenance: document.provenance.clone(),
            chunks: chunks.iter().map(ChunkResponse::from_chunk).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// stats / flush / health
// ---------------------------------------------------------------------------

/// Aggregate counts (wire form of [`Stats`]).
#[derive(Debug, Serialize)]
pub struct StatsDto {
    /// Live (non-tombstoned) vectors.
    pub live_vectors: u64,
    /// Tombstoned vectors (hidden, still in the graph until rebuild).
    pub tombstoned: u64,
    /// Stored documents.
    pub documents: u64,
    /// Stored chunks.
    pub chunks: u64,
    /// Committed write batches.
    pub generation: u64,
    /// Generation the on-disk dump reflects.
    pub synced_generation: u64,
}

impl StatsDto {
    /// Builds the DTO from storage stats.
    #[must_use]
    pub fn from_stats(stats: &Stats) -> Self {
        Self {
            live_vectors: stats.live_vectors,
            tombstoned: stats.tombstoned,
            documents: stats.documents,
            chunks: stats.chunks,
            generation: stats.generation,
            synced_generation: stats.synced_generation,
        }
    }
}

/// `GET /v1/collections/{id}/stats` response.
#[derive(Debug, Serialize)]
pub struct CollectionStatsResponse {
    /// Collection slug.
    pub id: String,
    /// Durable ANN index state.
    pub index_state: IndexState,
    /// Aggregate counts.
    pub stats: StatsDto,
}

/// `POST /v1/collections/{id}/flush` response.
#[derive(Debug, Serialize)]
pub struct FlushResponse {
    /// Collection slug.
    pub id: String,
    /// Always true when the call succeeds.
    pub flushed: bool,
    /// Catalog generation after the checkpoint.
    pub generation: u64,
}

/// `GET /healthz` response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Always `"ok"`.
    pub status: &'static str,
    /// Service name.
    pub service: &'static str,
    /// Crate version.
    pub version: &'static str,
}

/// `GET /readyz` response.
#[derive(Debug, Serialize)]
pub struct ReadyResponse {
    /// Always `"ready"` (otherwise the status is 503).
    pub status: &'static str,
}

// ---------------------------------------------------------------------------
// conversions (DTO -> domain, via core validators — never reimplemented)
// ---------------------------------------------------------------------------

/// Builds a validated [`CollectionConfig`] from a create request.
///
/// # Errors
/// [`ApiError`] (400) if the id, metric, dimension, model tag, or HNSW
/// parameters fail validation; nothing here duplicates core rules.
pub fn collection_config_from(req: &CreateCollectionRequest) -> Result<CollectionConfig, ApiError> {
    let id = CollectionId::new(&req.id)?;
    let metric: DistanceMetric = req.metric.parse()?;
    let mut config = CollectionConfig::new(id, req.dimension, metric)?;
    if let Some(model) = &req.model {
        config = config.with_model(ModelTag::new(&model.name, &model.version)?);
    }
    if let Some(hnsw) = &req.hnsw {
        config = config.with_hnsw(HnswParams {
            m: hnsw.m,
            ef_construction: hnsw.ef_construction,
            ef_search: hnsw.ef_search,
            max_elements: hnsw.max_elements,
        })?;
    }
    Ok(config)
}

/// Converts flat JSON metadata into the closed [`Metadata`] value model.
///
/// Only string/integer/float/boolean values are accepted — nested objects
/// and arrays are a 400 (the core value model has no nested maps).
///
/// # Errors
/// [`ApiError`] 400 if a value has an unsupported JSON shape.
pub fn metadata_from_json<S: std::hash::BuildHasher>(
    map: &HashMap<String, serde_json::Value, S>,
) -> Result<Metadata, ApiError> {
    let mut metadata = Metadata::new();
    for (key, value) in map {
        let converted = match value {
            serde_json::Value::String(s) => MetadataValue::Str(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    MetadataValue::Int(i)
                } else if let Some(f) = n.as_f64() {
                    MetadataValue::Float(f)
                } else {
                    return Err(ApiError::validation(format!(
                        "metadata[{key:?}]: number out of range"
                    )));
                }
            }
            serde_json::Value::Bool(b) => MetadataValue::Bool(*b),
            _ => {
                return Err(ApiError::validation(format!(
                    "metadata[{key:?}]: only string/number/boolean values are supported"
                )));
            }
        };
        metadata.insert(key.clone(), converted);
    }
    Ok(metadata)
}

/// Converts [`Metadata`] back to plain JSON.
#[must_use]
pub fn metadata_to_json(metadata: &Metadata) -> HashMap<String, serde_json::Value> {
    metadata
        .iter()
        .map(|(k, v)| {
            let json = match v {
                MetadataValue::Str(s) => serde_json::Value::String(s.clone()),
                MetadataValue::Int(i) => serde_json::json!(*i),
                MetadataValue::Float(f) => serde_json::json!(*f),
                MetadataValue::Bool(b) => serde_json::Value::Bool(*b),
            };
            (k.clone(), json)
        })
        .collect()
}

/// Parses an optional hex content hash, defaulting to the SHA-256 of the
/// concatenated chunk texts (joined by newline).
///
/// # Errors
/// [`ApiError`] 400 if the provided hash is not 64 hex chars.
pub fn content_hash_from(
    provided: Option<&str>,
    chunk_texts: &[&str],
) -> Result<ContentHash, ApiError> {
    if let Some(hex) = provided {
        return hex.parse::<ContentHash>().map_err(ApiError::from);
    }
    Ok(ContentHash::of(chunk_texts.join("\n").as_bytes()))
}

/// Validates a path id segment: core shape check plus an explicit refusal
/// of `/` and `\` (defense in depth — derived ids never contain them, and
/// actix path params never contain `/` anyway).
///
/// # Errors
/// [`ApiError`] 400 if the id is malformed.
pub fn parse_path_id<T>(raw: &str, kind: &str) -> Result<T, ApiError>
where
    T: std::str::FromStr<Err = polyvec_core::Error>,
{
    if raw.contains('/') || raw.contains('\\') {
        return Err(ApiError::validation(format!(
            "{kind} id must not contain path separators"
        )));
    }
    raw.parse::<T>().map_err(ApiError::from)
}

/// Validates document chunks: offsets must be ordered, and each chunk's
/// index is its position in the array.
///
/// # Errors
/// [`ApiError`] 400 on unordered offsets or an unrepresentable index.
pub fn chunks_from(document_id: &DocumentId, dtos: &[ChunkDto]) -> Result<Vec<Chunk>, ApiError> {
    dtos.iter()
        .enumerate()
        .map(|(i, dto)| {
            if dto.char_start > dto.char_end {
                return Err(ApiError::validation(format!(
                    "chunk {i}: char_start ({}) > char_end ({})",
                    dto.char_start, dto.char_end
                )));
            }
            let index = u32::try_from(i).map_err(|_| {
                ApiError::validation(format!("chunk {i}: too many chunks in one document"))
            })?;
            Ok(Chunk::new(
                document_id.clone(),
                index,
                &dto.text,
                dto.char_start,
                dto.char_end,
            ))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// ingestion
// ---------------------------------------------------------------------------

/// `POST /v1/collections/{id}/ingest` body.
#[derive(Debug, Deserialize)]
pub struct IngestRequestDto {
    /// Durable source identity (URI, path, external key).
    pub source: String,
    /// Original filename, if known (drives format detection).
    pub filename: Option<String>,
    /// MIME type hint, if known.
    pub mime: Option<String>,
    /// The source bytes: exactly one of `text` (UTF-8) or `base64`.
    pub content: IngestContentDto,
    /// Flat JSON metadata (string/number/boolean values only), merged over
    /// any existing document metadata on re-ingest.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    /// Client-supplied vectors, aligned 1:1 with chunks post-chunking.
    /// When present, no embedding provider is needed.
    pub vectors: Option<Vec<Vec<f32>>>,
    /// Caller-supplied idempotency key; derived from the content when absent.
    pub idempotency_key: Option<String>,
}

/// Source bytes for an ingest request: exactly one variant must be set.
#[derive(Debug, Deserialize)]
pub struct IngestContentDto {
    /// UTF-8 text content.
    pub text: Option<String>,
    /// Base64-encoded bytes (for HTML, CSV, or any binary-ish source).
    pub base64: Option<String>,
}

impl IngestContentDto {
    /// Decodes to raw bytes, enforcing "exactly one of text/base64".
    ///
    /// # Errors
    /// [`ApiError`] (400) when neither or both of `text`/`base64` are set,
    /// or when `base64` is not valid base64.
    pub fn into_bytes(self) -> Result<Vec<u8>, ApiError> {
        match (self.text, self.base64) {
            (Some(text), None) => Ok(text.into_bytes()),
            (None, Some(encoded)) => {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(&encoded)
                    .map_err(|e| {
                        ApiError::validation(format!("content.base64 is not valid base64: {e}"))
                    })
            }
            _ => Err(ApiError::validation(
                "content must set exactly one of 'text' or 'base64'",
            )),
        }
    }
}

/// `POST /v1/collections/{id}/ingest` response (`202 Accepted`).
#[derive(Debug, Serialize)]
pub struct IngestResponseDto {
    /// The accepted job's id (poll `GET /v1/jobs/{job_id}`).
    pub job_id: String,
    /// Always `"pending"` at submit time.
    pub state: String,
}

/// One ingestion job (`GET /v1/jobs/{id}`, `GET /v1/collections/{id}/jobs`).
#[derive(Debug, Serialize)]
pub struct JobDto {
    /// Job id.
    pub job_id: String,
    /// Owning collection.
    pub collection: String,
    /// Ingested source identity.
    pub source: String,
    /// `"ingest"` or `"delete"`.
    pub op: String,
    /// `"pending"`, `"running"`, `"retrying"`, `"done"`, or `"failed"`.
    pub state: String,
    /// Attempts used so far (1-based).
    pub attempts: u32,
    /// Attempt budget.
    pub max_attempts: u32,
    /// Creation time (unix seconds).
    pub created_at_secs: u64,
    /// Last update time (unix seconds).
    pub updated_at_secs: u64,
    /// Failure detail (set on `failed`, and while `retrying`).
    pub error: Option<JobErrorDto>,
    /// What the job changed (set on `done`).
    pub affected: Option<JobAffectedDto>,
}

/// Error detail for a failed (or retrying) job.
#[derive(Debug, Serialize)]
pub struct JobErrorDto {
    /// `"transient"` or `"permanent"`.
    pub kind: String,
    /// Human-readable cause (never carries secrets).
    pub message: String,
    /// Which attempt produced this error (1-based).
    pub attempt: u32,
}

/// What a finished job changed.
#[derive(Debug, Serialize)]
pub struct JobAffectedDto {
    /// Affected document id.
    pub document_id: String,
    /// Document version after the job (unchanged on skip).
    pub version: u64,
    /// Chunks written.
    pub chunks: u64,
    /// Vectors written.
    pub vectors: u64,
    /// True when the content was unchanged and nothing was written.
    pub skipped: bool,
}

impl JobDto {
    /// Projects an [`polyvec_ingestion::IngestionJob`] onto the wire.
    #[must_use]
    pub fn from_job(job: &polyvec_ingestion::IngestionJob) -> Self {
        Self {
            job_id: job.id.clone(),
            collection: job.collection.as_str().to_string(),
            source: job.source.clone(),
            op: job.op.as_str().to_string(),
            state: job.state.as_str().to_string(),
            attempts: job.attempts,
            max_attempts: job.max_attempts,
            created_at_secs: job.created_at_secs,
            updated_at_secs: job.updated_at_secs,
            error: job.error.as_ref().map(|e| JobErrorDto {
                kind: e.kind.clone(),
                message: e.message.clone(),
                attempt: e.attempt,
            }),
            affected: job.affected.as_ref().map(|a| JobAffectedDto {
                document_id: a.document_id.clone(),
                version: a.version,
                chunks: a.chunks,
                vectors: a.vectors,
                skipped: a.skipped,
            }),
        }
    }
}

/// `GET /v1/collections/{id}/jobs` response.
#[derive(Debug, Serialize)]
pub struct JobListResponse {
    /// Jobs, newest first.
    pub jobs: Vec<JobDto>,
    /// Number of jobs returned.
    pub count: usize,
}

// ---------------------------------------------------------------------------
// text query
// ---------------------------------------------------------------------------

/// `POST /v1/collections/{id}/query` body.
#[derive(Debug, Deserialize)]
pub struct QueryRequestDto {
    /// Text to embed with the configured provider, then search.
    pub text: String,
    /// Number of hits.
    pub k: usize,
    /// HNSW search width (default from the collection config).
    pub ef: Option<usize>,
}

/// One text-query hit.
#[derive(Debug, Serialize)]
pub struct QueryHitDto {
    /// Vector id.
    pub id: String,
    /// Distance to the query vector (lower is closer).
    pub distance: f32,
    /// Owning chunk id, when the vector came from an ingested chunk.
    pub chunk_id: Option<String>,
}

/// `POST /v1/collections/{id}/query` response.
#[derive(Debug, Serialize)]
pub struct QueryResponseDto {
    /// Hits ordered by increasing distance.
    pub hits: Vec<QueryHitDto>,
    /// Requested `k`.
    pub k: usize,
    /// Effective search width.
    pub ef: usize,
}
