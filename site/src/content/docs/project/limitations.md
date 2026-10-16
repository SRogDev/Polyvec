---
title: Limitations & roadmap
description: What Polyvec does not do yet, stated plainly, and what comes next.
---

This page exists so nobody discovers these by surprise. Nothing here is
planned to be fixed silently — when a limitation is lifted, it is
announced with evidence.

## Current limitations (measured or by design)

- **Pre-release, no tagged versions.** No binaries, no container image,
  no client SDKs — HTTP is the only interface.
- **The stub embedding provider is a hash function.** Retrieval-quality
  numbers measure pipeline mechanics, not semantic search quality.
- **Benchmarks are single-machine, single-workload, release profile.**
  No cross-hardware or cross-system comparisons are published — the Qdrant
  comparison was explicitly not performed (see [Performance](/operations/performance/)).
- **Rate limiting is in-memory and per-process** (resets on restart;
  behind a proxy it sees the proxy's IP).
- **Recovery after `kill -9` over HTTP is untested**; restart recovery is
  covered at the storage layer and graceful-shutdown recovery over HTTP.
- **Vectors are stored as JSON in redb** — not space-efficient.
- **Insert throughput is HNSW-bound and single-threaded per collection**
  (the per-collection `Mutex`); 50k dim-128 vectors take ~3 min. Reads are
  the optimized path (the thesis).
- **Vector-side filters are post-retrieval** — fewer than `k` hits may
  return when a filter excludes candidates; no filter pushdown into HNSW.
- **No reranking.** No automatic relation extraction; no graph query
  language (bounded BFS only).
- **Chunk-level relations are version-pinned** across re-ingests.
- **The lexical index rebuilds from the catalog on every collection open**
  — one full reindex per collection per server start.
- **`cargo audit` does not build in this sandbox** (linker env quirk);
  advisory coverage comes from `cargo deny check`.
- **MPL-2.0 license exceptions** exist for the vendored markit's
  cssparser/selectors stack (weak file-scoped copyleft, documented,
  reversible by dropping HTML conversion) — flagged for maintainer review.

## Roadmap

- **Phase 9 — MVP release**: tagging, release artifacts, dependency lock
  for release. This is the last phase of the definitive implementation plan.
- Beyond the plan (not committed, not scheduled): client SDKs, container
  images, hosted docs, incremental lexical-index open, quantization
  evaluation, filter pushdown, reranking.

If a limitation above blocks your use case, open an issue on GitHub —
concrete workloads beat speculation for prioritizing what comes next.
