//! Layer A micro-benchmarks: search latency distributions.
//!
//! Throughput/recall/memory numbers live in the `bench_report` binary
//! (single-shot measurements); criterion owns the latency distributions.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use polyvec_bench::fixtures::{Rng, unit_vectors};
use polyvec_core::{CollectionConfig, CollectionId, DistanceMetric, Vector, VectorId};
use polyvec_storage::CollectionStore;
use std::sync::atomic::{AtomicU64, Ordering};

static BENCH_SEQ: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let n = BENCH_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "polyvec-bench-{name}-{pid}-{n}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_config(dim: u32) -> CollectionConfig {
    CollectionConfig::new(
        CollectionId::new("bench").unwrap(),
        dim,
        DistanceMetric::Cosine,
    )
    .unwrap()
}

/// A collection pre-loaded with `n` vectors of `dim` (seed fixed).
fn loaded_store(n: usize, dim: u32, seed: u64) -> (CollectionStore, Vec<f32>) {
    let dir = scratch_dir(&format!("search-{n}-{dim}"));
    let mut store = CollectionStore::create(&dir, test_config(dim)).unwrap();
    let mut rng = Rng::new(seed);
    let dim_usize = usize::try_from(dim).expect("bench dims fit in usize");
    let data = unit_vectors(n, dim_usize, seed ^ 0xABCD);
    let mut vectors = Vec::with_capacity(n);
    for (i, v) in data.into_iter().enumerate() {
        let id = VectorId::parse(&format!("v-{i:06}")).unwrap();
        vectors.push(Vector::new(id, v, dim).unwrap());
    }
    store.insert_vectors(vectors).unwrap();
    // One query vector (deterministic, not in the corpus).
    let q: Vec<f32> = (0..dim_usize).map(|_| rng.next_f32() * 2.0 - 1.0).collect();
    (store, q)
}

fn search_latency(c: &mut Criterion) {
    let (store, q) = loaded_store(10_000, 64, 0x5EED_0001);
    let mut group = c.benchmark_group("search_latency");
    for ef in [50usize, 200usize] {
        group.bench_with_input(BenchmarkId::new("n10k_dim64_k10", ef), &ef, |b, &ef| {
            b.iter(|| store.search(&q, 10, ef).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, search_latency);
criterion_main!(benches);
