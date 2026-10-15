//! Layer C — end-to-end retrieval over HTTP against the real server binary.
//!
//! Spawns `polyvec-server` (found next to this binary), ingests a synthetic
//! corpus, and measures: time-to-first-useful-result, self-query MRR /
//! recall@5 / nDCG@5, evidence/provenance spot-check, and restart durability.
//!
//! The stub provider is a hash function: identical text → identical vector.
//! Queries are *exact chunk texts*, so rank 1 is expected by construction.
//! This measures pipeline mechanics, NOT semantic retrieval quality — see
//! `BENCHMARKS.md`.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use super::common::scratch;
use polyvec_bench::fixtures::synthetic_corpus;
use polyvec_core::{CollectionId, DocumentId};

/// A spawned server. Killed (SIGKILL) on drop; use `terminate_gracefully`
/// when the SIGTERM path itself is under test.
struct Server {
    child: Child,
    base: String,
    data_dir: PathBuf,
    agent: ureq::Agent,
}

impl Server {
    fn spawn() -> Option<Self> {
        let data_dir = scratch("layer-c");
        Self::spawn_on(data_dir)
    }

    fn spawn_on(data_dir: PathBuf) -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let bin = exe.parent()?.join("polyvec-server");
        if !bin.exists() {
            return None;
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        let port = listener.local_addr().ok()?.port();
        drop(listener);

        let log_path = data_dir.join("server.log");
        let out = std::fs::File::create(&log_path).ok()?;
        let err = out.try_clone().ok()?;
        let mut child = Command::new(&bin)
            .env("POLYVEC_BIND", format!("127.0.0.1:{port}"))
            .env("POLYVEC_DATA_DIR", &data_dir)
            .env("POLYVEC_EMBEDDING_PROVIDER", "stub")
            .env("POLYVEC_STUB_DIMENSION", "64")
            .env("POLYVEC_JOB_CONCURRENCY", "4")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .ok()?;
        let agent = ureq::Agent::new_with_defaults();
        let base = format!("http://127.0.0.1:{port}");
        let start = Instant::now();
        loop {
            match agent.get(&format!("{base}/healthz")).call() {
                Ok(resp) if resp.status().as_u16() == 200 => break,
                _ => {
                    if start.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        return None;
                    }
                    if child.try_wait().ok().flatten().is_some() {
                        return None; // died during startup; see server.log
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Some(Self {
            child,
            base,
            data_dir,
            agent,
        })
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        match self
            .agent
            .post(&format!("{}{path}", self.base))
            .send_json(body)
        {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let mut body = resp.into_body();
                let json = body.read_json::<Value>().unwrap_or(Value::Null);
                (status, json)
            }
            Err(ureq::Error::StatusCode(status)) => (status, Value::Null),
            Err(e) => panic!("POST {path} transport error: {e}"),
        }
    }

    fn get(&self, path: &str) -> (u16, Value) {
        match self.agent.get(&format!("{}{path}", self.base)).call() {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let mut body = resp.into_body();
                let json = body.read_json::<Value>().unwrap_or(Value::Null);
                (status, json)
            }
            Err(ureq::Error::StatusCode(status)) => (status, Value::Null),
            Err(e) => panic!("GET {path} transport error: {e}"),
        }
    }

    /// Ingests one text document and polls the job until terminal.
    /// Returns (job wall seconds, document id).
    fn ingest_text(&self, collection: &str, source: &str, text: &str) -> (f64, String) {
        let t0 = Instant::now();
        let (status, body) = self.post(
            &format!("/v1/collections/{collection}/ingest"),
            json!({"source": source, "content": {"text": text}}),
        );
        assert_eq!(status, 202, "ingest {source}: {body}");
        let job_id = body["job_id"].as_str().expect("job_id").to_string();
        let start = Instant::now();
        let state = loop {
            let (status, body) = self.get(&format!("/v1/jobs/{job_id}"));
            assert_eq!(status, 200, "get job: {body}");
            let state = body["state"].as_str().unwrap_or("?").to_string();
            if state == "done" || state == "failed" {
                break state;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "job {job_id} stuck in {state}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(state, "done", "job {job_id} failed");
        let doc_id = DocumentId::for_source(&CollectionId::new(collection).unwrap(), source)
            .as_str()
            .to_string();
        (t0.elapsed().as_secs_f64(), doc_id)
    }

    /// First chunk (id, text) of a document.
    fn first_chunk(&self, collection: &str, doc_id: &str) -> (String, String) {
        let (status, body) = self.get(&format!("/v1/collections/{collection}/documents/{doc_id}"));
        assert_eq!(status, 200, "get document: {body}");
        let chunk = &body["chunks"][0];
        (
            chunk["id"].as_str().expect("chunk id").to_string(),
            chunk["text"].as_str().expect("chunk text").to_string(),
        )
    }

    /// Vector-mode query with exact text; returns ranked chunk ids.
    fn query_chunk_ids(&self, collection: &str, text: &str, k: usize) -> Vec<String> {
        let (status, body) = self.post(
            &format!("/v1/collections/{collection}/query"),
            json!({"text": text, "k": k, "mode": "vector"}),
        );
        assert_eq!(status, 200, "query: {body}");
        body["hits"]
            .as_array()
            .expect("hits array")
            .iter()
            .map(|h| {
                h["chunk_id"]
                    .as_str()
                    .expect("chunk_id on vector hit")
                    .to_string()
            })
            .collect()
    }

    /// SIGTERM (graceful: the server flushes on shutdown), wait for exit.
    fn terminate_gracefully(&mut self) -> bool {
        let pid = self.child.id();
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output();
        let start = Instant::now();
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            if start.elapsed() > Duration::from_secs(15) {
                let _ = self.child.kill();
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Layer C over HTTP. Without a server binary next to this one, reports
/// `skipped` with the build command — never a fake number.
#[allow(clippy::too_many_lines)] // sequential end-to-end script; split would obscure the flow
pub fn layer_c() -> Value {
    let Some(mut server) = Server::spawn() else {
        return json!({
            "skipped": true,
            "reason": "polyvec-server binary not found next to bench_report; run: cargo build -p polyvec-server --bin polyvec-server",
        });
    };
    let mut out = Map::new();
    let collection = "e2e";

    let (status, _) = server.post(
        "/v1/collections",
        json!({"id": collection, "dimension": 64, "metric": "cosine"}),
    );
    assert_eq!(status, 201, "create collection");

    // C1: time-to-first-useful-result — ingest one doc, poll, then query
    // with the exact first-chunk text until the expected chunk is rank 1.
    let doc_text =
        "# TTFUR\n\nPhotosynthesis converts sunlight into chemical energy stored as sugars. "
            .repeat(10);
    let t0 = Instant::now();
    let (_, doc_id) = server.ingest_text(collection, "bench://ttfur", &doc_text);
    let (chunk_id, chunk_text) = server.first_chunk(collection, &doc_id);
    let ranked = server.query_chunk_ids(collection, &chunk_text, 5);
    let ttful = t0.elapsed().as_secs_f64();
    out.insert(
        "time_to_first_useful_result_sec".to_string(),
        json!({
            "seconds": ttful,
            "expected_chunk_rank_1": ranked.first().is_some_and(|id| id == &chunk_id),
        }),
    );

    // C2: corpus — 30 docs, one self-query each (exact first-chunk text).
    let corpus = synthetic_corpus(30, 0xC1);
    let mut sources = Vec::new();
    for (source, text) in &corpus {
        let (_, doc_id) = server.ingest_text(collection, source, text);
        sources.push((source.clone(), doc_id));
    }
    let mut ranks = Vec::new();
    let mut evidence_violations = 0u64;
    let mut query_ms = Vec::new();
    for (source, doc_id) in &sources {
        let (chunk_id, chunk_text) = server.first_chunk(collection, doc_id);
        let t0 = Instant::now();
        let (status, body) = server.post(
            &format!("/v1/collections/{collection}/query"),
            json!({"text": chunk_text, "k": 5, "mode": "vector"}),
        );
        query_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(status, 200, "query: {body}");
        let hits = body["hits"].as_array().expect("hits").clone();
        // evidence/provenance spot-check on every hit
        for h in &hits {
            let ok = h["chunk_id"].as_str().is_some_and(|s| !s.is_empty())
                && h["document_id"].as_str().is_some_and(|s| !s.is_empty())
                && h["source"].as_str().is_some_and(|s| !s.is_empty())
                && h["text"].as_str().is_some_and(|s| !s.is_empty());
            if !ok {
                evidence_violations += 1;
            }
        }
        let rank = hits
            .iter()
            .position(|h| h["chunk_id"].as_str() == Some(chunk_id.as_str()))
            .map(|p| p + 1);
        ranks.push((rank, source.clone()));
    }
    #[allow(clippy::cast_precision_loss)]
    let mrr = ranks
        .iter()
        .map(|(r, _)| r.map_or(0.0, |p| 1.0 / p as f64))
        .sum::<f64>()
        / ranks.len() as f64;
    #[allow(clippy::cast_precision_loss)]
    let recall_at_5 = ranks
        .iter()
        .filter(|(r, _)| r.is_some_and(|p| p <= 5))
        .count() as f64
        / ranks.len() as f64;
    #[allow(clippy::cast_precision_loss)]
    let ndcg_at_5 = ranks
        .iter()
        .map(|(r, _)| match r {
            Some(p) if *p <= 5 => 1.0 / ((*p as f64) + 1.0).log2(),
            _ => 0.0,
        })
        .sum::<f64>()
        / ranks.len() as f64;
    query_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = query_ms[query_ms.len() / 2];
    out.insert(
        "self_query_retrieval".to_string(),
        json!({
            "queries": ranks.len(),
            "k": 5,
            "mrr": mrr,
            "recall_at_5": recall_at_5,
            "ndcg_at_5": ndcg_at_5,
            "rank_1_count": ranks.iter().filter(|(r, _)| *r == Some(1)).count(),
            "query_p50_ms": p50,
            "evidence_violations": evidence_violations,
            "note": "queries are exact chunk texts; rank 1 expected by construction (stub provider is a hash). Mechanics check only.",
        }),
    );

    // C3: restart durability — SIGTERM, respawn on the same data dir, the
    // corpus is still searchable.
    let data_dir = server.data_dir.clone();
    let exited = server.terminate_gracefully();
    let mut restart_ok = false;
    if exited {
        if let Some(server2) = Server::spawn_on(data_dir) {
            let (source, doc_id) = &sources[0];
            let _ = source;
            let (chunk_id, chunk_text) = server2.first_chunk(collection, doc_id);
            let ranked = server2.query_chunk_ids(collection, &chunk_text, 5);
            restart_ok = ranked.first().is_some_and(|id| id == &chunk_id);
        }
    }
    out.insert(
        "restart_durability".to_string(),
        json!({
            "graceful_sigterm_exit": exited,
            "corpus_searchable_after_restart": restart_ok,
        }),
    );

    Value::Object(out)
}
