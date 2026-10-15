//! Phase 1 spike: head-to-head ANN comparison — hnsw_rs vs zvec-rs.
//!
//! Tests (all measured, deterministic seed):
//!  1. insert 10k unit vectors (dim 128)
//!  2. recall@10 vs brute-force exact ground truth (100 queries)
//!  3. persistence: dump/close, reopen from disk, re-query (recall must hold)
//!  4. update + delete behavior
//!  5. (zvec-rs only) SQL-like filter correctness
//!
//! Run: cargo run -p ann_compare --release
//!
//! NOTE: hnsw_rs ef_search=200 (diag showed ef=64 under-recalls at N=10k).

use anyhow::Result;
use hnsw_rs::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;
use zvec_rs::{Collection, CollectionConfig, HnswParams, MetricType};

// ---------------------------------------------------------------------------
// deterministic RNG (xorshift64*) — no external deps, reproducible
// ---------------------------------------------------------------------------
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 11) as f32 / (1u64 << 53) as f32
    }
}

const N: usize = 10_000;
const DIM: usize = 128;
const NQ: usize = 100;
const K: usize = 10;
const HNSW_EF_SEARCH: usize = 200; // tuned: ef=64 -> 0.74 recall, ef=200 -> ~1.0 (diag)

fn gen_unit_vectors(rng: &mut Rng, n: usize, dim: usize) -> Vec<Vec<f32>> {
    (0..n)
        .map(|_| {
            let mut v: Vec<f32> = (0..dim).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in &mut v {
                *x /= norm;
            }
            v
        })
        .collect()
}

/// exact top-k by max dot product (vectors are unit length => cosine)
fn brute_topk(data: &[Vec<f32>], q: &[f32], k: usize) -> Vec<usize> {
    let mut scores: Vec<(usize, f32)> = data
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.iter().zip(q.iter()).map(|(a, b)| a * b).sum::<f32>()))
        .collect();
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scores.truncate(k);
    scores.into_iter().map(|(i, _)| i).collect()
}

fn recall_at_k(retrieved: &[usize], exact: &[usize]) -> f32 {
    let set: HashSet<usize> = exact.iter().cloned().collect();
    retrieved.iter().filter(|i| set.contains(i)).count() as f32 / exact.len() as f32
}

fn zvec_config() -> CollectionConfig {
    CollectionConfig::new(DIM)
        .with_metric(MetricType::Cosine)
        .with_hnsw_params(HnswParams::new(16, 200))
}

fn main() -> Result<()> {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    println!("generating {N} unit vectors (dim {DIM}, fixed seed)...");
    let data = gen_unit_vectors(&mut rng, N, DIM);
    let queries = gen_unit_vectors(&mut rng, NQ, DIM);

    println!("computing exact ground truth ({NQ} queries x {N} vectors)...");
    let t = Instant::now();
    let truth: Vec<Vec<usize>> = queries.iter().map(|q| brute_topk(&data, q, K)).collect();
    println!("  brute force ground truth: {:?}", t.elapsed());

    let base = PathBuf::from("/tmp/polyvec-spike");
    std::fs::create_dir_all(&base)?;

    // ============================ hnsw_rs ============================
    println!("\n===== hnsw_rs 0.3.5 =====");
    let hnsw = Hnsw::<f32, DistCosine>::new(16, N + 1000, 16, 200, DistCosine {});
    let t = Instant::now();
    let refs: Vec<(&Vec<f32>, usize)> = data.iter().enumerate().map(|(i, v)| (v, i)).collect();
    hnsw.parallel_insert(&refs);
    println!("[hnsw_rs] parallel insert {N}: {:?}", t.elapsed());

    let t = Instant::now();
    let mut rec = 0.0f32;
    for qi in 0..NQ {
        let res = hnsw.search(&queries[qi], K, HNSW_EF_SEARCH);
        let ids: Vec<usize> = res.iter().map(|n| n.get_origin_id()).collect();
        rec += recall_at_k(&ids, &truth[qi]);
    }
    let qtime = t.elapsed();
    println!(
        "[hnsw_rs] recall@{K} = {:.4}  ({} queries in {:?}, {:.2} ms/q)",
        rec / NQ as f32,
        NQ,
        qtime,
        qtime.as_secs_f64() * 1000.0 / NQ as f64
    );

    // persistence: dump -> drop -> reload -> re-query
    let hdir = base.join("hnsw");
    std::fs::create_dir_all(&hdir)?;
    let t = Instant::now();
    let dump_basename = AnnT::file_dump(&hnsw, &hdir, "spike")?;
    println!(
        "[hnsw_rs] file_dump -> {:?} (basename {dump_basename:?}): {:?}",
        std::fs::read_dir(&hdir)?
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>(),
        t.elapsed()
    );
    drop(hnsw);
    let t = Instant::now();
    let mut io = HnswIo::new(&hdir, &dump_basename);
    let hnsw2: Hnsw<f32, DistCosine> = io.load_hnsw()?;
    println!("[hnsw_rs] reload from disk: {:?}", t.elapsed());
    let mut rec2 = 0.0f32;
    for qi in 0..NQ {
        let res = hnsw2.search(&queries[qi], K, HNSW_EF_SEARCH);
        let ids: Vec<usize> = res.iter().map(|n| n.get_origin_id()).collect();
        rec2 += recall_at_k(&ids, &truth[qi]);
    }
    println!(
        "[hnsw_rs] recall@{K} AFTER RESTART = {:.4}",
        rec2 / NQ as f32
    );
    assert!((rec - rec2).abs() < 1e-6, "recall changed across restart!");
    println!("[hnsw_rs] restart consistency: OK (identical recall)");
    println!(
        "[hnsw_rs] delete API: NONE in public API (source-checked) -> tombstones+rebuild needed"
    );

    // ============================ zvec-rs ============================
    println!("\n===== zvec-rs (thedonmon/zvec-rs master) =====");
    let zdir = base.join("zvec");
    std::fs::create_dir_all(&zdir)?;
    let col = Collection::open(&zdir, "spike", zvec_config()).map_err(|e| anyhow::anyhow!(e))?;
    let t = Instant::now();
    for (i, v) in data.iter().enumerate() {
        col.upsert(&format!("doc-{i}"), v, HashMap::new());
    }
    println!("[zvec-rs] upsert {N} (one-by-one): {:?}", t.elapsed());
    println!("[zvec-rs] doc_count = {}", col.doc_count());

    let t = Instant::now();
    let mut zrec = 0.0f32;
    for qi in 0..NQ {
        let hits = col
            .search(&queries[qi], K, None)
            .map_err(|e| anyhow::anyhow!(e))?;
        let ids: Vec<usize> = hits
            .iter()
            .map(|h| h.pk["doc-".len()..].parse::<usize>().unwrap())
            .collect();
        zrec += recall_at_k(&ids, &truth[qi]);
    }
    let zqtime = t.elapsed();
    println!(
        "[zvec-rs] recall@{K} = {:.4}  ({} queries in {:?}, {:.2} ms/q)",
        zrec / NQ as f32,
        NQ,
        zqtime,
        zqtime.as_secs_f64() * 1000.0 / NQ as f64
    );

    // filter correctness: tag 200 docs, filter must return only tagged docs
    for i in 0..200 {
        let mut f = HashMap::new();
        f.insert("category".to_string(), "science".to_string());
        col.upsert(&format!("doc-{i}"), &data[i], f);
    }
    let fhits = col
        .search(&queries[0], K, Some("category = 'science'"))
        .map_err(|e| anyhow::anyhow!(e))?;
    let filter_ok = fhits.iter().all(|h| {
        let i: usize = h.pk["doc-".len()..].parse().unwrap();
        i < 200
    });
    println!(
        "[zvec-rs] filter \"category = 'science'\" returned {} hits, all in tagged set: {}",
        fhits.len(),
        if filter_ok { "OK" } else { "FAIL" }
    );

    // persistence: flush -> drop -> reopen -> re-query
    let t = Instant::now();
    let flushed = col.flush().map_err(|e| anyhow::anyhow!(e))?;
    println!("[zvec-rs] flush() -> {flushed}: {:?}", t.elapsed());
    drop(col);
    let t = Instant::now();
    let col2 = Collection::open(&zdir, "spike", zvec_config()).map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "[zvec-rs] reopen from disk: {:?}, doc_count = {}",
        t.elapsed(),
        col2.doc_count()
    );
    assert_eq!(col2.doc_count(), N, "doc count changed across restart!");
    let mut zrec2 = 0.0f32;
    for qi in 0..NQ {
        let hits = col2
            .search(&queries[qi], K, None)
            .map_err(|e| anyhow::anyhow!(e))?;
        let ids: Vec<usize> = hits
            .iter()
            .map(|h| h.pk["doc-".len()..].parse::<usize>().unwrap())
            .collect();
        zrec2 += recall_at_k(&ids, &truth[qi]);
    }
    println!(
        "[zvec-rs] recall@{K} AFTER RESTART = {:.4}",
        zrec2 / NQ as f32
    );

    // update: replace doc-0's vector with a fresh random vector; it must become rank-1 for that vector
    let mut rng2 = Rng(0xDEAD_BEEF);
    let new_v = gen_unit_vectors(&mut rng2, 1, DIM).pop().unwrap();
    col2.upsert("doc-0", &new_v, HashMap::new());
    let hits = col2
        .search(&new_v, 3, None)
        .map_err(|e| anyhow::anyhow!(e))?;
    let upd_ok = hits.first().map(|h| h.pk == "doc-0").unwrap_or(false);
    println!(
        "[zvec-rs] update doc-0 -> search(new vector) rank-1 is doc-0: {}",
        if upd_ok { "OK" } else { "FAIL" }
    );

    // delete: remove doc-1; it must not appear when querying its old vector
    let removed = col2.remove("doc-1");
    let hits = col2
        .search(&data[1], K, None)
        .map_err(|e| anyhow::anyhow!(e))?;
    let del_ok = removed && hits.iter().all(|h| h.pk != "doc-1");
    println!(
        "[zvec-rs] remove(\"doc-1\") -> {removed}, absent from top-{K} of its old vector: {}",
        if del_ok { "OK" } else { "FAIL" }
    );

    println!("\n===== DONE =====");
    Ok(())
}
