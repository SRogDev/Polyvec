---
title: Architecture & decisions
description: Crate layout, dependency boundaries, data flows, and the ADRs behind them.
---

Polyvec is a modular monolith: one repo, many crates, dependencies pointing
inward only. Full detail lives in `ARCHITECTURE.md` in the repo; this page
is the map.

## Workspace layout

```text
crates/
├── polyvec-core/        # domain vocabulary: types, validated IDs, config contracts, Error
├── polyvec-index/       # hnsw_rs ANN wrapper, Polyvec-owned tombstones, exact-search baseline
├── polyvec-storage/     # redb-backed CollectionStore: catalog, persistence layout, recovery
├── polyvec-server/      # Actix Web HTTP API: /v1 routes, schemas, config, graceful shutdown
├── polyvec-ingestion/   # canonical model, chunking, ingestion jobs, converter adapter
├── polyvec-embeddings/  # provider trait, stub + remote (OpenAI-compatible) adapters
├── polyvec-retrieval/   # tantivy lexical index, RRF fusion, bounded relationship traversal
└── polyvec-bench/       # fixtures, Criterion benches, bench_report harness
vendor/markit/           # vendored markit (pinned 4197c76a; HTTP fetch stubbed)
```

Dependency direction (arrows mean "depends on"):

```text
polyvec-server
    └── polyvec-retrieval
            └── polyvec-embeddings
            └── polyvec-ingestion
            └── polyvec-index
            └── polyvec-storage
                    └── polyvec-core
```

- **`polyvec-core` has no framework dependencies** — no Actix, no Tokio,
  no HTTP. It must stay compilable as a pure library.
- Each crate depends only on crates below it. If a lower crate needs
  something from above, the trait definition moves down, not the
  dependency up.
- Shared lints from the workspace root: `unsafe` forbidden, `missing_docs`
  warned, Clippy `all` + `pedantic`.

## Data flow (write path)

```text
insert_vector(v)
  │  1. validate (dimension, finiteness)          [polyvec-core]
  │  2. redb txn: vector record + generation++    [polyvec-storage catalog]
  └─ 3. hnsw insert (node id ← monotonic counter) [polyvec-index AnnIndex]
```

Commit protocol: redb commits first (source of truth, durable
immediately); the ANN dump is a checksummed, rebuildable cache. See
[Persistence](/operations/persistence/).

## Data flow (HTTP)

No handler runs storage code on an async worker — every
`CollectionStore` call goes through `web::block` on Tokio's blocking
pool, with the per-collection `Mutex` held only inside the blocking
closure, never across `.await`. Collections open lazily on first use;
`GET /v1/collections` lists from memory plus cheap catalog config reads
(redb forbids opening a database twice in one process).

Errors map from `polyvec-core::Error` to structured JSON
`{error:{code,message,detail}}`: Validation/Config→400, NotFound→404,
AlreadyExists→409, Unsupported→501, Corrupt/Storage/Internal→500 with
safe messages (details go to the server log only).

## Key design decisions (ADRs)

In `docs/decisions/`:

- **ADR-001** — Actix Web over Axum for the HTTP layer.
- **ADR-002** — Modular monolith (one repo, many crates) over
  microservices or a single flat crate.
- **ADR-003** — Dual MIT/Apache-2.0 licensing and the dependency policy it
  forces (MIT/Apache-2.0 preferred; explicitly-reviewed permissive licenses
  via documented `deny.toml` exceptions; copyleft banned).
- **ADR-004** — Storage commit protocol and consistency model (redb commits
  first as source of truth; ANN dump is a checksummed, rebuildable cache;
  honest recovery rules).

Crate audits (`docs/audit/`): hnsw_rs 0.3.5, redb 4.4.0, tantivy 0.26.2
adopted; markit adapted (vendored); zvec-rs reference-only (tested broken);
Qdrant mandated reference.
