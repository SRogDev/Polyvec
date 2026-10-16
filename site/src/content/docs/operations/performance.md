---
title: Performance & benchmarks
description: Measured numbers with methodology — what was measured, how, and what it does not prove.
---

> Every number here was measured on 2026-10-15. Nothing is projected,
> estimated, or copied from another system. Methodology first, results
> after. The raw JSON report is committed at
> `crates/polyvec-bench/results/report-2026-10-15.json`.

## Principles

1. **No invented numbers.** Each result names its hardware, dataset,
   commit SHA, and reproduction command.
2. **Reproducible**: fixed seeds, pinned dependency versions.
3. **Honest scope**: the stub embedding provider is a hash function, so
   retrieval-quality numbers measure *pipeline mechanics*, not semantic
   search quality. Single-machine, single-workload; no cross-hardware
   comparisons. Cost ($/M reads) is not reported: without a reference
   deployment and billing model it would be fiction.

## Reproduce

```sh
# Release profile — all published numbers use release builds.
cargo build --release -p polyvec-bench --bin bench_report \
    -p polyvec-server --bin polyvec-server
./target/release/bench_report > crates/polyvec-bench/results/report-<date>.json

# Criterion micro-benchmarks
cargo bench -p polyvec-bench
```

`bench_report` prints one JSON document with `meta`, `layer_a` (vector
engine), `layer_b` (knowledge pipeline), `layer_c` (end-to-end over
HTTP). Layer C spawns the real `polyvec-server` binary found next to
`bench_report`; without it, layer C reports `skipped`, never a fake
number. `BENCH_LAYERS=a,b,c` selects a subset.

Environment: AMD EPYC 9D25 (2 vCPUs visible), 7.8 GiB RAM, Linux,
Rust 1.98.1, release profile, engine commit `1e80552`.

## Results

**Layer A — vector engine** (synthetic unit-norm vectors, fixed seeds;
HNSW collection defaults):

| Benchmark | Config | Result |
| --------- | ------ | ------ |
| Bulk insert | 10k vectors, dim 64 | **875 vec/s** |
| Bulk insert | 50k vectors, dim 128 | **275 vec/s** |
| Recall@10 vs exact ground truth | n=2000, dim 64, 50 queries, ef=200 | **mean 1.0000, min 1.0000** |
| Search latency, ef=50 | n=10k, dim 64, k=10 | **1.70 ms** mean |
| Search latency, ef=200 | n=10k, dim 64, k=10 | **6.94 ms** mean |
| Search QPS, 1 thread | 2000 queries, ef=200 | **422 qps** |
| RAM per vector | 20k vectors, dim 64 | **5255 bytes/vec** |
| Disk per vector | 20k vectors, dim 64 | **2725 bytes/vec** |
| Update cost | 10k updates (tombstone + reinsert) | **4.21 ms avg** |
| Delete cost | 1k deletes | **88 µs avg** |
| Explicit rebuild | 19k live vectors | 33.0 s |
| Recovery (dump deleted → reopen) | 20k vectors, dim 128 | 33.8 s; 20k live recovered |

Insert throughput is HNSW-bound and single-threaded per collection (the
per-collection `Mutex`); 50k dim-128 takes ~3 min. This is the known
ingest bottleneck, not a regression — bulk loads are rare, reads are the
optimized path (the thesis).

**Layer B — knowledge pipeline:**

| Benchmark | Result |
| --------- | ------ |
| Per-doc stage timing (20 synthetic docs) | **0.74 ms/doc** total (convert 0.101 ms, canonicalize 0.008 ms, chunk 0.014 ms, stub-embed 0.005 ms, store write 0.618 ms) |
| Ingest via real `JobRunner` | 52 ms, 1 attempt, `Done` |
| Idempotent re-ingest | 2.3 ms, `skipped: true`, version unchanged |
| Retry: 2× transient → success | 3 attempts, 3.07 s wall, `Done` |
| Retry: permanent failure | 1 attempt, `Failed` — permanent errors are never retried |

The store write dominates per-doc cost (0.62 of 0.74 ms).

**Layer C — end-to-end over HTTP** (real `polyvec-server`, 30 synthetic
docs, stub provider; queries are exact chunk texts → rank 1 expected **by
construction** — mechanics check, not a semantic-quality claim):

| Benchmark | Result |
| --------- | ------ |
| Time to first useful result (ingest → searchable → rank 1) | **0.104 s** |
| 30 self-queries: MRR / recall@5 / nDCG@5 | **1.0000** |
| Query latency p50 over HTTP | 0.78 ms |
| Restart durability (SIGTERM → restart → searchable) | graceful exit ✓, corpus searchable ✓ |

## Qdrant comparison

**Not performed.** No container runtime and no Qdrant binary in the
measurement environment; a same-machine comparison would still be
uncontrolled (different default index parameters, shared noisy VM,
measurer-is-vendor bias). **No cross-system numbers are published.** A fair
comparison needs: isolated hardware, equivalent HNSW parameters, the same
corpus and query set, and recall against the same exact ground truth.

## Known limitations of these numbers

- Stub provider = hash function: identical text → identical vector.
  Layer C measures plumbing, not retrieval intelligence.
- Single machine (2 vCPUs), single workload shape (uniform synthetic
  vectors). Real corpora (skewed, clustered) will differ.
- Only release-profile numbers are published; debug builds are an order of
  magnitude slower and are never quoted.
- RAM-per-vector is a VmRSS delta — approximate; treat as order-of-magnitude.
