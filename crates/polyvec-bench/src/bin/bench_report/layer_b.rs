//! Layer B — knowledge-pipeline measurements.
//!
//! Two halves:
//!
//! 1. **Stage-direct** (`layer_b_sync`): convert → canonicalize → chunk →
//!    embed → atomic store write over 20 synthetic documents, timed per
//!    stage. Mirrors the runner's stage order; excludes job queueing, the
//!    lexical-index write, and the durable doc-record write (documented).
//! 2. **Runner-driven** (`layer_b_runner`, async): real `JobRunner` end to
//!    end — per-doc job time, idempotent re-ingest skip, update propagation,
//!    delete propagation, and retry behavior under a failing provider.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use polyvec_core::{
    Chunk, CollectionConfig, CollectionId, DistanceMetric, Document, DocumentId, Metadata,
    ModelTag, Provenance, Vector, VectorId,
};
use polyvec_embeddings::{
    EmbedError, EmbedResult, EmbeddingProvider, ErrorKind, StubEmbeddingProvider,
};
use polyvec_ingestion::{
    Chunker, CollectionOpener, Converter, DeleteRequest, IngestRequest, JobRunner, JobState,
    MarkitConverter, SourceHint, canonicalize,
};
use polyvec_storage::CollectionStore;
use serde_json::{Map, Value, json};

use super::common::{now_unix_secs, scratch, test_config};
use polyvec_bench::fixtures::synthetic_corpus;
use polyvec_server::{Registry, ServerConfig, ServerOpener, build_runner};

// ---------------------------------------------------------------------------
// 1. stage-direct pipeline timing
// ---------------------------------------------------------------------------

/// Conversion success by format over the committed fixtures.
fn bench_convert_by_format() -> Value {
    let converter = MarkitConverter::new();
    let cases = [
        ("markdown", "sample.md", "text/markdown", "md"),
        ("plain text", "sample.txt", "text/plain", "txt"),
        ("html", "sample.html", "text/html", "html"),
    ];
    let mut rows = Vec::new();
    for (name, file, mime, ext) in cases {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(file);
        let bytes = std::fs::read(&path).unwrap();
        let hint = SourceHint {
            filename: Some(file.to_string()),
            mime: Some(mime.to_string()),
            extension: Some(ext.to_string()),
        };
        let t0 = Instant::now();
        let result = converter.convert(&bytes, &hint);
        let dt = t0.elapsed();
        match result {
            Ok(out) => rows.push(json!({
                "format": name, "success": true,
                "seconds": dt.as_secs_f64(),
                "markdown_chars": out.markdown.len(),
                "source_format": out.conversion.source_format,
            })),
            Err(e) => rows.push(json!({
                "format": name, "success": false, "error": format!("{e:?}"),
            })),
        }
    }
    Value::Array(rows)
}

/// Per-document pipeline timing, stage by stage.
fn bench_pipeline_per_doc() -> Value {
    let dim = 64u32;
    let dir = scratch("pipeline");
    let mut store = CollectionStore::create(&dir, test_config(dim)).unwrap();
    let converter = MarkitConverter::new();
    let chunker = Chunker::new(polyvec_core::ChunkingConfig::default()).unwrap();
    let provider = StubEmbeddingProvider::new(dim).unwrap();

    let docs = synthetic_corpus(20, 0xB1);
    let (mut t_convert, mut t_canon, mut t_chunk, mut t_embed, mut t_store) = (
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    );

    for (i, (source, text)) in docs.iter().enumerate() {
        let bytes = text.as_bytes();

        let t0 = Instant::now();
        let hint = SourceHint {
            filename: Some(format!("doc-{i}.md")),
            mime: Some("text/markdown".to_string()),
            extension: Some("md".to_string()),
        };
        let conv = converter.convert(bytes, &hint).unwrap();
        t_convert += t0.elapsed();

        let t0 = Instant::now();
        let canonical = canonicalize(conv, source, Some(&format!("doc-{i}.md")), bytes);
        t_canon += t0.elapsed();

        let t0 = Instant::now();
        let raw = chunker.chunk(&canonical.markdown);
        assert!(!raw.is_empty());
        t_chunk += t0.elapsed();

        let t0 = Instant::now();
        let texts: Vec<&str> = raw.iter().map(|r| r.text.as_str()).collect();
        let vectors = provider.embed(&texts).unwrap();
        t_embed += t0.elapsed();

        let t0 = Instant::now();
        let collection = CollectionId::new("bench").unwrap();
        let document_id = DocumentId::for_source(&collection, source);
        let doc = Document {
            id: document_id.clone(),
            collection: collection.clone(),
            source: source.clone(),
            metadata: Metadata::default(),
            content_hash: canonical.canonical_hash,
            version: 1,
            provenance: Provenance::Ingested {
                source: "bench:bench_report".to_string(),
                at_secs: now_unix_secs(),
            },
        };
        let chunks: Vec<Chunk> = raw
            .iter()
            .enumerate()
            .map(|(j, r)| {
                Chunk::new_versioned(
                    document_id.clone(),
                    1,
                    u32::try_from(j).unwrap_or(u32::MAX),
                    &r.text,
                    r.char_start,
                    r.char_end,
                )
            })
            .collect();
        let typed: Vec<Vector> = chunks
            .iter()
            .zip(vectors)
            .map(|(chunk, values)| {
                Vector::new(VectorId::for_chunk(&chunk.id), values, dim).unwrap()
            })
            .collect();
        store.ingest_document(&doc, &chunks, &typed).unwrap();
        t_store += t0.elapsed();
    }

    let n = docs.len();
    #[allow(clippy::cast_precision_loss)] // n is 20; exact in f64
    let n_f = n as f64;
    let total = t_convert + t_canon + t_chunk + t_embed + t_store;
    json!({
        "documents": n,
        "seconds_total": total.as_secs_f64(),
        "seconds_per_doc": total.as_secs_f64() / n_f,
        "stages": {
            "convert_avg_ms": t_convert.as_secs_f64() * 1000.0 / n_f,
            "canonicalize_avg_ms": t_canon.as_secs_f64() * 1000.0 / n_f,
            "chunk_avg_ms": t_chunk.as_secs_f64() * 1000.0 / n_f,
            "embed_stub_avg_ms": t_embed.as_secs_f64() * 1000.0 / n_f,
            "store_write_avg_ms": t_store.as_secs_f64() * 1000.0 / n_f,
        },
        "note": "excludes job queueing, lexical-index write, doc-record write (see runner measurements)",
    })
}

// ---------------------------------------------------------------------------
// 2. runner-driven measurements
// ---------------------------------------------------------------------------

/// Embedding provider with scripted failures, selected by source name.
/// One model tag for every collection, so the model-stamp check never
/// interferes with the retry measurement.
struct ScenarioProvider {
    stub: StubEmbeddingProvider,
    transient_failures: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

impl ScenarioProvider {
    fn new(dim: u32) -> Self {
        Self {
            stub: StubEmbeddingProvider::new(dim).unwrap(),
            transient_failures: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl EmbeddingProvider for ScenarioProvider {
    fn embed(&self, texts: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
        // The scenario is smuggled via the first text's source marker.
        // (The runner embeds chunk texts; the bench prefixes a marker.)
        let marker = texts.first().copied().unwrap_or("");
        if marker.starts_with("SCENARIO:flaky-transient:") {
            let mut map = self
                .transient_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let left = map.entry(marker.to_string()).or_insert(2);
            if *left > 0 {
                *left -= 1;
                return Err(EmbedError {
                    kind: ErrorKind::Transient,
                    message: "scripted transient failure".to_string(),
                    retry_after: None,
                });
            }
        } else if marker.starts_with("SCENARIO:fail-permanent:") {
            return Err(EmbedError {
                kind: ErrorKind::Permanent,
                message: "scripted permanent failure".to_string(),
                retry_after: None,
            });
        }
        self.stub.embed(texts)
    }

    fn model_id(&self) -> ModelTag {
        ModelTag::new("bench-scenario", "0").expect("static model tag")
    }

    fn dimension(&self) -> u32 {
        self.stub.dimension()
    }
}

struct RunnerCtx {
    runner: JobRunner,
    registry: Arc<Registry>,
    collection: CollectionId,
    stub: StubEmbeddingProvider,
}

impl RunnerCtx {
    fn new(dir_name: &str, collection: &str) -> Self {
        let data_dir = scratch(dir_name);
        let registry = Arc::new(Registry::new(data_dir.clone()));
        let collection = CollectionId::new(collection).unwrap();
        registry
            .create(CollectionConfig::new(collection.clone(), 64, DistanceMetric::Cosine).unwrap())
            .unwrap();
        let config = ServerConfig::from_env_in(&[
            ("POLYVEC_DATA_DIR", data_dir.to_str().unwrap()),
            ("POLYVEC_JOB_CONCURRENCY", "2"),
        ])
        .unwrap();
        let provider: Arc<dyn EmbeddingProvider> = Arc::new(ScenarioProvider::new(64));
        let runner = build_runner(&config, Some(provider)).unwrap();
        let opener: Arc<dyn CollectionOpener> = Arc::new(ServerOpener::new(Arc::clone(&registry)));
        assert!(runner.spawn(opener), "runner accept loop spawns");
        Self {
            runner,
            registry,
            collection,
            stub: StubEmbeddingProvider::new(64).unwrap(),
        }
    }

    /// Submits an ingest and polls until terminal.
    ///
    /// Returns `(job_id, attempts, wall_seconds, final_state, affected_json)`.
    async fn run_ingest(
        &self,
        source: &str,
        text: &str,
        max_attempts: Option<u32>,
    ) -> (String, u32, f64, String, Value) {
        let t0 = Instant::now();
        let job_id = self
            .runner
            .try_submit_ingest(IngestRequest {
                collection: self.collection.clone(),
                source: source.to_string(),
                filename: Some("doc.md".to_string()),
                mime: Some("text/markdown".to_string()),
                content: text.as_bytes().to_vec(),
                metadata: Metadata::default(),
                vectors: None,
                idempotency_key: None,
                max_attempts,
            })
            .expect("submit");
        let job = self.poll_job(&job_id).await;
        let dt = t0.elapsed().as_secs_f64();
        let affected = job.affected.as_ref().map_or(Value::Null, |a| {
            json!({
                "document_id": a.document_id,
                "version": a.version,
                "chunks": a.chunks,
                "vectors": a.vectors,
                "skipped": a.skipped,
            })
        });
        (
            job_id.to_string(),
            job.attempts,
            dt,
            format!("{:?}", job.state),
            affected,
        )
    }

    async fn poll_job(&self, job_id: &polyvec_ingestion::JobId) -> polyvec_ingestion::IngestionJob {
        let start = Instant::now();
        loop {
            let job = self
                .runner
                .get_job(job_id)
                .expect("get_job")
                .expect("job exists");
            match job.state {
                JobState::Done | JobState::Failed => return job,
                _ => {
                    assert!(
                        start.elapsed() < Duration::from_secs(120),
                        "job {job_id} stuck in {:?}",
                        job.state
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn shutdown(self) {
        self.runner.shutdown().await;
    }
}

/// Real `JobRunner` end to end: per-doc time, idempotent skip, update,
/// delete propagation, and retry behavior under scripted provider failures.
static DOC_SEQ: AtomicU64 = AtomicU64::new(0);

async fn bench_runner() -> Value {
    let mut out = Map::new();

    // --- ingest / re-ingest / update / delete on one collection ---
    {
        let ctx = RunnerCtx::new("runner-jobs", "kb");
        let doc_text =
            "# Bench doc\n\nPhotosynthesis converts light into chemical energy. ".repeat(20);

        // 1. fresh ingest
        let (_, attempts, secs, state, affected) =
            ctx.run_ingest("bench://doc-a", &doc_text, None).await;
        out.insert(
            "ingest_job_per_doc".to_string(),
            json!({"attempts": attempts, "seconds": secs, "state": state, "affected": affected}),
        );

        // 2. idempotent re-ingest (unchanged content → skipped, no new version)
        let t0 = Instant::now();
        let (_, attempts, _, state, affected) =
            ctx.run_ingest("bench://doc-a", &doc_text, None).await;
        out.insert(
            "reingest_skip".to_string(),
            json!({
                "seconds": t0.elapsed().as_secs_f64(),
                "attempts": attempts, "state": state, "affected": affected,
            }),
        );

        // 3. update propagation (changed content → version 2)
        let changed = format!("{doc_text}\n\nAn extra paragraph about mycorrhizae networks.");
        let (_, _, secs, state, affected) = ctx.run_ingest("bench://doc-a", &changed, None).await;
        out.insert(
            "update_propagates".to_string(),
            json!({"seconds": secs, "state": state, "affected": affected}),
        );

        // 4. delete propagation: job time + the document is really gone
        let t0 = Instant::now();
        let job_id = ctx
            .runner
            .try_submit_delete(DeleteRequest {
                collection: ctx.collection.clone(),
                source: "bench://doc-a".to_string(),
                idempotency_key: None,
                max_attempts: None,
            })
            .expect("submit delete");
        let job = ctx.poll_job(&job_id).await;
        let delete_secs = t0.elapsed().as_secs_f64();
        assert!(matches!(job.state, JobState::Done));
        // Scope the store lock: the guard must not be held across the
        // shutdown await below.
        let (doc_gone, live_after, hits_after) = {
            let store = ctx.registry.get_or_open(&ctx.collection).unwrap();
            let store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let doc_id = DocumentId::for_source(&ctx.collection, "bench://doc-a");
            let doc_gone = store.get_document(&doc_id).is_err();
            // The deleted doc's vectors are unsearchable: embed a chunk text
            // and confirm the search finds nothing from the deleted document.
            let prefix: String = changed.chars().take(200).collect();
            let q = ctx.stub.embed(&[prefix.as_str()]).unwrap().pop().unwrap();
            let hits = store.search(&q, 5, 100).unwrap();
            (doc_gone, store.stats().live_vectors, hits.len())
        };
        out.insert(
            "delete_propagates".to_string(),
            json!({
                "seconds": delete_secs,
                "document_gone": doc_gone,
                "live_vectors_after": live_after,
                "search_hits_after_delete": hits_after,
            }),
        );
        ctx.shutdown().await;
    }

    // --- retry: transient failures then success ---
    {
        let ctx = RunnerCtx::new("runner-flaky", "kb");
        let n = DOC_SEQ.fetch_add(1, Ordering::SeqCst);
        // The marker rides in the chunk text so the provider sees the scenario.
        let text = format!("SCENARIO:flaky-transient:{n}\n\n# Doc\n\nBody text here. ").repeat(10);
        let (_, attempts, secs, state, _) = ctx.run_ingest("bench://flaky", &text, Some(5)).await;
        out.insert(
            "retry_transient_then_success".to_string(),
            json!({
                "scripted_transient_failures": 2,
                "attempts": attempts, "seconds": secs, "state": state,
                "note": "attempts==3 and ~3s backoff (1s+2s) expected",
            }),
        );
        ctx.shutdown().await;
    }

    // --- retry: permanent failure fails fast (no retries) ---
    {
        let ctx = RunnerCtx::new("runner-permfail", "kb");
        let n = DOC_SEQ.fetch_add(1, Ordering::SeqCst);
        let text = format!("SCENARIO:fail-permanent:{n}\n\n# Doc\n\nBody text here. ").repeat(10);
        let (_, attempts, secs, state, _) =
            ctx.run_ingest("bench://permfail", &text, Some(3)).await;
        out.insert(
            "retry_permanent_fails_fast".to_string(),
            json!({
                "attempts": attempts, "seconds": secs, "state": state,
                "note": "attempts==1 expected: permanent errors are never retried",
            }),
        );
        ctx.shutdown().await;
    }

    Value::Object(out)
}

/// Layer B: stage-direct timings + conversion table + runner measurements.
pub async fn layer_b() -> Value {
    let mut out = Map::new();
    out.insert("convert_by_format".to_string(), bench_convert_by_format());
    out.insert("pipeline_per_doc".to_string(), bench_pipeline_per_doc());
    out.insert("runner".to_string(), bench_runner().await);
    Value::Object(out)
}
