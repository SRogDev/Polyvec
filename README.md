# Polyvec

A company knowledge-base vector database, built from scratch in Rust — optimized for **massive, cheap, high-recall reads by agents**.

## Thesis

Agents should be able to query a company's entire knowledge base constantly — thousands of reads per day per agent — at negligible cost. Existing vector DBs optimize for the general case; Polyvec optimizes for one: **massive, cheap, high-recall reads over company knowledge**, as the knowledge layer of Polygrow and future businesses.

## What it is not (for now)

- A general-purpose Pinecone/Qdrant replacement.
- The memory layer (episodic/conversational memory stays separate).

## Status

**Implementation plan: Phases 0–9** (definitive plan, 23 sections — research
phase is done, execution is running):

| Phase | Scope | Status |
| ----- | ----- | ------ |
| 0 | Repository & architecture foundation | done (PR #3) |
| 1 | Crate audit + persistence/index prototype | done |
| 2 | Core domain + persistent vector database | done |
| 3 | Server and public API (Actix Web) | done (PR #4) |
| 4 | Canonical model + ingestion pipeline | done |
| 5 | Lexical and hybrid retrieval | planned |
| 6 | Integrated relationship retrieval | planned |
| 7 | Reliability, quality and performance | planned |
| 8 | Documentation and AI-native DX | planned |
| 9 | MVP release | planned |

Progress is tracked in `STATUS.md`; each phase ships as its own PR.

## Layout

```text
crates/polyvec-core/      # domain types, IDs, config contracts, shared Error
crates/polyvec-index/     # hnsw_rs ANN wrapper, Polyvec-owned tombstones, exact-search baseline
crates/polyvec-storage/   # redb-backed CollectionStore: catalog, persistence layout, recovery
crates/polyvec-server/    # Actix Web HTTP API: /v1 routes, schemas, config, graceful shutdown
crates/polyvec-ingestion/ # ingestion pipeline: converter, canonical model, chunking, jobs
crates/polyvec-embeddings/ # embedding provider trait, stub + remote HTTP adapters
vendor/markit/            # vendored markit (pinned 4197c76a; HTTP stubbed, mimalloc removed)
docs/decisions/           # architecture decision records (ADRs)
docs/audit/               # crate & license audit reports
docs/API.md               # HTTP API reference with curl examples
docs/ingestion.md         # ingestion pipeline + cost-guard mechanisms
```

**Phase 2 in one paragraph:** `CollectionStore::create/open` manages one
collection directory (`catalog.redb` + `index.hnsw.{graph,data}` +
`sidecar.json`). Vectors are validated (dimension, finiteness), inserted
into the HNSW index (cosine/L2), and searchable via a Rust API; updates are
*tombstone + reinsert*, deletes hide immediately via the tombstone set and
are compacted by rebuilds. The commit protocol is documented honestly in
`docs/decisions/ADR-004-storage-commit-protocol.md`: redb commits first
(source of truth), the index dump is a checksummed, rebuildable cache —
restart recovery is asserted in tests (measured recall@10 = 1.0000 vs the
exact baseline at N=2000, dim=64).

`deny.toml` enforces the dependency policy (MIT/Apache-2.0 only — see
`CONTRIBUTING.md`); `cargo audit` runs in CI.

**Phase 3 in one paragraph:** `polyvec-server` exposes the engine over HTTP
(Actix Web, Tokio): versioned `/v1` paths for collections (CRUD, `/stats`,
`/flush`), vectors (single/batch insert, top-k search, get/update/delete),
and documents (upsert by source with server-derived ids, get, cascade
delete), plus `/healthz`/`/readyz`. Errors are structured JSON
(`{error:{code,message,detail}}`) mapped from `polyvec-core::Error`;
Corrupt/Storage/Internal details stay server-side. All storage work runs on
the blocking pool (`web::block`) so CPU-bound HNSW ops never stall request
workers. The server binds localhost by default and refuses non-loopback
binds without explicit opt-in — **there is no authentication in Phase 3**
(see `SECURITY.md`). Quickstart: `cargo run -p polyvec-server --bin polyvec-server`, then
`curl localhost:8080/healthz`; full reference in `docs/API.md`, runnable
smoke test in `docs/e2e.sh`.

**Phase 4 in one paragraph:** `POST /v1/collections/{id}/ingest` accepts
source bytes (`{text}` or `{base64}`) and returns `202 {job_id}`; the
`JobRunner` executes convert (vendored markit → Markdown) → canonicalize
(deterministic, original + canonical hashes) → change-detect (unchanged →
skip; a `vectors_present` guard finishes partial writes instead of
reporting them skipped) → chunk (section-aware, deterministic ids) →
embed (supplied vectors or the configured provider, batched) → atomic
store write (old version tombstoned, stale evidence unsearchable
immediately). Jobs persist in `jobs.redb` (`pending → running → done |
failed`, bounded retries with backoff honoring 429 `Retry-After`,
transient/permanent classification, bounded queue → `429` backpressure).
`POST /v1/collections/{id}/query {text, k}` embeds via the configured
provider (`none|stub|remote`) and searches — `501` when unconfigured.
Provider keys come from env only and never appear in logs or errors.
See `docs/ingestion.md`.

## License

Dual-licensed: MIT (`LICENSE-MIT`) OR Apache-2.0 (`LICENSE-APACHE`).
