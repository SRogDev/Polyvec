# STATUS — Polyvec

> Single source of truth for where this project stands. Last updated: 2026-10-15.
> Read this before starting work. Update it in the same PR when reality changes.

## In progress / blocked
- Phase 9 — MVP release preparation (final phase of the definitive plan):
  **in progress on branch `phase-9-release`** (no push, no PR, no tag —
  the coordinator verifies the release checklist and cuts the tag).
  Verifying the 14-item release definition of done (plan §22) with live
  evidence, making the README release-ready, writing release notes
  (`docs/RELEASE_NOTES_v0.1.0.md`) + `docs/RELEASING.md`. Known gaps
  recorded in the notes: `.github/workflows/ci.yml` still local-only
  (token lacks `workflows` scope), Vercel project creation manual,
  `cargo audit` unbuildable in sandbox.

### Release checklist state (Phase 9, evidence 2026-10-15 on `phase-9-release`)

| # | Item | Verdict |
|---|------|---------|
| 1 | Self-hosted Rust vector DB, own persistence/indexing path | PASS — own redb+hnsw_rs path; `cargo tree` grep for qdrant/pinecone/chroma/lancedb/milvus/weaviate/pgvector: no hits |
| 2 | Documented server API (docs/API.md + site match code) | PASS — all 14 `/v1` routes in code present in docs/API.md + exercised by e2e scripts |
| 3 | Ingestion into canonical, provenance-aware model | PASS — e2e-ingestion.sh 8/8 (HTML→canonical→chunk→provenance) |
| 4 | Pluggable embeddings + precomputed vectors | PASS — provider trait + stub/remote adapters + supplied-vectors path (tests green) |
| 5 | Vector, lexical, hybrid, bounded relationship retrieval | PASS — e2e-retrieval.sh 25/25, e2e-relations.sh 24/24 |
| 6 | Update/delete propagation (no stale evidence) | PASS — covered by #5 scripts + `vectors_present` guard (update→v2, delete→0 hits) |
| 7 | Restart/recovery tested | PASS — storage recovery tests green + SIGTERM→restart in e2e.sh 31/31 |
| 8 | Performance baselines reproducible | PASS — BENCHMARKS.md present, harness committed, coordinator re-ran layer A (same ballpark) |
| 9 | Deps audited | PASS — `cargo deny check` green; license-audit.md current incl. v0.1.0 addendum; MPL-2.0 flag still noted |
| 10 | Diagnostics + actionable errors | PASS — e2e.sh checks 400/404/409 + `{code,message,detail}` body shape |
| 11 | Docs site builds, Vercel-ready | PASS — 21 pages, pagefind index, exit 0; Vercel import manual (pending Roger) |
| 12 | AI integration prompt e2e | PASS — Phase 8 record 16/16; re-smoked on this branch (healthz→201→ingest done→hybrid rrf hit) |
| 13 | SaaS.md future-only, nothing contradicts | PASS — SaaS.md present (plan §20 list); codebase grep: no multi-tenancy/billing/clustering claims |
| 14 | Limitations + future work documented | PASS — README known limitations + docs/RELEASE_NOTES_v0.1.0.md |

## Done
- 2026-10-15 — **Phase 8 merged** (PR #12, squash-merged `334d654`):
  documentation website + AI-native DX. Astro Starlight site in `site/`
  (20 pages, pinned astro 7.4.0 / @astrojs/starlight 0.42.6,
  package-lock.json committed; `npm ci && npm run build` green, 21 pages
  incl. 404, pagefind search). Versioned AI integration prompt
  `integrate-polyvec/v1.md` (+ site mirror) with live e2e test record:
  throwaway stdlib-Python client followed the prompt against a real local
  `polyvec-server` — 16/16 checks passed (collection create, ingest job
  lifecycle, lexical + vector queries, evidence fields, negative query 0
  hits, typed 404); v1 needed no fixes. Docs consistency sweep: corrected
  stale drift (docs/API.md auth note, STATUS phase table, README phase
  table, ARCHITECTURE layout + BENCHMARKS/SECURITY descriptions,
  docs/PROJECT_BRIEF stack/questions); verified all 19 `POLYVEC_*` env
  vars in code are documented in docs/API.md + site configuration page.
  Honesty ledger honored: stub = mechanics-only, no invented metrics,
  MPL-2.0 flag preserved. Pending Roger: Vercel project creation
  (manual; site is deploy-ready).
- 2026-10-15 — **Phase 7 done** (PR #11, squash-merged):
  reliability, quality and performance. No new product features except
  the two Phase 3-deferred security items, now **implemented**:
  bearer-token auth (`POLYVEC_API_KEY`, constant-time compare, key
  redacted from logs) + per-IP token-bucket rate limiting
  (`POLYVEC_RATE_LIMIT_RPS`, default 1000, 429 + `Retry-After`).
  Reliability: ~21 new tests (concurrent read/write soak with exact
  live-count assertion, concurrent search during insert, fuzz
  canonicalizer/chunker, duplicate/idempotent jobs under concurrency,
  200-deep metadata rejection, URL-source-never-fetched). Benchmark
  harness `crates/polyvec-bench` (Criterion + `bench_report`, layers
  A/B/C) with committed fixtures and raw JSON. Measured (release,
  2026-10-15): 875 vec/s bulk insert (10k dim64), recall@10 = 1.0000,
  1.70 ms search (ef 50), 422 qps, 5255 B/vec RAM, 2725 B/vec disk,
  0.74 ms/doc pipeline, 0.104 s time-to-first-result, MRR = 1.0
  (by construction). Evidence: 244 workspace tests pass, clippy
  `-D warnings` clean, `cargo fmt --check` clean, `cargo deny check`
  green. `cargo audit` does not build in this sandbox (linker env
  quirk, same as Phase 0); advisory coverage via `cargo deny`.
  Qdrant comparison not performed (no container runtime; documented
  as infeasible, no uncontrolled numbers). Known limitations in
  README.md and BENCHMARKS.md.
- 2026-10-15 — **Phase 6 done** (PR #10, squash-merged `f76b810`):
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
- Phase 9 — MVP release (final phase of the definitive plan): tagging,
  release artifacts, dependency lock for release.
