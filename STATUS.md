# STATUS — Polyvec

> Single source of truth for where this project stands. Last updated: 2026-10-15.
> Read this before starting work. Update it in the same PR when reality changes.

## Done
- 2026-09-26 — Repo created (SRogDev/Polyvec).
- 2026-09-26 — Research started on Rust pieces/algorithms for the vector DB.
- 2026-10-15 — Research phase concluded; definitive 23-section implementation
  plan (Phases 0–9) approved and execution authorized.
- 2026-10-15 — **Phase 0 done** (PR #3, squash-merged `85542c4`): Cargo
  workspace + `polyvec-core` (`Error` type, 4 tests) + licenses +
  ARCHITECTURE/BENCHMARKS/SaaS + 3 ADRs + `deny.toml`/`audit.toml`.
  `.github/workflows/ci.yml` written but kept local-only (token lacks
  `workflows` scope).
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

## In progress / blocked
- (none — Phase 3 dispatch next)

## Done
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

## Next
- Phase 2 — core domain + persistent vector database: collections/documents/
  vectors model, redb-backed catalog, hnsw_rs ANN integration with
  Polyvec-owned tombstones, exact-search baseline, restart recovery.
