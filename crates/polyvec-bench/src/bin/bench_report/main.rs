//! Phase 7 single-shot benchmark report.
//!
//! Runs every benchmark measurement once with fixed seeds and prints a JSON
//! report to stdout. Save it next to the code:
//!
//! ```sh
//! cargo build -p polyvec-server --bin polyvec-server   # layer C needs this
//! cargo run --release -p polyvec-bench --bin bench_report \
//!     > crates/polyvec-bench/results/report-<date>.json
//! ```
//!
//! Layer C (HTTP end-to-end) spawns the server binary found next to this
//! binary (`target/debug/polyvec-server` / `target/release/polyvec-server`);
//! without it, layer C is reported as *skipped*, not failed.
//!
//! What the numbers mean (and do not mean) is documented in `BENCHMARKS.md`.
//! In particular: the stub provider is a hash function, so layer C measures
//! pipeline mechanics, not semantic retrieval quality.

mod common;
mod layer_b;
mod layer_c;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use polyvec_bench::fixtures::Rng;
use polyvec_bench::measure::{dir_size_bytes, peak_rss_kib, recall_at_k};
use polyvec_core::{DistanceMetric, Vector, VectorId};
use polyvec_index::ExactIndex;
use polyvec_storage::CollectionStore;
use serde_json::{Map, Value, json};

use common::{git_sha, hardware, scratch, test_config, typed_vectors};

// ---------------------------------------------------------------------------
// Layer A — vector engine
// ---------------------------------------------------------------------------

/// Bulk insert throughput: generate `n` vectors (outside the timer), then
/// time one atomic `insert_vectors` batch.
fn bench_bulk_insert(n: usize, dim: usize, seed: u64) -> Value {
    let dir = scratch(&format!("bulk-{n}-{dim}"));
    let dim_u32 = u32::try_from(dim).expect("benchmark dimensions fit in u32");
    let mut store = CollectionStore::create(&dir, test_config(dim_u32)).unwrap();
    let vectors = typed_vectors(n, dim, seed);
    let t0 = Instant::now();
    store.insert_vectors(vectors).unwrap();
    let dt = t0.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let vecs_per_sec = n as f64 / dt.as_secs_f64();
    json!({
        "n": n, "dim": dim, "seed": seed,
        "seconds": dt.as_secs_f64(),
        "vectors_per_sec": vecs_per_sec,
    })
}

#[allow(clippy::too_many_lines)] // one measurement per block; split would obscure the layer
fn layer_a() -> Value {
    let mut out = Map::new();

    // A1: bulk insert throughput (two sizes).
    out.insert(
        "bulk_insert_10k_dim64".to_string(),
        bench_bulk_insert(10_000, 64, 0xA1),
    );
    out.insert(
        "bulk_insert_50k_dim128".to_string(),
        bench_bulk_insert(50_000, 128, 0xA2),
    );

    // A2: flush (checkpoint) time after a 10k insert.
    let dir = scratch("flush");
    let mut store = CollectionStore::create(&dir, test_config(64)).unwrap();
    store
        .insert_vectors(typed_vectors(10_000, 64, 0xA3))
        .unwrap();
    let t0 = Instant::now();
    store.flush().unwrap();
    out.insert(
        "flush_10k_sec".to_string(),
        json!(t0.elapsed().as_secs_f64()),
    );

    // A3: recall@10 vs exact ground truth (N=2000, dim=64, ef=200).
    let (n, dim) = (2000usize, 64u32);
    let dir = scratch("recall");
    let mut store = CollectionStore::create(&dir, test_config(dim)).unwrap();
    let data =
        polyvec_bench::fixtures::unit_vectors(n, usize::try_from(dim).expect("dim fits"), 0xA4);
    let mut exact = ExactIndex::new(DistanceMetric::Cosine);
    for (i, v) in data.iter().enumerate() {
        exact.insert(i, v.clone());
        store
            .insert_vector(
                Vector::new(
                    VectorId::parse(&format!("v-{i:06}")).unwrap(),
                    v.clone(),
                    dim,
                )
                .unwrap(),
            )
            .unwrap();
    }
    let mut rng = Rng::new(0xA5);
    let dim_usize = usize::try_from(dim).expect("dim fits");
    let queries: Vec<Vec<f32>> = (0..50)
        .map(|_| (0..dim_usize).map(|_| rng.next_f32() * 2.0 - 1.0).collect())
        .collect();
    let mut recalls = Vec::new();
    for q in &queries {
        let approx: Vec<usize> = store
            .search(q, 10, 200)
            .unwrap()
            .iter()
            .map(|h| h.vector_id.as_str()[2..].parse::<usize>().unwrap())
            .collect();
        let truth: Vec<usize> = exact.search(q, 10).iter().map(|h| h.node_id).collect();
        recalls.push(recall_at_k(&truth, &approx, 10));
    }
    #[allow(clippy::cast_precision_loss)]
    let mean_recall = recalls.iter().sum::<f64>() / recalls.len() as f64;
    out.insert(
        "recall_at_10".to_string(),
        json!({
            "n": n, "dim": dim, "k": 10, "ef": 200, "queries": 50,
            "seed": "0xA4/0xA5", "mean": mean_recall,
            "min": recalls.iter().copied().fold(f64::INFINITY, f64::min),
        }),
    );

    // A4: search QPS, 1 thread and 8 threads (server-style Arc<Mutex>).
    let dir = scratch("qps");
    let mut store = CollectionStore::create(&dir, test_config(64)).unwrap();
    store
        .insert_vectors(typed_vectors(10_000, 64, 0xA6))
        .unwrap();
    let queries: Vec<Vec<f32>> = {
        let mut rng = Rng::new(0xA7);
        (0..2000)
            .map(|_| (0..64).map(|_| rng.next_f32() * 2.0 - 1.0).collect())
            .collect()
    };
    let t0 = Instant::now();
    for q in &queries {
        let _ = store.search(q, 10, 200).unwrap();
    }
    #[allow(clippy::cast_precision_loss)]
    let qps_1t = queries.len() as f64 / t0.elapsed().as_secs_f64();
    let store = Arc::new(Mutex::new(store));
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let t0 = Instant::now();
    let mut handles = Vec::new();
    for t in 0..8 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        let chunk: Vec<Vec<f32>> = queries[t * 250..(t + 1) * 250].to_vec();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for q in &chunk {
                let s = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = s.search(q, 10, 200).unwrap();
            }
        }));
    }
    barrier.wait();
    for h in handles {
        h.join().unwrap();
    }
    #[allow(clippy::cast_precision_loss)]
    let qps_8t = queries.len() as f64 / t0.elapsed().as_secs_f64();
    out.insert(
        "search_qps".to_string(),
        json!({
            "n": 10_000, "dim": 64, "k": 10, "ef": 200,
            "queries": 2000, "qps_1_thread": qps_1t, "qps_8_threads": qps_8t,
        }),
    );

    // A5: RAM per vector — peak-RSS delta across insert+flush of 20k vectors.
    // (Raw vector bytes are caller-owned before insert; the delta is
    // index + catalog overhead.)
    let dir = scratch("ram");
    let vectors = typed_vectors(20_000, 64, 0xA8);
    let rss_before = peak_rss_kib();
    let mut store = CollectionStore::create(&dir, test_config(64)).unwrap();
    store.insert_vectors(vectors).unwrap();
    store.flush().unwrap();
    let rss_after = peak_rss_kib();
    let ram_per_vec = match (rss_before, rss_after) {
        (Some(b), Some(a)) => {
            #[allow(clippy::cast_precision_loss)]
            let per = (a.saturating_sub(b)) as f64 * 1024.0 / 20_000.0;
            json!({"bytes_per_vector": per, "rss_before_kib": b, "rss_after_kib": a})
        }
        _ => json!({"bytes_per_vector": Value::Null, "note": "VmHWM unavailable (non-Linux?)"}),
    };
    out.insert("ram_per_vector".to_string(), ram_per_vec);

    // A6: disk per vector after flush.
    let bytes = dir_size_bytes(&dir);
    #[allow(clippy::cast_precision_loss)]
    let disk_per_vec = bytes as f64 / 20_000.0;
    out.insert(
        "disk_per_vector".to_string(),
        json!({"bytes_per_vector": disk_per_vec, "total_bytes": bytes, "n": 20_000}),
    );

    // A7: update + delete cost (1000 ops each), then rebuild time.
    let t0 = Instant::now();
    for i in 0..1000usize {
        let v = Vector::new(
            VectorId::parse(&format!("v-{i:06}")).unwrap(),
            polyvec_bench::fixtures::unit_vectors(1, 64, 0xA9 + i as u64)
                .pop()
                .unwrap(),
            64,
        )
        .unwrap();
        store.update_vector(v).unwrap();
    }
    #[allow(clippy::cast_precision_loss)]
    let update_us = t0.elapsed().as_micros() as f64 / 1000.0;
    let t0 = Instant::now();
    for i in 0..1000usize {
        store
            .delete_vector(&VectorId::parse(&format!("v-{i:06}")).unwrap())
            .unwrap();
    }
    #[allow(clippy::cast_precision_loss)]
    let delete_us = t0.elapsed().as_micros() as f64 / 1000.0;
    let t0 = Instant::now();
    store.flush().unwrap();
    let flush_after_delete = t0.elapsed().as_secs_f64();
    let t0 = Instant::now();
    store.rebuild().unwrap();
    let rebuild_sec = t0.elapsed().as_secs_f64();
    out.insert(
        "update_delete_rebuild".to_string(),
        json!({
            "update_avg_us": update_us, "delete_avg_us": delete_us,
            "flush_after_1k_deletes_sec": flush_after_delete,
            "explicit_rebuild_19k_live_sec": rebuild_sec,
        }),
    );

    // A8: recovery — delete the ANN dump + sidecar, reopen, time it.
    // The catalog is the source of truth; open must rebuild, not fail.
    let coll_dir = dir.clone();
    drop(store);
    for f in ["index.hnsw.graph", "index.hnsw.data", "sidecar.json"] {
        let _ = std::fs::remove_file(coll_dir.join(f));
    }
    let t0 = Instant::now();
    let (store, recovery) = CollectionStore::open(&coll_dir).unwrap();
    let recovery_sec = t0.elapsed().as_secs_f64();
    out.insert(
        "recovery_rebuild_on_open".to_string(),
        json!({
            "seconds": recovery_sec,
            "recovery": format!("{recovery:?}"),
            "live_vectors_after": store.stats().live_vectors,
        }),
    );

    Value::Object(out)
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async_main());
}

async fn async_main() {
    let t_start = Instant::now();
    let mut report = Map::new();
    report.insert(
        "meta".to_string(),
        json!({
            "date": civil_date(),
            "git_sha": git_sha(),
            "hardware": hardware(),
        }),
    );
    report.insert("layer_a_vector_engine".to_string(), layer_a());
    report.insert(
        "layer_b_knowledge_pipeline".to_string(),
        layer_b::layer_b().await,
    );
    report.insert("layer_c_end_to_end".to_string(), layer_c::layer_c());
    report.insert(
        "report_wall_seconds".to_string(),
        json!(t_start.elapsed().as_secs_f64()),
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(report)).unwrap()
    );
    // Let stdout flush before exit.
    std::thread::sleep(Duration::from_millis(100));
}

/// `YYYY-MM-DD` without a date dependency (Howard Hinnant's algorithm).
fn civil_date() -> String {
    let days = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86400);
    #[allow(clippy::cast_possible_wrap)]
    let days = days as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}
