//! Job runner: executes ingestion jobs with bounded concurrency, bounded
//! retries, and backpressure.
//!
//! The runner never touches storage directly: it works through the
//! [`DocStore`] and [`CollectionOpener`] traits, which the server implements
//! over its per-collection `Mutex<CollectionStore>`. That keeps
//! `polyvec-ingestion` independent of `polyvec-server` (dependency rule) and
//! makes the whole pipeline testable against fakes.
//!
//! Threading: the accept loop is async, but every job's pipeline (convert,
//! chunk, embed, store) is synchronous and runs on Tokio's blocking pool via
//! `spawn_blocking` — async workers are never stalled. The blocking HTTP of
//! the remote embedding adapter is safe there too.
//!
//! cost-guard mechanisms (see `docs/ingestion.md`):
//! * Bounded retries with exponential backoff and a per-job attempt budget;
//!   transient vs permanent classification — permanent failures never retry.
//! * `Retry-After` from a 429 is honored (capped) instead of hammering.
//! * Bounded queue + bounded concurrency; submitters get `429`/`503`, never
//!   an unbounded backlog.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, mpsc};

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, Document, DocumentId, Error, Metadata, MetadataValue,
    ModelTag, Provenance, Result, Vector, VectorId,
};
use polyvec_embeddings::{EmbedError, EmbeddingProvider, ErrorKind};

use crate::canonical::canonicalize;
use crate::chunk::Chunker;
use crate::converter::{Converter, SourceHint};
use crate::jobs::{
    AffectedRecords, DocRecord, IngestionJob, JobErrorInfo, JobId, JobOp, JobState, JobStore,
};

// ---------------------------------------------------------------------------
// storage traits (implemented by the server)
// ---------------------------------------------------------------------------

/// Storage operations the runner needs. Implemented by `polyvec-server` over
/// its per-collection `Mutex<CollectionStore>`.
pub trait DocStore: Send + Sync {
    /// Snapshot of the collection config (dimension, model tag, chunking).
    fn config(&self) -> CollectionConfig;

    /// Fetches a document and its chunks, if it exists.
    fn get_document(&self, id: &DocumentId) -> Result<Option<(Document, Vec<Chunk>)>>;

    /// Writes a document version atomically: tombstones the previous
    /// version's vectors, removes its chunks, then stores the new
    /// document, chunks, and vectors.
    fn ingest_document(&self, doc: &Document, chunks: &[Chunk], vectors: &[Vector]) -> Result<()>;

    /// Deletes a document, cascading to its chunks and their vectors.
    fn delete_document(&self, id: &DocumentId) -> Result<()>;

    /// Claims the collection for an embedding model (one-time). Fails if a
    /// *different* tag is already set; setting the same tag twice is a no-op.
    fn stamp_model_tag(&self, tag: &ModelTag) -> Result<()>;
}

/// Opens collections for the runner. Implemented by `polyvec-server` over
/// its registry.
pub trait CollectionOpener: Send + Sync {
    /// Opens (or returns the open handle for) a collection.
    ///
    /// # Errors
    /// [`Error::NotFound`] for unknown collections.
    fn open(&self, id: &CollectionId) -> Result<Arc<dyn DocStore>>;
}

// ---------------------------------------------------------------------------
// requests
// ---------------------------------------------------------------------------

/// An ingest submission: source bytes plus options.
#[derive(Clone, Debug)]
pub struct IngestRequest {
    /// Target collection.
    pub collection: CollectionId,
    /// Durable source identity (URI, path, external key).
    pub source: String,
    /// Original filename, if known (drives format detection).
    pub filename: Option<String>,
    /// MIME type hint, if known.
    pub mime: Option<String>,
    /// Raw source bytes.
    pub content: Vec<u8>,
    /// User metadata for the document (merged over existing on re-ingest;
    /// `_polyvec.*` keys are pipeline-owned).
    pub metadata: Metadata,
    /// Client-supplied vectors, aligned 1:1 with chunks post-chunking.
    /// When present, no embedding provider is needed.
    pub vectors: Option<Vec<Vec<f32>>>,
    /// Caller-supplied idempotency key; derived from the content when absent.
    pub idempotency_key: Option<String>,
    /// Attempt budget override.
    pub max_attempts: Option<u32>,
}

/// A delete submission.
#[derive(Clone, Debug)]
pub struct DeleteRequest {
    /// Target collection.
    pub collection: CollectionId,
    /// Durable source identity of the document to delete.
    pub source: String,
    /// Caller-supplied idempotency key; derived when absent.
    pub idempotency_key: Option<String>,
    /// Attempt budget override.
    pub max_attempts: Option<u32>,
}

// ---------------------------------------------------------------------------
// limits & errors
// ---------------------------------------------------------------------------

/// Runner tuning knobs.
#[derive(Clone, Debug)]
pub struct RunnerLimits {
    /// Max queued (not yet running) jobs; submissions beyond this fail fast.
    pub queue_capacity: usize,
    /// Max jobs executing concurrently.
    pub max_concurrency: usize,
    /// Default per-job attempt budget.
    pub max_attempts: u32,
    /// First retry delay; doubles per attempt.
    pub base_backoff: Duration,
    /// Backoff cap.
    pub max_backoff: Duration,
}

impl Default for RunnerLimits {
    fn default() -> Self {
        Self {
            queue_capacity: 64,
            max_concurrency: 4,
            max_attempts: 5,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// Why a submission was refused.
#[derive(Debug)]
pub enum SubmitError {
    /// The bounded queue is full — back off and retry.
    QueueFull {
        /// Seconds the submitter should wait before retrying.
        retry_after_secs: u64,
    },
    /// The runner is shutting down.
    ShuttingDown,
    /// Job creation failed.
    Store(Error),
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::QueueFull { retry_after_secs } => write!(
                f,
                "ingest queue is full; retry after {retry_after_secs}s"
            ),
            SubmitError::ShuttingDown => write!(f, "ingest runner is shutting down"),
            SubmitError::Store(e) => write!(f, "job store: {e}"),
        }
    }
}

impl std::error::Error for SubmitError {}

// ---------------------------------------------------------------------------
// pipeline error
// ---------------------------------------------------------------------------

/// A pipeline failure with its retry classification.
enum PipelineError {
    Permanent(String),
    Transient {
        message: String,
        retry_after: Option<Duration>,
    },
}

/// Pipeline results carry [`PipelineError`] (not [`Error`]) so the retry
/// classification survives to the runner.
type PipelineResult<T> = std::result::Result<T, PipelineError>;

impl PipelineError {
    fn message(&self) -> &str {
        match self {
            PipelineError::Permanent(m) => m,
            PipelineError::Transient { message, .. } => message,
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            PipelineError::Permanent(_) => "permanent",
            PipelineError::Transient { .. } => "transient",
        }
    }
}

impl From<Error> for PipelineError {
    fn from(e: Error) -> Self {
        // Storage I/O may be transient (bounded retries make this safe);
        // everything else fails the job outright.
        match e {
            Error::Storage { .. } => PipelineError::Transient {
                message: e.to_string(),
                retry_after: None,
            },
            _ => PipelineError::Permanent(e.to_string()),
        }
    }
}

impl From<EmbedError> for PipelineError {
    fn from(e: EmbedError) -> Self {
        match e.kind {
            ErrorKind::Transient => PipelineError::Transient {
                message: e.message.clone(),
                retry_after: e.retry_after,
            },
            ErrorKind::Permanent => PipelineError::Permanent(e.message.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// runner
// ---------------------------------------------------------------------------

/// A queued job with its input payload (payloads live only in memory).
enum QueuedJob {
    Ingest { id: JobId, req: IngestRequest },
    Delete { id: JobId, req: DeleteRequest },
}

impl QueuedJob {
    fn id(&self) -> &JobId {
        match self {
            QueuedJob::Ingest { id, .. } | QueuedJob::Delete { id, .. } => id,
        }
    }
}

/// Outcome of one pipeline attempt.
enum Outcome {
    Done,
    Failed,
    RetryAfter(Duration),
}

struct RunnerInner {
    store: Mutex<JobStore>,
    provider: Option<Arc<dyn EmbeddingProvider>>,
    converter: Arc<dyn Converter>,
    limits: RunnerLimits,
    tx: Mutex<Option<mpsc::Sender<QueuedJob>>>,
    rx: Mutex<Option<mpsc::Receiver<QueuedJob>>>,
    accept_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    spawned: AtomicBool,
}

/// Executes ingestion jobs with bounded concurrency, retries, and backpressure.
#[derive(Clone)]
pub struct JobRunner {
    inner: Arc<RunnerInner>,
}

impl JobRunner {
    /// Builds the runner. Call [`JobRunner::spawn`] to start accepting jobs.
    pub fn new(
        store: JobStore,
        provider: Option<Arc<dyn EmbeddingProvider>>,
        converter: Arc<dyn Converter>,
        limits: RunnerLimits,
    ) -> Self {
        let (tx, rx) = mpsc::channel(limits.queue_capacity.max(1));
        Self {
            inner: Arc::new(RunnerInner {
                store: Mutex::new(store),
                provider,
                converter,
                limits,
                tx: Mutex::new(Some(tx)),
                rx: Mutex::new(Some(rx)),
                accept_handle: Mutex::new(None),
                spawned: AtomicBool::new(false),
            }),
        }
    }

    /// Starts the accept loop on the current Tokio runtime. Idempotent:
    /// returns `false` if already started.
    ///
    /// # Panics
    /// Panics if called outside a Tokio runtime (documented: the server
    /// always calls this from async context).
    pub fn spawn(&self, opener: Arc<dyn CollectionOpener>) -> bool {
        if self.inner.spawned.swap(true, Ordering::SeqCst) {
            return false;
        }
        let rx = self
            .inner
            .rx
            .lock()
            .expect("runner lock")
            .take()
            .expect("runner spawned twice without receiver");
        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(async move {
            accept_loop(inner, rx, opener).await;
        });
        *self
            .inner
            .accept_handle
            .lock()
            .expect("runner lock") = Some(handle);
        true
    }

    /// Submits an ingest job without blocking. Duplicate in-flight
    /// submissions (same idempotency key) return the existing job id.
    pub fn try_submit_ingest(&self, req: IngestRequest) -> std::result::Result<JobId, SubmitError> {
        let key = req.idempotency_key.clone().unwrap_or_else(|| {
            ingest_idempotency_key(&req.collection, &req.source, &req.content)
        });
        let max_attempts = req
            .max_attempts
            .unwrap_or(self.inner.limits.max_attempts);
        let (job, created) = self
            .inner
            .store
            .lock()
            .expect("runner lock")
            .find_or_create(
                &req.collection,
                &req.source,
                JobOp::Ingest,
                &key,
                max_attempts,
            )
            .map_err(SubmitError::Store)?;
        let id = JobId::new(job.id.clone());
        if created {
            self.enqueue(QueuedJob::Ingest { id: id.clone(), req })?;
        }
        Ok(id)
    }

    /// Submits a delete job without blocking.
    pub fn try_submit_delete(&self, req: DeleteRequest) -> std::result::Result<JobId, SubmitError> {
        let key = req
            .idempotency_key
            .clone()
            .unwrap_or_else(|| delete_idempotency_key(&req.collection, &req.source));
        let max_attempts = req
            .max_attempts
            .unwrap_or(self.inner.limits.max_attempts);
        let (job, created) = self
            .inner
            .store
            .lock()
            .expect("runner lock")
            .find_or_create(
                &req.collection,
                &req.source,
                JobOp::Delete,
                &key,
                max_attempts,
            )
            .map_err(SubmitError::Store)?;
        let id = JobId::new(job.id.clone());
        if created {
            self.enqueue(QueuedJob::Delete { id: id.clone(), req })?;
        }
        Ok(id)
    }

    /// Fetches a job record by id.
    pub fn get_job(&self, id: &JobId) -> Result<Option<IngestionJob>> {
        self.inner
            .store
            .lock()
            .expect("runner lock")
            .get(id)
    }

    /// Lists jobs for a collection, newest first.
    pub fn list_jobs(&self, collection: &CollectionId, limit: usize) -> Result<Vec<IngestionJob>> {
        self.inner
            .store
            .lock()
            .expect("runner lock")
            .list_for_collection(collection, limit)
    }

    /// Stops accepting new submissions and waits for in-flight jobs to
    /// finish. Jobs still queued are left `Pending` in the store — but their
    /// payloads are gone, so a restart marks them failed (see
    /// [`JobStore::open`]); resubmission is the remedy.
    pub async fn shutdown(&self) {
        // Stop accepting: dropping the sender closes the channel.
        self.inner.tx.lock().expect("runner lock").take();
        if let Some(handle) = self
            .inner
            .accept_handle
            .lock()
            .expect("runner lock")
            .take()
        {
            let _ = handle.await;
        }
    }

    /// Enqueues a job or fails fast when the bounded queue is full.
    fn enqueue(&self, job: QueuedJob) -> std::result::Result<(), SubmitError> {
        let guard = self.inner.tx.lock().expect("runner lock");
        let Some(tx) = guard.as_ref() else {
            return Err(SubmitError::ShuttingDown);
        };
        match tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(SubmitError::QueueFull {
                retry_after_secs: 5,
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SubmitError::ShuttingDown),
        }
    }
}

/// Derives the default idempotency key for an ingest: identical bytes for
/// the same source always map to the same key.
fn ingest_idempotency_key(collection: &CollectionId, source: &str, content: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"ingest|");
    h.update(collection.as_str().as_bytes());
    h.update(b"|");
    h.update(source.as_bytes());
    h.update(b"|");
    h.update(content);
    format!("{:x}", h.finalize())
}

/// Derives the default idempotency key for a delete.
fn delete_idempotency_key(collection: &CollectionId, source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"delete|");
    h.update(collection.as_str().as_bytes());
    h.update(b"|");
    h.update(source.as_bytes());
    format!("{:x}", h.finalize())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The accept loop: pulls queued jobs and runs each under the concurrency
/// semaphore. Ends when the channel closes; then drains in-flight jobs.
async fn accept_loop(
    inner: Arc<RunnerInner>,
    mut rx: mpsc::Receiver<QueuedJob>,
    opener: Arc<dyn CollectionOpener>,
) {
    let semaphore = Arc::new(Semaphore::new(inner.limits.max_concurrency.max(1)));
    let mut handles = Vec::new();
    while let Some(queued) = rx.recv().await {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore closed");
        let inner = Arc::clone(&inner);
        let opener = Arc::clone(&opener);
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            run_with_retries(&inner, &opener, queued).await;
        }));
    }
    for handle in handles {
        let _ = handle.await;
    }
}

/// Runs one job to a terminal state, retrying transient failures with
/// bounded exponential backoff.
async fn run_with_retries(
    inner: &Arc<RunnerInner>,
    opener: &Arc<dyn CollectionOpener>,
    queued: QueuedJob,
) {
    let queued = Arc::new(queued);
    let job_id = queued.id().clone();
    loop {
        // Fresh clones for the blocking closure; the loop's own bindings
        // stay usable for the failure path below.
        let inner_c = Arc::clone(inner);
        let opener_c = Arc::clone(opener);
        let queued_c = Arc::clone(&queued);
        let job_id_c = job_id.clone();
        // The whole pipeline is synchronous; run it on the blocking pool.
        let outcome = tokio::task::spawn_blocking(move || {
            execute_once(&inner_c, &opener_c, &job_id_c, &queued_c)
        })
        .await;
        match outcome {
            Ok(Outcome::Done) | Ok(Outcome::Failed) => break,
            Ok(Outcome::RetryAfter(d)) => tokio::time::sleep(d).await,
            Err(join_err) => {
                // The worker panicked: never report it as complete.
                mark_failed(
                    inner,
                    &job_id,
                    PipelineError::Permanent(format!("worker panic: {join_err}")),
                );
                break;
            }
        }
    }
}

/// One pipeline attempt: marks the job running, runs it, records the outcome.
fn execute_once(
    inner: &RunnerInner,
    opener: &Arc<dyn CollectionOpener>,
    job_id: &JobId,
    queued: &QueuedJob,
) -> Outcome {
    let mut job = match inner.store.lock().expect("runner lock").get(job_id) {
        Ok(Some(j)) => j,
        Ok(None) => {
            log::error!("ingest job {} vanished from the store; dropping", job_id);
            return Outcome::Failed;
        }
        Err(e) => {
            log::error!("ingest job {job_id}: cannot load record: {e}; dropping");
            return Outcome::Failed;
        }
    };
    job.state = JobState::Running;
    job.attempts += 1;
    job.error = None;
    if let Err(e) = inner.store.lock().expect("runner lock").update(&job) {
        log::error!("ingest job {job_id}: cannot mark running: {e}");
        return Outcome::Failed;
    }

    let result = match queued {
        QueuedJob::Ingest { req, .. } => run_ingest(inner, opener, &job, req),
        QueuedJob::Delete { req, .. } => run_delete(inner, opener, &job, req),
    };

    match result {
        Ok(affected) => {
            job.state = JobState::Done;
            job.affected = Some(affected);
            job.error = None;
            if let Err(e) = inner.store.lock().expect("runner lock").update(&job) {
                log::error!("ingest job {job_id}: cannot mark done: {e}");
            }
            Outcome::Done
        }
        Err(e) => {
            let budget_exhausted = job.attempts >= job.max_attempts;
            match e {
                PipelineError::Permanent(_) => {
                    mark_failed(inner, job_id, e);
                    Outcome::Failed
                }
                PipelineError::Transient { retry_after, .. } if budget_exhausted => {
                    let msg = format!("{}; retry budget exhausted after {} attempts", e.message(), job.attempts);
                    mark_failed(
                        inner,
                        job_id,
                        PipelineError::Transient {
                            message: msg,
                            retry_after,
                        },
                    );
                    Outcome::Failed
                }
                PipelineError::Transient { retry_after, .. } => {
                    job.state = JobState::Retrying;
                    job.error = Some(JobErrorInfo {
                        kind: e.kind_label().to_string(),
                        message: e.message().to_string(),
                        attempt: job.attempts,
                    });
                    if let Err(upd) = inner.store.lock().expect("runner lock").update(&job) {
                        log::error!("ingest job {job_id}: cannot mark retrying: {upd}");
                        return Outcome::Failed;
                    }
                    Outcome::RetryAfter(backoff_for(inner, job.attempts, retry_after))
                }
            }
        }
    }
}

/// Records a terminal failure. Failed jobs stay failed — partial progress
/// is visible in `affected` but never reported as complete.
fn mark_failed(inner: &RunnerInner, job_id: &JobId, e: PipelineError) {
    let mut job = match inner.store.lock().expect("runner lock").get(job_id) {
        Ok(Some(j)) => j,
        _ => return,
    };
    // Preserve partial progress if the pipeline recorded any.
    let affected = job.affected.clone();
    job.state = JobState::Failed;
    job.error = Some(JobErrorInfo {
        kind: e.kind_label().to_string(),
        message: e.message().to_string(),
        attempt: job.attempts,
    });
    job.affected = affected;
    if let Err(upd) = inner.store.lock().expect("runner lock").update(&job) {
        log::error!("ingest job {job_id}: cannot mark failed: {upd}");
    }
}

/// Backoff for attempt `n` (1-based): `base * 2^(n-1)` capped at max, or the
/// server's `Retry-After` when present (also capped — a malicious or
/// confused server cannot park the job forever).
fn backoff_for(inner: &RunnerInner, attempts: u32, retry_after: Option<Duration>) -> Duration {
    let exp = attempts.saturating_sub(1).min(10);
    let backoff = inner
        .limits
        .base_backoff
        .checked_mul(2u32.pow(exp))
        .unwrap_or(inner.limits.max_backoff)
        .min(inner.limits.max_backoff);
    retry_after.map_or(backoff, |ra| ra.min(inner.limits.max_backoff))
}

/// The ingest pipeline: convert → canonicalize → chunk → embed (or supplied
/// vectors) → atomic store write.
#[allow(clippy::too_many_lines)]
fn run_ingest(
    inner: &RunnerInner,
    opener: &Arc<dyn CollectionOpener>,
    job: &IngestionJob,
    req: &IngestRequest,
) -> PipelineResult<AffectedRecords> {
    let store = opener.open(&req.collection).map_err(PipelineError::from)?;
    let config = store.config();
    let document_id = DocumentId::for_source(&req.collection, &req.source);
    let document_id_str = document_id.as_str().to_string();

    // 1. Convert + canonicalize.
    let hint = SourceHint {
        filename: req.filename.clone(),
        mime: req.mime.clone(),
        extension: req
            .filename
            .as_deref()
            .and_then(|f| f.rsplit_once('.'))
            .map(|(_, ext)| ext.to_string()),
    };
    let converted = inner.converter.convert(&req.content, &hint).map_err(PipelineError::from)?;
    let canonical = canonicalize(converted, &req.source, req.filename.as_deref(), &req.content);

    // 2. Change detection: unchanged content (and chunking) → skip.
    let existing = store.get_document(&document_id).map_err(PipelineError::from)?;
    if let Some((doc, _)) = &existing {
        let prev = inner
            .store
            .lock()
            .expect("runner lock")
            .get_doc_record(&document_id_str).map_err(PipelineError::from)?;
        let chunking_same = prev
            .as_ref()
            .is_some_and(|r| r.chunking_version == config.chunking.version);
        if doc.content_hash == canonical.canonical_hash && chunking_same {
            log::info!(
                "ingest job {}: content unchanged, skipping (version {})",
                job.id,
                doc.version
            );
            return Ok(AffectedRecords {
                document_id: document_id_str,
                version: doc.version,
                chunks: 0,
                vectors: 0,
                skipped: true,
            });
        }
    }

    // 3. Chunk.
    let chunker = Chunker::new(config.chunking.clone()).map_err(PipelineError::from)?;
    let raw = chunker.chunk(&canonical.markdown);
    if raw.is_empty() {
        return Err(PipelineError::Permanent(
            "document produced no chunks (empty after normalization)".to_string(),
        ));
    }

    // 4. Vectors: supplied path or provider path (never silently mixed).
    let dim = config.dimension;
    let vectors: Vec<Vec<f32>> = match &req.vectors {
        Some(supplied) => {
            if supplied.len() != raw.len() {
                return Err(PipelineError::Permanent(format!(
                    "supplied {} vectors for {} chunks; must align 1:1",
                    supplied.len(),
                    raw.len()
                )));
            }
            supplied.clone()
        }
        None => {
            let provider = inner.provider.as_ref().ok_or_else(|| {
                PipelineError::Permanent(
                    "no embedding provider configured and no supplied vectors; \
                     set POLYVEC_EMBEDDING_PROVIDER or supply vectors"
                        .to_string(),
                )
            })?;
            if provider.dimension() != dim {
                return Err(PipelineError::Permanent(format!(
                    "provider dimension {} does not match collection dimension {dim}",
                    provider.dimension()
                )));
            }
            check_model_tag(&*store, &**provider)?;
            let texts: Vec<&str> = raw.iter().map(|c| c.text.as_str()).collect();
            provider.embed(&texts).map_err(PipelineError::from)?
        }
    };

    // 5. Build the document version, chunks, and vectors.
    let version = existing.as_ref().map_or(1, |(d, _)| d.version + 1);
    let mut metadata = existing.as_ref().map_or_else(Metadata::default, |(d, _)| d.metadata.clone());
    for (k, v) in req.metadata.iter() {
        metadata.insert(k.clone(), v.clone());
    }
    // Pipeline-owned stamps: visible via the documents API, never silently dropped.
    metadata.insert(
        "_polyvec.original_hash".to_string(),
        MetadataValue::Str(canonical.original_hash.to_hex()),
    );
    metadata.insert(
        "_polyvec.converter".to_string(),
        MetadataValue::Str(canonical.conversion.converter.clone()),
    );
    metadata.insert(
        "_polyvec.source_format".to_string(),
        MetadataValue::Str(canonical.source_format.clone()),
    );
    metadata.insert(
        "_polyvec.chunking_version".to_string(),
        MetadataValue::Int(i64::from(config.chunking.version)),
    );
    let doc = Document {
        id: document_id.clone(),
        collection: req.collection.clone(),
        source: req.source.clone(),
        metadata,
        content_hash: canonical.canonical_hash.clone(),
        version,
        provenance: Provenance::Ingested {
            source: format!("job:{}", job.id),
            at_secs: now_secs(),
        },
    };
    let chunks: Vec<Chunk> = raw
        .iter()
        .enumerate()
        .map(|(i, r)| {
            Chunk::new_versioned(
                document_id.clone(),
                version,
                i as u32,
                &r.text,
                r.char_start,
                r.char_end,
            )
        })
        .collect();
    let mut typed_vectors = Vec::with_capacity(vectors.len());
    for (chunk, values) in chunks.iter().zip(vectors) {
        typed_vectors.push(Vector::new(VectorId::for_chunk(&chunk.id), values, dim).map_err(PipelineError::from)?);
    }

    // 6. Atomic store write: old version tombstoned, new version stored.
    store.ingest_document(&doc, &chunks, &typed_vectors).map_err(PipelineError::from)?;

    // 7. Durable provenance record.
    inner.store.lock().expect("runner lock").put_doc_record(
        &DocRecord {
            document_id: document_id_str.clone(),
            collection: req.collection.as_str().to_string(),
            source: req.source.clone(),
            version,
            original_hash: canonical.original_hash.to_hex(),
            canonical_hash: canonical.canonical_hash.to_hex(),
            chunking_version: config.chunking.version,
            converter: canonical.conversion.converter.clone(),
            source_format: canonical.source_format.clone(),
            filename: req.filename.clone(),
        },
    ).map_err(PipelineError::from)?;

    Ok(AffectedRecords {
        document_id: document_id_str,
        version,
        chunks: chunks.len() as u64,
        vectors: typed_vectors.len() as u64,
        skipped: false,
    })
}

/// The delete pipeline: cascade through storage, drop the provenance record.
/// Deleting a missing document is a no-op success (idempotent).
fn run_delete(
    inner: &RunnerInner,
    opener: &Arc<dyn CollectionOpener>,
    _job: &IngestionJob,
    req: &DeleteRequest,
) -> PipelineResult<AffectedRecords> {
    let store = opener.open(&req.collection).map_err(PipelineError::from)?;
    let document_id = DocumentId::for_source(&req.collection, &req.source);
    let document_id_str = document_id.as_str().to_string();
    match store.get_document(&document_id).map_err(PipelineError::from)? {
        None => Ok(AffectedRecords {
            document_id: document_id_str,
            version: 0,
            chunks: 0,
            vectors: 0,
            skipped: true,
        }),
        Some((doc, _)) => {
            store.delete_document(&document_id).map_err(PipelineError::from)?;
            inner
                .store
                .lock()
                .expect("runner lock")
                .remove_doc_record(&document_id_str).map_err(PipelineError::from)?;
            Ok(AffectedRecords {
                document_id: document_id_str,
                version: doc.version,
                chunks: 0,
                vectors: 0,
                skipped: false,
            })
        }
    }
}

/// Enforces the never-silently-mix-models rule: the provider's dimension and
/// model tag must agree with the collection. An untagged collection is
/// claimed (once, loudly) by the first model that writes to it.
fn check_model_tag(
    store: &dyn DocStore,
    provider: &dyn EmbeddingProvider,
) -> PipelineResult<()> {
    let tag = provider.model_id();
    let config = store.config();
    match &config.model {
        Some(existing) if *existing != tag => Err(PipelineError::Permanent(format!(
            "model mismatch: collection uses {} {}, provider is {} {} — refusing to mix",
            existing.name, existing.version, tag.name, tag.version
        ))),
        Some(_) => Ok(()),
        None => {
            log::info!(
                "claiming collection '{}' for embedding model {} {}",
                config.name,
                tag.name,
                tag.version
            );
            store.stamp_model_tag(&tag).map_err(PipelineError::from)
        }
    }
}
