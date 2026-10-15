//! Job store and runner tests: persistence, crash recovery, idempotency,
//! and the full ingest pipeline against a real `CollectionStore`.
//!
//! These are the Phase 4 acceptance tests: bytes in → searchable vectors
//! out; unchanged re-ingest → no dupes; update → old tombstoned; delete →
//! cascade; failures stay failed with detail.

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, Document, DocumentId, Error, Metadata, ModelTag, Result,
    Vector, VectorId,
};
use polyvec_embeddings::{EmbedError, EmbeddingProvider, StubEmbeddingProvider};
use polyvec_ingestion::{
    CollectionOpener, Converter, DeleteRequest, DocStore, IngestRequest, IngestionJob, JobId,
    JobOp, JobRunner, JobState, JobStore, MarkitConverter, RunnerLimits, SubmitError,
};
use polyvec_storage::CollectionStore;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DIM: u32 = 32;

fn tmp(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("polyvec-ingest-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_config() -> CollectionConfig {
    CollectionConfig::new(
        CollectionId::new("kb").unwrap(),
        DIM,
        polyvec_core::DistanceMetric::Cosine,
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// test doubles: storage access over a real CollectionStore
// ---------------------------------------------------------------------------

struct TestDocStore {
    store: Mutex<CollectionStore>,
    lexical_calls: Mutex<Vec<LexicalCall>>,
}

/// Records how the runner drives the lexical hooks: the ingestion crate
/// owns the trait, the real tantivy behavior lives in `polyvec-retrieval`
/// (tested there) and in the server wiring (tested at the HTTP level).
#[derive(Clone, Debug, PartialEq, Eq)]
enum LexicalCall {
    Index { doc_id: String, chunks: usize },
    Remove { doc_id: String },
}

impl TestDocStore {
    fn search(&self, query: &[f32], k: usize, ef: usize) -> Vec<polyvec_storage::SearchHit> {
        self.store.lock().unwrap().search(query, k, ef).unwrap()
    }

    fn stats(&self) -> polyvec_storage::Stats {
        self.store.lock().unwrap().stats()
    }
}

impl DocStore for TestDocStore {
    fn config(&self) -> Result<CollectionConfig> {
        Ok(self.store.lock().unwrap().config().clone())
    }
    fn get_document(&self, id: &DocumentId) -> Result<Option<(Document, Vec<Chunk>)>> {
        Ok(self.store.lock().unwrap().get_document(id).ok())
    }
    fn vectors_present(&self, chunks: &[Chunk]) -> Result<bool> {
        self.store.lock().unwrap().vectors_present(chunks)
    }
    fn ingest_document(&self, doc: &Document, chunks: &[Chunk], vectors: &[Vector]) -> Result<()> {
        self.store
            .lock()
            .unwrap()
            .ingest_document(doc, chunks, vectors)
    }
    fn index_lexical(&self, doc: &Document, chunks: &[Chunk]) -> Result<()> {
        self.lexical_calls.lock().unwrap().push(LexicalCall::Index {
            doc_id: doc.id.as_str().to_string(),
            chunks: chunks.len(),
        });
        Ok(())
    }
    fn remove_lexical(&self, id: &DocumentId) -> Result<()> {
        self.lexical_calls
            .lock()
            .unwrap()
            .push(LexicalCall::Remove {
                doc_id: id.as_str().to_string(),
            });
        Ok(())
    }
    fn delete_document(&self, id: &DocumentId) -> Result<()> {
        self.store.lock().unwrap().delete_document(id)
    }
    fn stamp_model_tag(&self, tag: &ModelTag) -> Result<()> {
        self.store.lock().unwrap().stamp_model_tag(tag)
    }
}

struct TestOpener {
    dir: std::path::PathBuf,
    stores: Mutex<HashMap<String, Arc<TestDocStore>>>,
}

impl TestOpener {
    fn new(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            stores: Mutex::new(HashMap::new()),
        }
    }

    fn raw(&self, id: &CollectionId) -> Arc<TestDocStore> {
        let mut stores = self.stores.lock().unwrap();
        Arc::clone(stores.entry(id.as_str().to_string()).or_insert_with(|| {
            let coll_dir = self.dir.join(id.as_str());
            let store = if coll_dir.join("catalog.redb").exists() {
                CollectionStore::open(&coll_dir).unwrap().0
            } else {
                CollectionStore::create(&coll_dir, test_config()).unwrap()
            };
            Arc::new(TestDocStore {
                store: Mutex::new(store),
                lexical_calls: Mutex::new(Vec::new()),
            })
        }))
    }
}

impl CollectionOpener for TestOpener {
    fn open(&self, id: &CollectionId) -> Result<Arc<dyn DocStore>> {
        if id.as_str() != "kb" {
            return Err(Error::NotFound {
                what: format!("collection '{id}'"),
            });
        }
        Ok(self.raw(id))
    }
}

// ---------------------------------------------------------------------------
// test doubles: providers
// ---------------------------------------------------------------------------

/// A provider with scripted failures for retry tests.
struct FlakyProvider {
    stub: StubEmbeddingProvider,
    failures_left: AtomicU32,
    mode: FlakyMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlakyMode {
    Transient,
    Permanent,
}

impl FlakyProvider {
    fn new(failures: u32, mode: FlakyMode) -> Self {
        Self {
            stub: StubEmbeddingProvider::new(DIM).unwrap(),
            failures_left: AtomicU32::new(failures),
            mode,
        }
    }
}

impl EmbeddingProvider for FlakyProvider {
    fn embed(&self, texts: &[&str]) -> std::result::Result<Vec<Vec<f32>>, EmbedError> {
        if self.failures_left.fetch_sub(1, Ordering::SeqCst) > 0 {
            return Err(match self.mode {
                FlakyMode::Transient => EmbedError::transient("flaky: transient boom"),
                FlakyMode::Permanent => EmbedError::permanent("flaky: permanent boom"),
            });
        }
        self.stub.embed(texts)
    }
    fn model_id(&self) -> ModelTag {
        self.stub.model_id()
    }
    fn dimension(&self) -> u32 {
        DIM
    }
}

/// Same deterministic embeddings as the stub, but a different model tag.
struct RenamedProvider {
    stub: StubEmbeddingProvider,
}

impl EmbeddingProvider for RenamedProvider {
    fn embed(&self, texts: &[&str]) -> std::result::Result<Vec<Vec<f32>>, EmbedError> {
        self.stub.embed(texts)
    }
    fn model_id(&self) -> ModelTag {
        ModelTag::new("totally-different-model", "9.9").unwrap()
    }
    fn dimension(&self) -> u32 {
        DIM
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn test_limits() -> RunnerLimits {
    RunnerLimits {
        queue_capacity: 16,
        max_concurrency: 2,
        max_attempts: 3,
        base_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    }
}

fn stub_provider() -> Arc<dyn EmbeddingProvider> {
    Arc::new(StubEmbeddingProvider::new(DIM).unwrap())
}

/// Builds + spawns a runner wired to a real `CollectionStore`.
fn spawn_runner(
    dir: &std::path::Path,
    provider: Option<Arc<dyn EmbeddingProvider>>,
) -> (JobRunner, Arc<TestOpener>) {
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let runner = JobRunner::new(
        store,
        provider,
        Arc::new(MarkitConverter::new()),
        test_limits(),
    );
    let opener = Arc::new(TestOpener::new(dir.to_path_buf()));
    assert!(runner.spawn(Arc::clone(&opener) as Arc<dyn CollectionOpener>));
    (runner, opener)
}

fn ingest_req(source: &str, content: &str) -> IngestRequest {
    IngestRequest {
        collection: CollectionId::new("kb").unwrap(),
        source: source.to_string(),
        filename: Some("doc.txt".to_string()),
        mime: Some("text/plain".to_string()),
        content: content.as_bytes().to_vec(),
        metadata: Metadata::default(),
        vectors: None,
        idempotency_key: None,
        max_attempts: None,
    }
}

/// Blocks until the job is terminal (panics on timeout).
async fn wait_done(runner: &JobRunner, id: &JobId) -> IngestionJob {
    let start = Instant::now();
    loop {
        let job = runner.get_job(id).unwrap().unwrap();
        if job.state.is_terminal() {
            return job;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "job {id} stuck in {:?}",
            job.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// job store: persistence, crash recovery, idempotency
// ---------------------------------------------------------------------------

#[test]
fn job_store_persists_across_reopen() {
    let dir = tmp("store-persist");
    let coll = CollectionId::new("kb").unwrap();
    let id = {
        let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
        let job = store
            .create_job(&coll, "src", JobOp::Ingest, "key-1", 5)
            .unwrap();
        assert_eq!(job.state, JobState::Pending);
        assert_eq!(job.attempts, 0);
        job.id.clone()
    };
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let job = store.get(&JobId::new(id)).unwrap().unwrap();
    assert_eq!(job.state, JobState::Pending);
    assert_eq!(job.max_attempts, 5);
}

#[test]
fn job_store_marks_inflight_as_failed_on_reopen() {
    let dir = tmp("store-crash");
    let coll = CollectionId::new("kb").unwrap();
    let id = {
        let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
        let mut job = store
            .create_job(&coll, "src", JobOp::Ingest, "key-1", 5)
            .unwrap();
        // Simulate a crash mid-execution.
        job.state = JobState::Running;
        job.attempts = 1;
        store.update(&job).unwrap();
        job.id.clone()
    };
    // Reopen = restart: the in-flight job must fail loudly, never silently
    // vanish and never pretend to have completed.
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let job = store.get(&JobId::new(id)).unwrap().unwrap();
    assert_eq!(job.state, JobState::Failed);
    let err = job.error.unwrap();
    assert!(err.message.contains("resubmit"), "got: {}", err.message);
}

#[test]
fn find_or_create_dedups_inflight_only() {
    let dir = tmp("store-idem");
    let coll = CollectionId::new("kb").unwrap();
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let (a, created_a) = store
        .find_or_create(&coll, "src", JobOp::Ingest, "key-1", 5)
        .unwrap();
    assert!(created_a);
    let (b, created_b) = store
        .find_or_create(&coll, "src", JobOp::Ingest, "key-1", 5)
        .unwrap();
    assert!(
        !created_b,
        "in-flight duplicate must return the existing job"
    );
    assert_eq!(a.id, b.id);
    // Terminal jobs do not block resubmission.
    let mut done = b.clone();
    done.state = JobState::Done;
    store.update(&done).unwrap();
    let (c, created_c) = store
        .find_or_create(&coll, "src", JobOp::Ingest, "key-1", 5)
        .unwrap();
    assert!(created_c);
    assert_ne!(a.id, c.id);
}

// ---------------------------------------------------------------------------
// runner: end-to-end acceptance
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ingest_end_to_end_bytes_to_searchable_vectors() {
    let dir = tmp("e2e");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));

    let html = "<html><body><h1>Rust Guide</h1><p>Ownership and borrowing.</p></body></html>";
    let mut req = ingest_req("https://example.com/rust", html);
    req.filename = Some("rust.html".to_string());
    req.mime = Some("text/html".to_string());
    let id = runner.try_submit_ingest(req).unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done, "error: {:?}", job.error);
    let affected = job.affected.unwrap();
    assert_eq!(affected.version, 1);
    assert!(!affected.skipped);
    assert!(affected.chunks > 0 && affected.vectors == affected.chunks);

    // The document is stored with version 1 and full provenance.
    let doc_id = DocumentId::for_source(
        &CollectionId::new("kb").unwrap(),
        "https://example.com/rust",
    );
    let store = opener.raw(&CollectionId::new("kb").unwrap());
    let (doc, chunks) = store
        .get_document(&doc_id)
        .unwrap()
        .expect("document must exist");
    assert_eq!(doc.version, 1);
    assert_eq!(doc.content_hash, {
        let canon = polyvec_ingestion::normalize_markdown(
            &MarkitConverter::new()
                .convert(
                    html.as_bytes(),
                    &polyvec_ingestion::SourceHint {
                        filename: Some("rust.html".to_string()),
                        mime: Some("text/html".to_string()),
                        extension: Some("html".to_string()),
                    },
                )
                .unwrap()
                .markdown,
        );
        polyvec_core::ContentHash::of(canon.as_bytes())
    });

    // Searchable: embedding the exact chunk text finds its vector.
    let provider = StubEmbeddingProvider::new(DIM).unwrap();
    let q = provider
        .embed(&[chunks[0].text.as_str()])
        .unwrap()
        .remove(0);
    let hits = store.search(&q, 5, 32);
    let want = polyvec_core::VectorId::for_chunk(&chunks[0].id);
    assert!(
        hits.iter().any(|h| h.vector_id == want),
        "chunk vector must be searchable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partial_write_is_completed_not_reported_skipped() {
    // A previous attempt stored the document + chunks but the vectors never
    // landed (crash between transactions). Re-ingesting the same content
    // must FINISH the work at the same version — never report "skipped"
    // for a document with no searchable vectors.
    let dir = tmp("partial");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let content = "# Partial\n\nAlpha beta gamma delta.";

    let id = runner
        .try_submit_ingest(ingest_req("kb/guide.md", content))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.affected.as_ref().unwrap().version, 1);

    // Simulate the partial write: remove the vectors, keep doc + chunks +
    // the job store's doc record.
    let coll = CollectionId::new("kb").unwrap();
    let raw = opener.raw(&coll);
    let doc_id = DocumentId::for_source(&coll, "kb/guide.md");
    let (_doc, chunks) = raw.store.lock().unwrap().get_document(&doc_id).unwrap();
    assert!(!chunks.is_empty());
    {
        let mut store = raw.store.lock().unwrap();
        for chunk in &chunks {
            store
                .delete_vector(&VectorId::for_chunk(&chunk.id))
                .unwrap();
        }
        assert!(!store.vectors_present(&chunks).unwrap());
    }

    // Re-ingest identical content: the pipeline must detect the missing
    // vectors and complete version 1 (not bump to 2, not skip).
    let id2 = runner
        .try_submit_ingest(ingest_req("kb/guide.md", content))
        .unwrap();
    let job2 = wait_done(&runner, &id2).await;
    assert_eq!(job2.state, JobState::Done, "error: {:?}", job2.error);
    let affected = job2.affected.as_ref().unwrap();
    assert!(
        !affected.skipped,
        "partial write must not be reported skipped"
    );
    assert_eq!(affected.version, 1, "must reuse the version, not bump");

    // And the document is searchable again.
    let provider = stub_provider();
    let query = provider.embed(&["alpha beta gamma"]).unwrap().remove(0);
    assert!(!raw.search(&query, 5, 32).is_empty());
    runner.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reingest_unchanged_content_creates_no_duplicates() {
    let dir = tmp("skip");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));

    let content = "stable content\nsecond line\n";
    let id1 = runner
        .try_submit_ingest(ingest_req("src://stable", content))
        .unwrap();
    let job1 = wait_done(&runner, &id1).await;
    assert_eq!(job1.state, JobState::Done);
    assert!(!job1.affected.as_ref().unwrap().skipped);

    // Same bytes again (after the first job finished): new job, but the
    // pipeline detects unchanged content and skips — no new version, no new
    // vectors.
    let id2 = runner
        .try_submit_ingest(ingest_req("src://stable", content))
        .unwrap();
    assert_ne!(id1, id2, "terminal jobs do not block resubmission");
    let job2 = wait_done(&runner, &id2).await;
    assert_eq!(job2.state, JobState::Done);
    let affected = job2.affected.unwrap();
    assert!(affected.skipped, "unchanged content must skip");
    assert_eq!(affected.version, 1);

    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://stable");
    let store = opener.raw(&CollectionId::new("kb").unwrap());
    let (doc, _) = store.get_document(&doc_id).unwrap().unwrap();
    assert_eq!(doc.version, 1, "no version bump on unchanged re-ingest");
    let stats = store.stats();
    assert_eq!(stats.documents, 1, "no duplicate logical records");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_inflight_submit_returns_same_job() {
    let dir = tmp("dedup");
    let (runner, _opener) = spawn_runner(&dir, Some(stub_provider()));

    // Two rapid submits with an explicit shared key: the second must attach
    // to the in-flight job, not duplicate the work.
    let mut r1 = ingest_req("src://race", "racing content here");
    r1.idempotency_key = Some("shared-key".to_string());
    let mut r2 = ingest_req("src://race", "racing content here");
    r2.idempotency_key = Some("shared-key".to_string());
    let id1 = runner.try_submit_ingest(r1).unwrap();
    let id2 = runner.try_submit_ingest(r2).unwrap();
    assert_eq!(id1, id2);
    let job = wait_done(&runner, &id1).await;
    assert_eq!(job.state, JobState::Done);
    assert_eq!(
        runner
            .list_jobs(&CollectionId::new("kb").unwrap(), 100)
            .unwrap()
            .len(),
        1,
        "exactly one job record for the duplicated submit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_bumps_version_and_tombstones_old_vectors() {
    let dir = tmp("update");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let provider = StubEmbeddingProvider::new(DIM).unwrap();

    let id1 = runner
        .try_submit_ingest(ingest_req(
            "src://changing",
            "the quick brown fox jumps over",
        ))
        .unwrap();
    assert_eq!(wait_done(&runner, &id1).await.state, JobState::Done);

    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://changing");
    let store = opener.raw(&CollectionId::new("kb").unwrap());
    let (_, chunks_v1) = store.get_document(&doc_id).unwrap().unwrap();
    let old_vector_ids: Vec<_> = chunks_v1
        .iter()
        .map(|c| polyvec_core::VectorId::for_chunk(&c.id))
        .collect();
    let old_query = provider
        .embed(&[chunks_v1[0].text.as_str()])
        .unwrap()
        .remove(0);

    // Changed content → version bump.
    let id2 = runner
        .try_submit_ingest(ingest_req(
            "src://changing",
            "zebra stripes in the savanna dusk",
        ))
        .unwrap();
    let job2 = wait_done(&runner, &id2).await;
    assert_eq!(job2.state, JobState::Done);
    assert_eq!(job2.affected.unwrap().version, 2);

    // Old evidence is unsearchable immediately.
    let hits = store.search(&old_query, 10, 32);
    assert!(
        !hits.iter().any(|h| old_vector_ids.contains(&h.vector_id)),
        "old version vectors must be tombstoned"
    );
    // New content is searchable.
    let (_, chunks_v2) = store.get_document(&doc_id).unwrap().unwrap();
    assert_eq!(chunks_v2.len(), chunks_v1.len());
    assert_ne!(
        chunks_v2[0].id, chunks_v1[0].id,
        "fresh chunk ids per version"
    );
    let new_query = provider
        .embed(&[chunks_v2[0].text.as_str()])
        .unwrap()
        .remove(0);
    let hits = store.search(&new_query, 10, 32);
    let want = polyvec_core::VectorId::for_chunk(&chunks_v2[0].id);
    assert!(hits.iter().any(|h| h.vector_id == want));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_cascades_to_chunks_and_vectors() {
    let dir = tmp("delete");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let provider = StubEmbeddingProvider::new(DIM).unwrap();

    let id1 = runner
        .try_submit_ingest(ingest_req("src://doomed", "temporary knowledge base entry"))
        .unwrap();
    assert_eq!(wait_done(&runner, &id1).await.state, JobState::Done);

    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://doomed");
    let store = opener.raw(&CollectionId::new("kb").unwrap());
    let (_, chunks) = store.get_document(&doc_id).unwrap().unwrap();
    let query = provider
        .embed(&[chunks[0].text.as_str()])
        .unwrap()
        .remove(0);
    assert!(
        !store.search(&query, 5, 32).is_empty(),
        "sanity: vector searchable before delete"
    );

    let del = DeleteRequest {
        collection: CollectionId::new("kb").unwrap(),
        source: "src://doomed".to_string(),
        idempotency_key: None,
        max_attempts: None,
    };
    let id2 = runner.try_submit_delete(del).unwrap();
    assert_eq!(wait_done(&runner, &id2).await.state, JobState::Done);

    assert!(
        store.get_document(&doc_id).unwrap().is_none(),
        "document must be gone"
    );
    let hits = store.search(&query, 5, 32);
    assert!(
        hits.is_empty(),
        "deleted vectors must be unsearchable, got {hits:?}"
    );
    // Deleting again is a no-op success (idempotent).
    let del2 = DeleteRequest {
        collection: CollectionId::new("kb").unwrap(),
        source: "src://doomed".to_string(),
        idempotency_key: None,
        max_attempts: None,
    };
    let id3 = runner.try_submit_delete(del2).unwrap();
    let job3 = wait_done(&runner, &id3).await;
    assert_eq!(job3.state, JobState::Done);
    assert!(job3.affected.unwrap().skipped);
}

// ---------------------------------------------------------------------------
// runner: failure semantics
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_provider_failure_retries_then_fails_loudly() {
    let dir = tmp("fail-transient");
    // Always transient, budget 3 → attempts == 3, then Failed with detail.
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::new(FlakyProvider::new(u32::MAX, FlakyMode::Transient));
    let (runner, opener) = spawn_runner(&dir, Some(provider));

    let id = runner
        .try_submit_ingest(ingest_req("src://flaky", "will never embed"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed, "failed jobs stay failed");
    assert_eq!(job.attempts, 3, "bounded retries, no retry storm");
    let err = job.error.clone().unwrap();
    assert_eq!(err.kind, "transient");
    assert!(
        err.message.contains("flaky"),
        "error detail preserved, got: {}",
        err.message
    );

    // Partial progress is never reported complete: no document was stored.
    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://flaky");
    assert!(
        opener
            .raw(&CollectionId::new("kb").unwrap())
            .get_document(&doc_id)
            .unwrap()
            .is_none(),
        "nothing searchable may be left behind by a failed job"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permanent_provider_failure_does_not_retry() {
    let dir = tmp("fail-permanent");
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::new(FlakyProvider::new(u32::MAX, FlakyMode::Permanent));
    let (runner, _opener) = spawn_runner(&dir, Some(provider));

    let id = runner
        .try_submit_ingest(ingest_req("src://bad", "bad request"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.attempts, 1, "permanent failures must not be retried");
    assert_eq!(job.error.unwrap().kind, "permanent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transient_failure_then_success_recovers() {
    let dir = tmp("recover");
    // Fail twice transiently, then succeed.
    let provider: Arc<dyn EmbeddingProvider> =
        Arc::new(FlakyProvider::new(2, FlakyMode::Transient));
    let (runner, _opener) = spawn_runner(&dir, Some(provider));

    let id = runner
        .try_submit_ingest(ingest_req("src://rec", "recovers"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.attempts, 3, "two failures then success");
    assert!(job.error.is_none(), "error cleared on success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_full_is_backpressure_not_silent_drop() {
    let dir = tmp("backpressure");
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let runner = JobRunner::new(
        store,
        Some(stub_provider()),
        Arc::new(MarkitConverter::new()),
        RunnerLimits {
            queue_capacity: 1,
            ..test_limits()
        },
    );
    // No spawn: the queue is never drained, so filling is deterministic.
    // channel(1) buffers one job; the second try_send fails fast.
    let mk = |i: u32| {
        let mut r = ingest_req(&format!("src://q{i}"), "queued");
        r.idempotency_key = Some(format!("qkey-{i}"));
        r
    };
    assert!(runner.try_submit_ingest(mk(0)).is_ok());
    let err = runner.try_submit_ingest(mk(1)).unwrap_err();
    match err {
        SubmitError::QueueFull { retry_after_secs } => {
            assert!(retry_after_secs > 0);
        }
        other => panic!("expected QueueFull, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ingest_without_provider_or_vectors_fails_fast() {
    let dir = tmp("no-provider");
    let (runner, _opener) = spawn_runner(&dir, None);
    let id = runner
        .try_submit_ingest(ingest_req("src://np", "no one to embed"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed);
    let msg = job.error.unwrap().message;
    assert!(msg.contains("no embedding provider"), "got: {msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supplied_vectors_need_no_provider() {
    let dir = tmp("supplied");
    let (runner, opener) = spawn_runner(&dir, None);
    let provider = StubEmbeddingProvider::new(DIM).unwrap();

    // One chunk → one supplied vector. Chunking a tiny doc yields 1 chunk.
    let mut req = ingest_req("src://sup", "supplied vector content");
    let chunk_text = polyvec_ingestion::normalize_markdown("supplied vector content\n");
    let vec = provider.embed(&[chunk_text.as_str()]).unwrap().remove(0);
    req.vectors = Some(vec![vec]);

    let id = runner.try_submit_ingest(req).unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done, "error: {:?}", job.error);

    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://sup");
    let store = opener.raw(&CollectionId::new("kb").unwrap());
    let (_, chunks) = store.get_document(&doc_id).unwrap().unwrap();
    assert_eq!(chunks.len(), 1);
    let hits = store.search(
        &provider
            .embed(&[chunks[0].text.as_str()])
            .unwrap()
            .remove(0),
        5,
        32,
    );
    assert!(!hits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supplied_vector_count_mismatch_is_permanent() {
    let dir = tmp("supplied-mismatch");
    let (runner, _opener) = spawn_runner(&dir, None);
    let mut req = ingest_req("src://sup2", "some content");
    req.vectors = Some(vec![vec![0.0; DIM as usize]; 99]);
    let id = runner.try_submit_ingest(req).unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.attempts, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dimension_mismatch_is_rejected() {
    let dir = tmp("dim-mismatch");
    // Provider dim 16 vs collection dim 32.
    let provider: Arc<dyn EmbeddingProvider> = Arc::new(StubEmbeddingProvider::new(16).unwrap());
    let (runner, opener) = spawn_runner(&dir, Some(provider));
    let id = runner
        .try_submit_ingest(ingest_req("src://dim", "content"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed);
    assert!(job.error.unwrap().message.contains("dimension"),);
    let doc_id = DocumentId::for_source(&CollectionId::new("kb").unwrap(), "src://dim");
    assert!(
        opener
            .raw(&CollectionId::new("kb").unwrap())
            .get_document(&doc_id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_mismatch_is_rejected_after_first_claim() {
    let dir = tmp("model-mismatch");
    // One shared opener: both runners see the same collection (redb forbids
    // opening the same file twice in one process).
    let opener = Arc::new(TestOpener::new(dir.clone()));
    let store = JobStore::open(&dir.join("jobs.redb")).unwrap();
    let runner = JobRunner::new(
        store,
        Some(stub_provider()),
        Arc::new(MarkitConverter::new()),
        test_limits(),
    );
    assert!(runner.spawn(Arc::clone(&opener) as Arc<dyn CollectionOpener>));

    // First ingest claims the collection for the stub model.
    let id1 = runner
        .try_submit_ingest(ingest_req("src://m1", "first model"))
        .unwrap();
    assert_eq!(wait_done(&runner, &id1).await.state, JobState::Done);

    // A different model (same dimension) must be refused — never silently mixed.
    let other: Arc<dyn EmbeddingProvider> = Arc::new(RenamedProvider {
        stub: StubEmbeddingProvider::new(DIM).unwrap(),
    });
    let store2 = JobStore::open(&dir.join("jobs2.redb")).unwrap();
    let runner2 = JobRunner::new(
        store2,
        Some(other),
        Arc::new(MarkitConverter::new()),
        test_limits(),
    );
    assert!(runner2.spawn(Arc::clone(&opener) as Arc<dyn CollectionOpener>));
    let id2 = runner2
        .try_submit_ingest(ingest_req("src://m2", "second model"))
        .unwrap();
    let job2 = wait_done(&runner2, &id2).await;
    assert_eq!(job2.state, JobState::Failed);
    assert!(job2.error.unwrap().message.contains("model mismatch"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_collection_fails_job() {
    let dir = tmp("unknown-coll");
    let (runner, _opener) = spawn_runner(&dir, Some(stub_provider()));
    let mut req = ingest_req("src://x", "content");
    req.collection = CollectionId::new("nope").unwrap();
    let id = runner.try_submit_ingest(req).unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error.unwrap().kind, "permanent");
}

// ---------------------------------------------------------------------------
// runner: lexical wiring (Phase 5)
//
// The runner drives chunk-text indexing through the `DocStore` trait in the
// same job as vector indexing. These tests assert the wiring (call order
// and arguments); the tantivy behavior itself is tested in
// `polyvec-retrieval`, and the server-side propagation end-to-end at the
// HTTP level.
// ---------------------------------------------------------------------------

fn lexical_calls_of(opener: &TestOpener, coll: &CollectionId) -> Vec<LexicalCall> {
    opener.raw(coll).lexical_calls.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ingest_indexes_chunk_text_in_the_same_job() {
    let dir = tmp("lex-ingest");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let coll = CollectionId::new("kb").unwrap();

    let id = runner
        .try_submit_ingest(ingest_req("src://lex", "lexical hook content here"))
        .unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done);
    let affected = job.affected.unwrap();
    assert!(!affected.skipped);

    let doc_id = DocumentId::for_source(&coll, "src://lex")
        .as_str()
        .to_string();
    assert_eq!(
        lexical_calls_of(&opener, &coll),
        vec![LexicalCall::Index {
            doc_id: doc_id.clone(),
            chunks: usize::try_from(affected.chunks).unwrap(),
        }],
        "one lexical upsert per ingest, in the same job"
    );

    // Unchanged re-ingest skips everything — including the lexical hook
    // (nothing changed, so the lexical view is already correct).
    let id2 = runner
        .try_submit_ingest(ingest_req("src://lex", "lexical hook content here"))
        .unwrap();
    let job2 = wait_done(&runner, &id2).await;
    assert!(job2.affected.unwrap().skipped);
    assert_eq!(
        lexical_calls_of(&opener, &coll).len(),
        1,
        "skipped re-ingest must not re-index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_reindexes_changed_chunks() {
    let dir = tmp("lex-update");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let coll = CollectionId::new("kb").unwrap();

    let id1 = runner
        .try_submit_ingest(ingest_req("src://lexupd", "first version text"))
        .unwrap();
    assert_eq!(wait_done(&runner, &id1).await.state, JobState::Done);
    let id2 = runner
        .try_submit_ingest(ingest_req("src://lexupd", "second version text here"))
        .unwrap();
    let job2 = wait_done(&runner, &id2).await;
    assert_eq!(job2.state, JobState::Done);
    assert_eq!(job2.affected.unwrap().version, 2);

    let doc_id = DocumentId::for_source(&coll, "src://lexupd")
        .as_str()
        .to_string();
    let calls = lexical_calls_of(&opener, &coll);
    assert_eq!(calls.len(), 2, "update must re-index, got: {calls:?}");
    assert!(
        matches!(&calls[1], LexicalCall::Index { doc_id: d, .. } if d == &doc_id),
        "second call re-indexes the same document, got: {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_job_removes_lexical_entries() {
    let dir = tmp("lex-delete");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let coll = CollectionId::new("kb").unwrap();

    let id1 = runner
        .try_submit_ingest(ingest_req("src://lexdel", "doomed lexical content"))
        .unwrap();
    assert_eq!(wait_done(&runner, &id1).await.state, JobState::Done);

    let del = DeleteRequest {
        collection: coll.clone(),
        source: "src://lexdel".to_string(),
        idempotency_key: None,
        max_attempts: None,
    };
    let id2 = runner.try_submit_delete(del).unwrap();
    assert_eq!(wait_done(&runner, &id2).await.state, JobState::Done);

    let doc_id = DocumentId::for_source(&coll, "src://lexdel")
        .as_str()
        .to_string();
    let calls = lexical_calls_of(&opener, &coll);
    assert!(
        calls.iter().any(|c| *c
            == LexicalCall::Remove {
                doc_id: doc_id.clone()
            }),
        "delete must scrub the lexical index, got: {calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_of_missing_document_still_scrubs_lexical() {
    let dir = tmp("lex-delete-missing");
    let (runner, opener) = spawn_runner(&dir, Some(stub_provider()));
    let coll = CollectionId::new("kb").unwrap();

    // No ingest: the document never existed. The delete is a no-op
    // success, but the lexical scrub must still run (a previously failed
    // delete could have left ghost entries).
    let del = DeleteRequest {
        collection: coll.clone(),
        source: "src://never-there".to_string(),
        idempotency_key: None,
        max_attempts: None,
    };
    let id = runner.try_submit_delete(del).unwrap();
    let job = wait_done(&runner, &id).await;
    assert_eq!(job.state, JobState::Done);
    assert!(job.affected.unwrap().skipped);

    let doc_id = DocumentId::for_source(&coll, "src://never-there")
        .as_str()
        .to_string();
    assert_eq!(
        lexical_calls_of(&opener, &coll),
        vec![LexicalCall::Remove { doc_id }],
    );
}
