//! Provider-interface tests: stub determinism and the remote HTTP adapter
//! against a local mock server (no external network).
//!
//! RED: these tests define the intended API; they fail to compile until
//! `polyvec-embeddings` implements it.

use polyvec_core::ModelTag;
use polyvec_embeddings::{EmbedError, EmbeddingProvider, ErrorKind, RemoteEmbeddingProvider, StubEmbeddingProvider};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// stub provider
// ---------------------------------------------------------------------------

#[test]
fn stub_is_deterministic_and_l2_normalized() {
    let p = StubEmbeddingProvider::new(64).unwrap();
    let a = p.embed(&["hello world"]).unwrap();
    let b = p.embed(&["hello world"]).unwrap();
    assert_eq!(a, b, "same text must embed identically");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].len(), 64);
    let norm: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-5,
        "stub vectors are L2-normalized, got norm {norm}"
    );
    let c = p.embed(&["completely different text"]).unwrap();
    assert_ne!(a, c, "different texts should (almost surely) differ");
}

#[test]
fn stub_validates_dimension() {
    assert!(StubEmbeddingProvider::new(0).is_err());
    assert!(StubEmbeddingProvider::new(4097).is_err());
    assert!(StubEmbeddingProvider::new(4096).is_ok());
}

#[test]
fn stub_reports_its_model_tag() {
    let p = StubEmbeddingProvider::new(32).unwrap();
    assert_eq!(p.dimension(), 32);
    let tag = p.model_id();
    assert!(!tag.name.is_empty() && !tag.version.is_empty());
}

#[test]
fn stub_embeds_empty_input_without_work() {
    let p = StubEmbeddingProvider::new(16).unwrap();
    assert_eq!(p.embed(&[]).unwrap(), Vec::<Vec<f32>>::new());
}

// ---------------------------------------------------------------------------
// mock OpenAI-compatible server (std-only, loopback)
// ---------------------------------------------------------------------------

/// Serves `respond` for each of `n_requests` connections, then exits.
/// Returns the base URL and a hit counter.
fn mock_server(
    n_requests: usize,
    respond: impl Fn(usize, &str) -> (u16, Vec<(&'static str, String)>, String) + Send + 'static,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = Arc::clone(&hits);
    thread::spawn(move || {
        for i in 0..n_requests {
            let (mut stream, _) = listener.accept().unwrap();
            // Read headers.
            let mut buf = Vec::new();
            let mut one = [0u8; 1];
            while !buf.ends_with(b"\r\n\r\n") {
                if stream.read(&mut one).unwrap() == 0 {
                    break;
                }
                buf.push(one[0]);
                if buf.len() > 1_000_000 {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let len = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("Content-Length:")
                        .or_else(|| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).unwrap_or(());
            let (status, headers, resp_body) = respond(i, &String::from_utf8_lossy(&body));
            let reason = match status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                _ => "Error",
            };
            let mut resp = format!("HTTP/1.1 {status} {reason}\r\n");
            for (k, v) in headers {
                resp.push_str(&format!("{k}: {v}\r\n"));
            }
            resp.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                resp_body.len()
            ));
            stream.write_all(resp.as_bytes()).unwrap();
            stream.write_all(resp_body.as_bytes()).unwrap();
            hits2.fetch_add(1, Ordering::SeqCst);
        }
    });
    (url, hits)
}

/// Builds an OpenAI-style embeddings response with `dim`-wide vectors whose
/// first component encodes the input index (order check).
fn embeddings_response(n: usize, dim: usize) -> String {
    let data: Vec<String> = (0..n)
        .map(|i| {
            let mut v = vec![0.0f32; dim];
            v[0] = i as f32 * 0.25 + 0.5;
            format!(r#"{{"embedding":[{}],"index":{i}}}"#, v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","))
        })
        .collect();
    format!(r#"{{"data":[{}],"model":"mock"}}"#, data.join(","))
}

/// Counts the inputs in an embeddings request body.
fn count_inputs(body: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("input").and_then(|i| i.as_array()).map(|a| a.len()))
        .unwrap_or(0)
}

fn test_tag() -> ModelTag {
    ModelTag::new("mock-model", "1.0").unwrap()
}

// ---------------------------------------------------------------------------
// remote provider
// ---------------------------------------------------------------------------

#[test]
fn remote_batches_and_preserves_order() {
    // 7 texts with batch size 3 -> exactly 3 HTTP calls (3+3+1).
    let (url, hits) = mock_server(3, |_, body| {
        let n = count_inputs(body);
        (200, vec![], embeddings_response(n, 8))
    });
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8)
        .unwrap()
        .with_batch_size(3);
    let texts: Vec<String> = (0..7).map(|i| format!("text {i}")).collect();
    let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let out = p.embed(&refs).unwrap();
    assert_eq!(out.len(), 7);
    assert_eq!(hits.load(Ordering::SeqCst), 3, "must batch, not one-call-per-text");
    for (i, v) in out.iter().enumerate() {
        assert_eq!(v.len(), 8);
        assert!(
            (v[0] - (i_in_batch(i) as f32 * 0.25 + 0.5)).abs() < 1e-6,
            "output order must match input order (input {i})"
        );
    }
}

/// Input i lands at batch position i % 3 in the mock's per-request indexing.
fn i_in_batch(i: usize) -> usize {
    i % 3
}

#[test]
fn remote_empty_input_makes_no_calls() {
    let (url, hits) = mock_server(1, |_, _| (200, vec![], embeddings_response(0, 8)));
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8).unwrap();
    assert_eq!(p.embed(&[]).unwrap(), Vec::<Vec<f32>>::new());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn remote_429_is_transient_with_retry_after() {
    let (url, _) = mock_server(1, |_, _| {
        (
            429,
            vec![("Retry-After", "7".to_string())],
            r#"{"error":"slow down"}"#.to_string(),
        )
    });
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8).unwrap();
    let err = p.embed(&["x"]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Transient);
    assert_eq!(err.retry_after, Some(Duration::from_secs(7)));
}

#[test]
fn remote_401_is_permanent() {
    let (url, _) = mock_server(1, |_, _| (401, vec![], r#"{"error":"bad key"}"#.to_string()));
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8).unwrap();
    let err = p.embed(&["x"]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Permanent);
    assert!(err.retry_after.is_none());
}

#[test]
fn remote_500_is_transient() {
    let (url, _) = mock_server(1, |_, _| (500, vec![], "boom".to_string()));
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8).unwrap();
    assert_eq!(p.embed(&["x"]).unwrap_err().kind, ErrorKind::Transient);
}

#[test]
fn remote_timeout_is_transient() {
    let (url, _) = mock_server(1, |_, _| {
        thread::sleep(Duration::from_secs(5));
        (200, vec![], embeddings_response(1, 8))
    });
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8)
        .unwrap()
        .with_timeout(Duration::from_secs(1));
    assert_eq!(p.embed(&["x"]).unwrap_err().kind, ErrorKind::Transient);
}

#[test]
fn remote_dimension_mismatch_is_permanent() {
    let (url, _) = mock_server(1, |_, _| (200, vec![], embeddings_response(1, 4)));
    // Provider configured for dim 8 but the API returns dim 4.
    let p = RemoteEmbeddingProvider::new(url, "sekret", test_tag(), 8).unwrap();
    assert_eq!(p.embed(&["x"]).unwrap_err().kind, ErrorKind::Permanent);
}

#[test]
fn remote_validates_construction() {
    assert!(RemoteEmbeddingProvider::new("", "k", test_tag(), 8).is_err());
    assert!(RemoteEmbeddingProvider::new("http://x", "", test_tag(), 8).is_err());
    assert!(RemoteEmbeddingProvider::new("http://x", "k", test_tag(), 0).is_err());
}

#[test]
fn remote_from_env_reads_configuration() {
    let vars = [
        ("POLYVEC_EMBEDDING_BASE_URL", "https://api.example.com/v1"),
        ("POLYVEC_EMBEDDING_API_KEY", "sekret"),
        ("POLYVEC_EMBEDDING_MODEL", "e5-large"),
        ("POLYVEC_EMBEDDING_MODEL_VERSION", "2.0"),
        ("POLYVEC_EMBEDDING_DIMENSION", "1024"),
    ];
    let p = RemoteEmbeddingProvider::from_env_in(&vars).unwrap();
    assert_eq!(p.dimension(), 1024);
    assert_eq!(p.model_id().name, "e5-large");
    assert_eq!(p.model_id().version, "2.0");
    // Missing key -> error naming the variable.
    let missing: [(&str, &str); 0] = [];
    let err = RemoteEmbeddingProvider::from_env_in(&missing).unwrap_err();
    assert!(err.contains("POLYVEC_EMBEDDING_BASE_URL"), "got: {err}");
}

#[test]
fn embed_error_converts_to_core_provider_error() {
    let e = EmbedError::permanent("bad request");
    let core: polyvec_core::Error = e.into();
    assert!(core.to_string().contains("provider error"));
}
