//! Phase 2 RED tests: ANN index layer.
//!
//! Defines the intended `AnnIndex` / `TombstoneSet` / `ExactIndex` API.
//! FAILS until implemented. Includes the critical insert-after-load test
//! (the reopen-insert path that disqualified zvec-rs).

use std::collections::HashSet;

use polyvec_core::{DistanceMetric, Error, HnswParams};
use polyvec_index::{AnnIndex, ExactIndex, TombstoneSet, recall_at_k};

// ---------------------------------------------------------------------------
// deterministic RNG (xorshift64*) — reproducible, no extra deps
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
    #[allow(clippy::cast_precision_loss)] // test RNG: float conversion is the recipe
    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 11) as f32 / (1u64 << 53) as f32
    }
}

fn unit_vectors(rng: &mut Rng, n: usize, dim: usize) -> Vec<Vec<f32>> {
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

fn params() -> HnswParams {
    HnswParams {
        m: 16,
        ef_construction: 200,
        ef_search: 200,
        max_elements: 100_000,
    }
}

#[test]
fn insert_search_roundtrip_returns_self_at_rank_1() {
    let dim = 32u32;
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let mut rng = Rng(0x1234);
    let data = unit_vectors(&mut rng, 100, dim as usize);
    for (i, v) in data.iter().enumerate() {
        index.insert(i, v).unwrap();
    }
    assert_eq!(index.len(), 100);

    let tombstones = TombstoneSet::new();
    // Querying with a vector's own bytes must return it at rank 1, distance ~0.
    for (i, v) in data.iter().enumerate().take(10) {
        let hits = index.search(v, 5, 64, &tombstones).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].node_id, i, "self must be rank 1");
        assert!(
            hits[0].distance < 1e-4,
            "self distance ~0, got {}",
            hits[0].distance
        );
    }
}

#[test]
fn l2_metric_roundtrip() {
    let dim = 16u32;
    let index = AnnIndex::new(DistanceMetric::L2, &params(), dim);
    let mut rng = Rng(0x99);
    let data = unit_vectors(&mut rng, 50, dim as usize);
    for (i, v) in data.iter().enumerate() {
        index.insert(i, v).unwrap();
    }
    assert_eq!(index.metric(), DistanceMetric::L2);
    let tombstones = TombstoneSet::new();
    let hits = index.search(&data[7], 3, 32, &tombstones).unwrap();
    assert_eq!(hits[0].node_id, 7);
}

#[test]
fn recall_at_10_vs_exact_baseline() {
    const N: usize = 2000;
    const DIM: usize = 64;
    const NQ: usize = 50;
    const K: usize = 10;
    let mut rng = Rng(0xDEAD_BEEF);
    let data = unit_vectors(&mut rng, N, DIM);
    let queries = unit_vectors(&mut rng, NQ, DIM);

    let index = AnnIndex::new(
        DistanceMetric::Cosine,
        &params(),
        u32::try_from(DIM).unwrap(),
    );
    let refs: Vec<(&Vec<f32>, usize)> = data.iter().enumerate().map(|(i, v)| (v, i)).collect();
    index.insert_batch(&refs).unwrap();

    let mut exact = ExactIndex::new(DistanceMetric::Cosine);
    for (i, v) in data.iter().enumerate() {
        exact.insert(i, v.clone());
    }

    let tombstones = TombstoneSet::new();
    let mut recall_sum = 0.0f32;
    for q in &queries {
        let hits = index.search(q, K, 200, &tombstones).unwrap();
        let got: Vec<usize> = hits.iter().map(|h| h.node_id).collect();
        let want: Vec<usize> = exact.search(q, K).iter().map(|h| h.node_id).collect();
        recall_sum += recall_at_k(&got, &want);
    }
    #[allow(clippy::cast_precision_loss)]
    let nq = NQ as f32;
    let recall = recall_sum / nq;
    println!("MEASURED recall@10 (N=2000, dim=64, ef=200): {recall:.4}");
    assert!(
        recall >= 0.90,
        "recall@10 {recall:.4} below 0.90 threshold (ef_search may need tuning)"
    );
}

#[test]
fn tombstones_hide_vectors_from_search_immediately() {
    let dim = 32u32;
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let mut rng = Rng(0x55);
    let data = unit_vectors(&mut rng, 200, dim as usize);
    for (i, v) in data.iter().enumerate() {
        index.insert(i, v).unwrap();
    }

    let mut tombstones = TombstoneSet::new();
    // Find the top-5 for a query, tombstone all of them, verify they vanish
    // and the result set still fills up with live vectors.
    let query = &data[0];
    let before = index.search(query, 5, 64, &tombstones).unwrap();
    assert_eq!(before.len(), 5);
    for h in &before {
        tombstones.insert(h.node_id);
    }
    let after = index.search(query, 5, 64, &tombstones).unwrap();
    assert_eq!(after.len(), 5, "results must refill from live vectors");
    let dead: HashSet<usize> = before.iter().map(|h| h.node_id).collect();
    for h in &after {
        assert!(
            !dead.contains(&h.node_id),
            "tombstoned node leaked into results"
        );
    }
    assert_eq!(tombstones.len(), 5);
}

#[test]
fn dump_load_produces_identical_results() {
    let dim = 24u32;
    let dir = tempfile_dir("dump_load");
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let mut rng = Rng(0x77);
    let data = unit_vectors(&mut rng, 300, dim as usize);
    for (i, v) in data.iter().enumerate() {
        index.insert(i, v).unwrap();
    }
    index.dump(&dir, "vectors").unwrap();
    assert!(dir.join("vectors.hnsw.graph").exists());
    assert!(dir.join("vectors.hnsw.data").exists());

    let tombstones = TombstoneSet::new();
    let query = &data[42];
    let before = index.search(query, 10, 64, &tombstones).unwrap();
    drop(index);

    let loaded = AnnIndex::load(&dir, "vectors", DistanceMetric::Cosine, dim, 300).unwrap();
    assert_eq!(loaded.len(), 300);
    let after = loaded.search(query, 10, 64, &tombstones).unwrap();
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(after.iter()) {
        assert_eq!(a.node_id, b.node_id, "node ids must survive dump/load");
        assert!(
            (a.distance - b.distance).abs() < 1e-6,
            "distances must survive dump/load"
        );
    }
}

#[test]
fn insert_after_load_works() {
    // The reopen-insert path: zvec-rs panicked here; hnsw_rs must not.
    let dim = 16u32;
    let dir = tempfile_dir("insert_after_load");
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let mut rng = Rng(0xABCD);
    let data = unit_vectors(&mut rng, 100, dim as usize);
    for (i, v) in data.iter().enumerate() {
        index.insert(i, v).unwrap();
    }
    index.dump(&dir, "vectors").unwrap();
    drop(index);

    let loaded = AnnIndex::load(&dir, "vectors", DistanceMetric::Cosine, dim, 100).unwrap();
    let more = unit_vectors(&mut rng, 50, dim as usize);
    for (i, v) in more.iter().enumerate() {
        loaded.insert(100 + i, v).unwrap();
    }
    assert_eq!(loaded.len(), 150);

    let tombstones = TombstoneSet::new();
    let hits = loaded.search(&more[13], 5, 64, &tombstones).unwrap();
    assert_eq!(hits[0].node_id, 113, "post-load insert must be searchable");
}

#[test]
fn update_is_tombstone_old_plus_insert_new() {
    let dim = 8u32;
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let v_old = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let v_new = vec![0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    index.insert(0, &v_old).unwrap();

    let mut tombstones = TombstoneSet::new();
    // update: hide old node, insert new node for the same logical vector
    tombstones.insert(0);
    index.insert(1, &v_new).unwrap();

    let hits = index.search(&v_new, 5, 32, &tombstones).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].node_id, 1);
    assert!(
        hits.iter().all(|h| h.node_id != 0),
        "old node must stay hidden"
    );
}

#[test]
fn search_validates_inputs_and_handles_edges() {
    let dim = 8u32;
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    let tombstones = TombstoneSet::new();

    // empty index -> empty results, not a panic
    let q = vec![1.0f32; 8];
    assert!(index.search(&q, 5, 32, &tombstones).unwrap().is_empty());
    // k = 0 -> empty
    assert!(index.search(&q, 0, 32, &tombstones).unwrap().is_empty());
    // wrong query dim -> Validation error (hnsw_rs would panic on assert)
    assert!(matches!(
        index.search(&[1.0f32; 7], 5, 32, &tombstones),
        Err(Error::Validation { .. })
    ));
    // NaN query -> Validation error
    let mut nan_q = vec![1.0f32; 8];
    nan_q[0] = f32::NAN;
    assert!(matches!(
        index.search(&nan_q, 5, 32, &tombstones),
        Err(Error::Validation { .. })
    ));
    // ef < k is clamped, not an error
    index.insert(0, &q).unwrap();
    let hits = index.search(&q, 5, 2, &tombstones).unwrap();
    assert_eq!(hits.len(), 1);
    // insert dim mismatch -> Validation error
    assert!(matches!(
        index.insert(1, &[1.0f32; 9]),
        Err(Error::Validation { .. })
    ));
}

#[test]
fn load_rejects_metric_mismatch() {
    let dim = 8u32;
    let dir = tempfile_dir("metric_mismatch");
    let index = AnnIndex::new(DistanceMetric::Cosine, &params(), dim);
    index.insert(0, &[1.0f32; 8]).unwrap();
    index.dump(&dir, "vectors").unwrap();
    drop(index);
    // Dump is cosine; loading as L2 must fail, not silently reinterpret.
    assert!(AnnIndex::load(&dir, "vectors", DistanceMetric::L2, dim, 1).is_err());
}

#[test]
fn exact_index_matches_manual_brute_force() {
    let mut exact = ExactIndex::new(DistanceMetric::L2);
    exact.insert(0, vec![0.0f32, 0.0]);
    exact.insert(1, vec![3.0f32, 4.0]);
    exact.insert(2, vec![1.0f32, 1.0]);
    let hits = exact.search(&[0.0f32, 0.0], 2);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].node_id, 0); // distance 0
    assert_eq!(hits[1].node_id, 2); // sqrt(2) < 5
    assert!(exact.remove(0));
    assert_eq!(exact.len(), 2);
    assert!(!exact.remove(0));
}

#[test]
fn recall_at_k_helper_is_correct() {
    assert!((recall_at_k(&[1, 2, 3], &[1, 2, 3]) - 1.0).abs() < 1e-6);
    assert!((recall_at_k(&[1, 9, 9], &[1, 2, 3]) - 1.0 / 3.0).abs() < 1e-6);
    assert!((recall_at_k(&[], &[1, 2])).abs() < 1e-6);
}

fn tempfile_dir(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("polyvec-index-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
