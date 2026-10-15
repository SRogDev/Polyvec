# BENCHMARKS — Polyvec Phase 7

> Every number in this file was measured on 2026-10-15. Nothing is
> projected, estimated, or copied from another system. Methodology first,
> results after.

## Principles

1. **No invented numbers.** Each result names its hardware, dataset,
   commit SHA, and reproduction command.
2. **Reproducible**: fixed seeds, pinned dependency versions. The raw JSON
   report is committed at `crates/polyvec-bench/results/report-2026-10-15.json`.
3. **Honest scope**: the stub embedding provider is a hash function, so
   retrieval-quality numbers measure *pipeline mechanics*, not semantic
   search quality. Single-machine, single-workload; no cross-hardware
   comparisons. Cost ($/M reads) is not reported: without a reference
   deployment and billing model it would be fiction.

## How to reproduce

```sh
# Release profile — all published numbers use release builds.
cargo build --release -p polyvec-bench --bin bench_report \
    -p polyvec-server --bin polyvec-server
./target/release/bench_report > crates/polyvec-bench/results/report-<date>.json

# Criterion micro-benchmarks (search latency distributions, pipeline stages)
cargo bench -p polyvec-bench
```

`bench_report` prints one JSON document with `meta` (engine SHA, hardware,
profile, seeds), `layer_a` (vector engine), `layer_b` (knowledge pipeline),
and `layer_c` (end-to-end over HTTP). Layer C spawns the real
`polyvec-server` binary found next to `bench_report`; without it, layer C
reports `skipped`, never a fake number. `BENCH_LAYERS=a,b,c` selects a
subset while debugging.

## Environment (all runs below)

| Field | Value |
| ----- | ----- |
| Date | 2026-10-15 |
| Engine commit | `1e80552` (Phase 7 bench-harness fix; engine code identical since `6d9340a`) |
| Profile | release (`cargo build --release`; Criterion uses the bench profile) |
| CPU | AMD EPYC 9D25 126-Core (2 vCPUs visible in this VM) |
| RAM | 7.8 GiB |
| OS | Linux |
| Rust | 1.98.1; criterion 0.8.2, ureq 3.4.2, tokio 1.53.3, actix-web 4.17.0 |

## Layer A — vector engine

Synthetic unit-norm vectors, fixed seeds
(`crates/polyvec-bench/src/fixtures.rs`: xorshift64, seed `0xA1`/`0xA2`/…).
HNSW params: the collection default (`test_config`).

| Benchmark | Config | Result |
| --------- | ------ | ------ |
| Bulk insert | 10k vectors, dim 64 | **875 vec/s** (11.43 s) |
| Bulk insert | 50k vectors, dim 128 | **275 vec/s** (181.5 s) |
| Flush (checkpoint) | 10k vectors, dim 64 | 0.055 s |
| Recall@10 vs exact ground truth | n=2000, dim 64, 50 queries, ef=200 | **mean 1.0000, min 1.0000** |
| Search latency, ef=50 | n=10k, dim 64, k=10 (Criterion) | **1.70 ms** mean |
| Search latency, ef=200 | n=10k, dim 64, k=10 (Criterion) | **6.94 ms** mean |
| Search QPS, 1 thread | 2000 queries, ef=200 | **422 qps** |
| Search QPS, 8 threads | 2000 queries, ef=200, shared `Mutex<CollectionStore>` | **430 qps** |
| RAM per vector | 20k vectors, dim 64, `VmRSS` delta (clean heap baseline) | **5255 bytes/vec** (5.3 MB → 103.9 MB) |
| Disk per vector | 20k vectors, dim 64 (collection dir) | **2725 bytes/vec** (54.5 MB total) |
| Update cost | 10k updates (tombstone + reinsert) | **4.21 ms avg** |
| Delete cost | 1k deletes | **88 µs avg**; flush after: 0.090 s |
| Explicit rebuild | 19k live vectors | 33.0 s |
| Recovery (dump deleted → reopen) | 20k vectors, dim 128 | 33.8 s; `RebuiltFromCatalog`, 20k live recovered |

Notes:

- Insert throughput is HNSW-bound and single-threaded per collection
  (the per-collection `Mutex`); 50k dim-128 takes ~3 min. This is the
  known ingest bottleneck, not a regression — bulk loads are rare,
  reads are the optimized path (the thesis).
- 8-thread QPS through the server's per-collection lock shape ≈ 1-thread
  QPS: searches hold the lock briefly, so contention is low at this
  query shape. Lock-free scaling was not measured (`CollectionStore`
  is not `Sync` by design).
- Update = tombstone old + insert new (4.2 ms); delete only tombstones
  (88 µs) — compaction happens on rebuild.

### Criterion micro-benchmarks (bench profile)

| Benchmark | Result (mean) |
| --------- | ------------- |
| `chunk_8k_chars` | 61.9 µs |
| `canonicalize_8k_chars` | 42.8 µs |
| `stub_embed_batch_32_dim64` | 30.5 µs per batch of 32 (~0.95 µs/vec) |
| `search_latency/n10k_dim64_k10/50` | 1.70 ms |
| `search_latency/n10k_dim64_k10/200` | 6.94 ms |

## Layer B — knowledge pipeline

| Benchmark | Config | Result |
| --------- | ------ | ------ |
| Conversion: markdown | 668-char fixture | 158 µs, success |
| Conversion: plain text | 351-char fixture | 4.2 µs, success |
| Conversion: HTML | 186-char fixture | 456 µs, success |
| Per-doc stage timing | 20 synthetic docs | **0.74 ms/doc** total (convert 0.101 ms, canonicalize 0.008 ms, chunk 0.014 ms, stub-embed 0.005 ms, store write 0.618 ms) |
| Ingest via real `JobRunner` | 1 doc | 52 ms, 1 attempt, `Done` |
| Idempotent re-ingest | same bytes | 2.3 ms, `skipped: true`, version unchanged |
| Update (changed text) | 1 doc | 52 ms, version 1 → 2 |
| Delete propagation | 1 doc | 51 ms; document gone, 0 live vectors, 0 search hits |
| Retry: 2× transient → success | scripted provider | 3 attempts, 3.07 s wall (1 s + 2 s backoff), `Done` |
| Retry: permanent failure | scripted provider, max_attempts=3 | 1 attempt, 52 ms, `Failed` — permanent errors are never retried |

The store write dominates per-doc cost (0.62 of 0.74 ms); conversion,
canonicalization, chunking, and stub embedding are all sub-millisecond.

## Layer C — end-to-end over HTTP

Real `polyvec-server` binary over HTTP, 30 synthetic docs, stub provider.
Queries are exact chunk texts → rank 1 expected **by construction**
(mechanics check, not a semantic-quality claim).

| Benchmark | Result |
| --------- | ------ |
| Time to first useful result (ingest → searchable → rank 1) | **0.104 s** |
| 30 self-queries: MRR | **1.0000** |
| 30 self-queries: recall@5 | **1.0000** |
| 30 self-queries: nDCG@5 | **1.0000** |
| Query latency p50 over HTTP | 0.78 ms |
| Evidence spot-check (chunk text + source + doc metadata) | 0 violations |
| Restart durability (SIGTERM → restart → searchable) | graceful exit ✓, corpus searchable ✓ |

## Qdrant comparison

Not performed. There is no container runtime in this environment and no
Qdrant binary; more importantly, a same-machine comparison would still be
uncontrolled (different default index parameters, shared noisy VM,
measurer-is-vendor bias). **No cross-system numbers are published.**
A fair comparison needs: isolated hardware, equivalent HNSW parameters
(M, ef_construct, ef_search), the same corpus and query set, and recall
measured against the same exact ground truth.

## Known limitations of these numbers

- Stub provider = hash function: identical text → identical vector.
  Layer C measures plumbing, not retrieval intelligence.
- Single machine (2 vCPUs), single workload shape (uniform synthetic
  vectors). Real corpora (skewed, clustered) will differ.
- Only release-profile numbers are published; debug builds are an order
  of magnitude slower and are never quoted.
- RAM-per-vector is a VmRSS delta over one insert+flush — approximate
  (allocator retention); treat as order-of-magnitude.
- `kill -9` recovery over HTTP is untested (storage-layer recovery is
  tested; graceful-shutdown recovery over HTTP is measured above).
