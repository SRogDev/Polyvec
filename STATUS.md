# STATUS — Polyvec

> Single source of truth for where this project stands. Last updated: 2026-10-15.
> Read this before starting work. Update it in the same PR when reality changes.

## In progress / blocked
- Phase 7 (next): reliability, quality and performance (plan §15).

## Done
- 2026-10-15 — **Phase 6 done** (branch `phase-6-relations`, unmerged):
  integrated relationship retrieval. Typed, directed, explicitly-supplied
  relations (fixed vocabulary: `references`, `related_to`, `supersedes`;
  stable ids + provenance metadata + server-stamped `created_at`) stored
  in redb (`relations` + `rel_by_source`/`rel_by_target` composite-index
  tables; `Catalog::open` creates missing tables idempotently, so
  pre-Phase-6 catalogs keep opening with no format bump). Referential
  integrity at creation (404 unknown endpoint); `delete_document`
  cascades to relations of the doc and its chunks in both directions.
  `POST/GET/DELETE /v1/collections/{id}/relations[/{rel_id}]`; any query
  mode accepts `expand: {depth, max_nodes, relation_types?}` — base
  retrieval then bounded BFS (depth ≤ 5 default 3, nodes ≤ 1000 default
  100, fan-out 64, 2000 ms backstop), reached nodes appended with
  `score_kind: "relationship"` and `evidence: {hops, path}`. Pure
  `traverse()` in `polyvec-retrieval` (no storage dep; fallible adjacency
  so storage errors fail the query). Evidence: 223 workspace tests pass
  (incl. 8 storage-relation + 1 migration + 12 traversal + 8 server
  relation/expand tests), clippy `-D warnings` clean, `cargo fmt --check`
  clean, `cargo deny check` green (no new dependencies — zero license
  impact), live `docs/e2e-relations.sh` 24/24. Multi-hop fixture:
  lexical alone returns A; lexical + expand returns A, B, C with exact
  evidence paths (honest scope: structural retrieval value demonstrated,
  no semantic-quality claim). Known limitations: chunk-level relations
  are version-pinned across re-ingests; no extraction, no graph query
  language.
- 2026-10-15 — **Phase 5 done**: lexical and hybrid retrieval. New crate
  `polyvec-retrieval` (tantivy 0.26.2 `default-features=false` lexical
  index per collection; Reciprocal Rank Fusion, k=60 default). Query API:
  `mode` = `vector` (default, Phase 4 behavior) | `lexical` (BM25, no
  provider needed) | `hybrid` (RRF); common result schema
  (rank/id/score/score_kind/path + chunk text, source, metadata,
  provenance; scores never comparable across modes). Ingestion jobs index
  chunk text in the same job as vectors; updates re-index, deletes scrub
  (updated/deleted content absent from lexical results, tested);
  lexical index rebuilds from the catalog on every collection open.
  Exact-match metadata filters behave consistently across modes.
  `cargo deny check` green (foldhash/Zlib exception now documents the
  tantivy→lru provenance; no new copyleft). Evidence: workspace tests
  green incl. 8 new HTTP tests (hybrid fixture: lexical-wins +
  vector-wins + RRF; filter consistency; stale-content absence; reopen
  rebuild); `docs/retrieval.md` new; `docs/e2e-retrieval.sh` live script.
- 2026-10-15 — **Phase 4 done**: canonical model and ingestion pipeline.
  New crates `polyvec-embeddings` (sync `EmbeddingProvider` trait with
  transient/permanent classification; deterministic stub for dev/test;
  remote OpenAI-compatible HTTP adapter with batching, timeout, and
  `Retry-After` handling) and `polyvec-ingestion` (markit-backed
  `Converter` trait; deterministic canonicalization; section-aware
  chunker; `JobRunner` with persisted jobs, bounded retries, backpressure,
  idempotency). Vendored `markit` (`rust/` @ `4197c76a`) as
  `vendor/markit` with HTTP fetching stubbed and mimalloc removed.
  Storage: atomic `ingest_document` + one-time model-tag stamping.
  Server: `POST /ingest` → 202, `GET /jobs/{id}`, `GET /{id}/jobs`,
  `POST /query` (501 when no provider); `Error::Provider` → 502.
  Evidence: 150 tests pass workspace-wide, clippy `-D warnings` clean,
  `cargo fmt --check` clean, `cargo deny check` green (new license
  exceptions documented; MPL-2.0 flagged for review). Live e2e via HTTP
  (ingest → query → skip → update → delete) green.
- 2026-10-15 — **Phase 3 done**: server and public API. New crate
  `polyvec-server` (Actix Web 4.17.0 on Tokio 1.53.3, per ADR-001):
  versioned `/v1` routes — collections CRUD + `/stats` + `/flush`,
  vectors insert (single object or atomic batch) / top-k search
  `{vector,k,ef?}` / get / update / delete (idempotent), documents
  PUT-by-source (server-derived ids) / GET / DELETE (cascade) —
  plus `GET /healthz` + `GET /readyz`. Structured JSON errors
  `{error:{code,message,detail}}` mapped from `polyvec-core::Error`
  (Validation/Config→400, NotFound→404, AlreadyExists→409,
  Unsupported→501, Corrupt/Storage/Internal→500 with safe messages).
  CPU isolation: all storage work in `web::block` (per-collection
  `Mutex` never held across `.await`); lazy collection open; env config
  (`POLYVEC_BIND` default `127.0.0.1:8080`, non-loopback refused without
  `POLYVEC_ALLOW_NONLOCAL_BIND=1`); SIGTERM/SIGINT graceful shutdown
  with per-collection flush. No auth in Phase 3 (documented).
  Boundary-breaker audit (inspect-and-reason) recorded in `SECURITY.md`
  with CONFIRMED/PLAUSIBLE/UNTESTED tiers. Supporting changes:
  `CollectionStore::read_collection_config` (cheap listing; redb forbids
  double-open), `put_document` counter fix on overwrite. Evidence:
  88 tests pass workspace-wide (46 server: 44 API + 2 error-mapping),
  clippy `-D warnings` clean, `cargo fmt --check` clean,
  `cargo deny check` green (documented BSD-3-Clause/Unicode-3.0
  exceptions for the actix-web tree; RUSTSEC-2026-0258 h2 ignore —
  unreachable without TLS — documented), `docs/e2e.sh` 31/31 checks
  green against a live binary incl. SIGTERM→restart durability proof.
  `Cargo.lock` now committed (first binary ships).
- 2026-10-15 — **Phase 2 done**: core domain + persistent vector database.
  New crates `polyvec-index` (hnsw_rs ANN wrapper, Polyvec-owned tombstones,
  exact-search baseline) and `polyvec-storage` (`CollectionStore`: redb
  catalog, checksummed persistence layout, crash recovery). `polyvec-core`
  grew the domain: validated IDs (Collection/Document/Chunk/Vector),
  DistanceMetric (L2/cosine), CollectionConfig/HnswParams/ModelTag,
  Document/Chunk/Metadata, ContentHash, IndexState. Rust API: create
  collection, insert/search/update/delete vectors, documents/chunks with
  cascade delete. Commit protocol + consistency model in
  `docs/decisions/ADR-004-storage-commit-protocol.md` (redb commits first;
  no fake cross-file atomicity). Evidence: 40 tests pass
  (14 core + 11 index + 11 storage + 4 pre-existing); measured recall@10 =
  1.0000 vs exact baseline (N=2000, dim=64, ef=200, cosine); restart,
  interrupted-write, corrupt-dump, and tombstone-rebuild recovery tests
  green. `cargo deny check` green (added documented Zlib exception for
  foldhash via hnsw_rs→hashbrown). tantivy `default-features=false`:
  compiles; zstd/BSD/stemmer flags gone, foldhash (Zlib) remains via
  tantivy→lru (exception needed at Phase 5 integration) — recorded in
  `docs/audit/license-audit.md`.
- 2026-10-15 — **Phase 1 done**: crate audit complete (`docs/audit/`).
  **Adopted:** hnsw_rs 0.3.5 (ANN core), redb 4.4.0 (catalog/metadata),
  tantivy 0.26.2 (lexical). **Adapt:** markit (vendored, converter adapter).
  **Reference only:** zvec-rs (TESTED ANN incorrectness for cosine/IP +
  reopen-insert panic — rejected), Qdrant (mandated reference).
  Prototype evidence in `docs/audit/spike-phase1/` (recall@10 0.97 hnsw_rs vs
  0.016 zvec-rs-cosine, measured). License audit: no copyleft in 340-package
  tree; coordinator decision — explicitly-reviewed permissive licenses
  (Zlib/ISC/BSD/Unlicense) allowed via documented `deny.toml` exceptions,
  copyleft banned (reversible by Roger).
- 2026-10-15 — **Phase 0 done** (PR #3, squash-merged `85542c4`): Cargo
  workspace + `polyvec-core` (`Error` type, 4 tests) + licenses +
  ARCHITECTURE/BENCHMARKS/SaaS + 3 ADRs + `deny.toml`/`audit.toml`.
  `.github/workflows/ci.yml` written but kept local-only (token lacks
  `workflows` scope).
- 2026-09-26 — Research phase concluded; definitive 23-section implementation
  plan (Phases 0–9) approved and execution authorized.
- 2026-09-26 — Research started on Rust pieces/algorithms for the vector DB.
- 2026-09-26 — Repo created (SRogDev/Polyvec).

## Next
- Phase 7 — reliability, quality and performance (plan §15/§16): fill
  test gaps (concurrency, crash-during-write), auth/rate-limit decision,
  benchmark harness + honest BENCHMARKS.md. See
  `~/workspace/polyvec-build/phase7-plan.md`.
