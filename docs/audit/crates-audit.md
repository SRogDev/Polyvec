# Polyvec — Critical Crate Audit (Phase 1)

> Status: accepted 2026-10-15. Prototype evidence: `docs/audit/spike-phase1/` (sources committed; `vendor/markit-src` excluded — see spike README for reproduction).

Date: 2026-10-15. Auditor: Phase-1 builder (subagent), reviewed by coordinator.
Method: capability-scout procedure (decompose → sweep → rubric → DOCUMENTED vs TESTED).
Evidence levels: **DOCUMENTED** = official docs/source say so; **TESTED** = ran in this
environment and observed. Nothing below is claimed from a README alone.

## Capability requirements (from plan §5 + landscape doc)

| # | Requirement | Testable form |
|---|---|---|
| R1 | ANN index, incremental insert | insert 10k vectors without rebuild |
| R2 | ANN recall vs exact | recall@10 vs brute-force ground truth, measured |
| R3 | Persistence + restart | dump/close/reopen, recall identical after restart |
| R4 | Update + delete | upsert same key reflects; deleted key absent |
| R5 | Metadata filtering | filtered search returns only matching docs |
| R6 | Embedded transactional storage | ACID commit, uncommitted rollback, reopen durability |
| R7 | Lexical search | BM25 ranking, incremental add/delete, persistence |
| R8 | Document conversion to markdown | convert HTML/CSV/text without network |
| R9 | License policy | MIT/Apache-2.0 only, direct + transitive |

## Rubric (per candidate)

Fit / Compatibility / Maintenance / License / Limits / Security / Integration effort / Uncertainty.

---

## A. zvec-rs — `thedonmon/zvec-rs` (crate name `zvec-rs`, unpublished)

**Version checked:** git master @ 2026-03-10 (no tags, no releases). NOT on crates.io
under this name. crates.io `zvec` 0.1.0 (2026-04-24, Apache-2.0) is published from a
DIFFERENT repo (`oly-wan-kenobi/zvec-rs`, 1 star, pushed 2026-07-20) — treat as
unrelated/name-squat; not evaluated.

| Field | Finding |
|---|---|
| Fit | Collection API: open/upsert/search/remove/flush/delete-by-filter; HNSW+IVF+sparse; SQL-like filters; quantization (FP16/INT8/INT4/PQ); RRF fusion. Covers R1–R5 on paper — **but ANN search is BROKEN for similarity metrics (see TESTED evidence).** |
| Compatibility | Edition 2021, MSRV unknown. Deps: redb **v2** (stale vs current v4), rayon, parking_lot, serde. Optional reqwest/tokio features. Builds on rustc 1.98.1 — TESTED (spike compiles+runs). |
| Maintenance | **Weak.** 1 star, 1 fork, last push 2026-03-10 (~7 months stale), 8 open issues, no releases, no changelog. Bus factor 1. |
| License | Cargo.toml declares `MIT`; **no LICENSE file in repo** (GitHub license detection: none). Policy-compatible in declaration only — flag for legal hygiene. |
| Limits | Single-node embedded only (fine for v0.1). |
| Security | No CVEs found (too small to have any). `unsafe` usage: not audited (reference-only). |
| Integration effort | Low as git dep (`zvec-rs = { git = ... }`); API is high-level and pleasant. |
| Uncertainty | Crash recovery semantics of its redb-v2 layer untested; filter edge cases untested; no fuzzing/CI visible. |

**Spike evidence (TESTED):** `docs/audit/spike-phase1/crates/ann_compare` (+ `src/bin/zdiag.rs`) —
10k×128f32, 100 queries, recall@10 vs brute force:
- **Cosine: recall@10 = 0.016** (N=10k). At N=2000: 0.072.
- **IP: recall@10 = 0.070** (N=2000).
- **L2: recall@10 = 0.81** (N=2000) — the only working metric.
- **Exact-match broken for Cosine/IP:** querying with doc-42's own vector returns
  doc-1515 (Cosine) / doc-370 (IP) at rank 1 instead of doc-42. L2 returns doc-42. TESTED.
- Root cause (source-read): distance fns are correct (`distance/mod.rs`), so the bug is
  in the HNSW search/neighbor-selection path mishandling "higher is better" metrics.
- **Reopen-insert panic (TESTED, reproducible):** `Collection::open` on a previously
  flushed dir succeeds and `doc_count` is preserved, but the first subsequent `upsert`
  panics with `index out of bounds: the len is 1 but the index is 1` at
  `src/hnsw/graph.rs:273` (node level vs `connections[lc]` bookkeeping corrupted by
  reload). Fresh-dir runs complete; reopened-dir runs crash. Their redb-backed
  "graph topology export/import" does not round-trip write-continuation.
- What DOES work (TESTED): filter `category = 'science'` correct; flush→reopen
  preserves doc_count=10000; update reflects; remove hides the doc.

**Decision: REFERENCE ONLY (defer adoption).** Four independent hard-filter failures:
(1) TESTED ANN incorrectness for cosine/IP — the metric Polyvec needs;
(2) TESTED panic on reopen-then-insert (persistence doesn't round-trip writes);
(3) maintenance (1 star, 7 months stale, bus factor 1);
(4) no LICENSE file. The architecture (Collection over HNSW + redb + inverted
filter index) remains a good shape to learn from; its HNSW internals should be
treated with suspicion even as reference. Revisit only if published + maintained +
search bug fixed.

---

## B. Qdrant — REFERENCE ONLY (mandated)

Version spot-checked: qdrant/qdrant, Apache-2.0, actively maintained (DOCUMENTED).
Studied for: collection lifecycle, single-stage filtered HNSW traversal, payload
indexing, quantization (SQ/PQ/BQ) + rerank recipe, WAL/snapshot lifecycle, test
layout. **Must NOT become a runtime dependency** — no integration work done, none planned.

**Decision: REFERENCE ONLY.** (No rubric needed; mandated by plan.)

---

## C. hnsw_rs — `jean-pierreBoth/hnswlib-rs`, crate `hnsw_rs` 0.3.5

| Field | Finding |
|---|---|
| Fit | Pure-Rust HNSW. Incremental + parallel insert (R1 TESTED). `search`/`search_filter` with `FilterT` trait (R5 DOCUMENTED, filter trait exists — hook quality TESTED only via search_filter compile). **No delete/remove API in public surface** (source-grepped 0.3.5: no `remove`/`delete`/tombstone) → R4-delete must be layered (tombstones + periodic rebuild). Persistence via `HnswIo`: `file_dump` → `$base.hnsw.graph` + `$base.hnsw.data`, `load_hnsw()` with mmap option (R3 TESTED in spike). |
| Compatibility | Edition 2021; no `rust-version` declared. Deps: anndists, rayon-ish internals, serde (for dump). Compiles clean on 1.98.1 TESTED. `DistCosine`/`DistL2` from anndists. |
| Maintenance | **Strong.** 255 stars, pushed 2026-10-09 (6 days before audit), 10 open issues, 1.15M crates.io downloads, Changes.md maintained. Author responsive (single maintainer but active). |
| License | Dual **MIT/Apache-2.0** — LICENSE-MIT + LICENSE-APACHE in crate. Policy-clean. |
| Limits | No deletion; dump is whole-graph (no incremental snapshot); filter trait is per-query callback (post-filter style). |
| Security | Pure Rust; `unsafe` usage: not yet fully audited (grep pending in spike dir). |
| Integration effort | Medium: Polyvec must own id↔DocId mapping, tombstone set, snapshot scheduling, and filtered-search plumbing around it. |
| Uncertainty | Reload-time behavior at 1M+ scale untested; `load_hnsw` lifetime API is awkward (`&'a mut self` borrow). |

**Spike evidence (TESTED):** `docs/audit/spike-phase1/crates/ann_compare` (+ `src/bin/diag.rs`) —
10k×128f32 unit vectors, 100 queries, recall@10 vs brute-force exact ground truth:
- **recall@10 = 0.9700** (N=10k, M=16, ef_construction=200, ef_search=200, DistCosine).
  (ef_search=64 gave 0.744 — ef must scale with N; diag at N=2k: ef=64→0.944, ef=200→1.0000.)
- insert 10k (parallel): 31–45s; query 5.1 ms/q @ ef=200.
- `file_dump` → `spike.hnsw.{graph,data}` (247ms); `HnswIo::load_hnsw()` reload 214–386ms;
  **recall identical after restart** (asserted <1e-6 diff).
- insert 10k (parallel): 44.7s; query 3.8ms/q @ ef=64 (re-measured @ ef=200 in final run).
- `file_dump` → `spike.hnsw.{graph,data}` (246ms); `HnswIo::load_hnsw()` reload 214ms;
  **recall identical after restart** (asserted <1e-6 diff).
- **No delete/remove API** in public surface (source-grepped 0.3.5) — deletion must be
  layered (tombstones + periodic rebuild).
- `unsafe` count in crate src: **0** (pure safe Rust).

**Decision: ADOPT as the MVP ANN core (library, not fork).** Best fit for R1–R3, maintained,
license-clean, pure Rust, TESTED recall ~1.0 and restart-stable persistence. Deletion via tombstones + rebuild is an accepted, documented
trade-off (matches plan §2.3 "incrementally mutable" via rebuild cadence; tombstone
design lands in Phase 2/3).

---

## D. redb — `cberner/redb`, crate `redb` 4.4.0

| Field | Finding |
|---|---|
| Fit | Embedded transactional KV, ACID, MVCC read txns, zero-copy reads. Ideal for catalog/metadata/payload/job-state (R6). NOT for vectors at QPS (not its job). |
| Compatibility | MSRV 1.90 (declared). Single file DB. Compiles clean on 1.98.1 TESTED. API note: v4 moved `len()` to `ReadableTableMetadata` trait. |
| Maintenance | **Very strong.** 4849 stars, pushed 2026-10-14 (yesterday), 13.4M downloads, 3 open issues. |
| License | Dual **MIT/Apache-2.0** (LICENSE-MIT + LICENSE-APACHE). Policy-clean. |
| Limits | Single-writer transactions (one write txn at a time); fine for catalog, not for high-write vector ingest — vectors live in the ANN layer, not redb. |
| Security | No unsafe-heavy surface; well-fuzzed upstream (DOCUMENTED). |
| Integration effort | Low. |
| Uncertainty | None material for catalog use. |

**Spike evidence (TESTED):** 10k×512B blobs in one txn (141.8 ms), reopen 124 µs,
count+100 spot-checks identical after restart, update/delete visible, uncommitted
txn rolled back. All assertions passed.

**Decision: ADOPT for catalog/metadata/payload/job state.** What persists where:
- redb: collection catalog, schemas, doc metadata/payloads, filter field indexes (later), job/ingest state.
- hnsw_rs dump files: ANN graph + vectors (its own `$base.hnsw.{graph,data}` files, checksummed + versioned by Polyvec).
- tantivy: own index directory (lexical).
- Rationale: each engine owns its storage format (best tool per job); redb is the transactional source of truth for everything else.

---

## E. tantivy — `quickwit-oss/tantivy`, crate `tantivy` 0.26.2

| Field | Finding |
|---|---|
| Fit | BM25, tokenizers/stemmers, field indexing, term/posting deletes, incremental commit, directory persistence (R7). |
| Compatibility | MSRV 1.86. Default features pull mmap/lz4/zstd/stemmer. Compiles on 1.98.1 TESTED. |
| Maintenance | **Very strong.** 16224 stars, pushed 2026-10-15 (today), 20M downloads. |
| License | **MIT** (LICENSE file). Policy-clean. |
| Limits | IndexWriter heap setting; deletes are by term (no doc-id delete — must map id→term, TESTED pattern works). |
| Security | Mature; no new concerns. |
| Integration effort | Low-medium: schema design + query parser wiring. |
| Uncertainty | None material. |

**Spike evidence (TESTED):** 10k docs indexed+committed (248ms); BM25 "quokka" query
126µs, top-10 all contain the term with scores > 0; delete(7)+update(8) verified
absent (18ms); reopen from dir 451µs — **deletes survived restart**.
Learning: deletes are term-based — the id field must be `INDEXED` (a `STORED`-only
field silently skips `delete_term`; verified via isolated repro).
`unsafe` in crate src: 15 (mmap/postings decode — expected areas).

**Decision: ADOPT as the lexical/BM25 sidecar.** (Matches landscape-doc recommendation.)

---

## F. markit — `shift-labs-ai/markit` (npm `@shiftlabs/markit` 0.6.1; Rust core in `rust/`)

| Field | Finding |
|---|---|
| Fit | 20 converters (pdf/docx/pptx/xlsx/epub/html/csv/json/xml/yaml/ipynb/rss/zip/audio/image/iwork/wikipedia/github/plain-text). Clean Rust API: `Markit::new()`, `convert(&[u8], &StreamInfo) -> ConversionResult{markdown,title}`, `Converter` trait, injectable `HttpFetch` (R8). |
| Compatibility | **Not on crates.io; Cargo.toml lives in `rust/` subdir → cannot be a cargo git dependency.** Integration = vendor subdirectory (path dep) or wait for publish. Default features=`["cli"]` must be disabled (`default-features=false`); napi bindings are feature-gated (off by default in lib use). Compiles as rlib TESTED. Deps include mimalloc, ureq, zip, scraper — heavier tree, all policy-license (see license-audit). |
| Maintenance | Moderate: 1324 stars, pushed 2026-08-17 (~2 months), 16 open issues, active. (Note: crates.io `markit` 1.2.2 is an UNRELATED Nightstack project — do not confuse.) |
| License | **MIT** (Cargo.toml + LICENSE). Policy-clean. |
| Limits | Conversion quality per-format not evaluated here (Phase 4). |
| Security | Parses untrusted files (zip/pdf/html) — sandboxing/limits belong to Phase 4 design. |
| Integration effort | Medium: vendor `rust/` as subtree + adapter trait in Polyvec (so the converter is swappable). |
| Uncertainty | Upstream may restructure `rust/` dir; pin to a commit SHA when vendoring. |

**Spike evidence (TESTED):** `convert_markit` — `Markit::new()` constructs offline;
HTML→markdown (h1/link/list preserved), CSV→markdown table, plain-text passthrough.
All assertions passed.

**Decision: ADAPT (vendor behind an adapter interface).** The Rust API is real and usable
— not CLI-only. But: no crates.io publish + subdir layout forces vendoring; keep
conversion behind a `Converter` adapter trait (defined in Phase 4) so markit is
replaceable. Pin vendored commit SHA in the spike README.

---

## G. Foundation crates (tokio / actix-web / reqwest / serde / thiserror / tracing)

| Crate | Latest (checked 2026-10-15) | License | MSRV | Note |
|---|---|---|---|---|
| tokio | 1.53.3 | MIT | 1.71 | runtime |
| actix-web | 4.17.0 | MIT OR Apache-2.0 | 1.88 | HTTP API |
| reqwest | 0.13.5 | MIT OR Apache-2.0 | 1.85.0 | embedding-provider HTTP (feature-gated, blocking off) |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | 1.56 / 1.71 | |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | 1.77 | (markit itself uses anyhow — adapter will map) |
| tracing | 0.1.44 | MIT | 1.65.0 | |

All policy-clean (MIT/Apache-2.0). Full table with transitive notes in `license-audit.md`.

---

## Cost-guard lens (embedding provider surface)

Applies to Phase 3+ (embedding providers), recorded now per skill:
1. **Retry storms (present risk):** embedding APIs rate-limit (429). Unbounded retry × per-query embedding = cost × attempts. Mitigation (design now, build later): bounded retries + backoff + retry budget per batch; classify 4xx-permanent vs 429/5xx-transient; cache failure signatures. No code yet — recorded as requirement.
2. **Batching (absent → add):** one HTTP call per query vector multiplies request overhead. Mitigation: batch embed API calls (provider-dependent batch limits); local-model path (ort/candle) removes the API entirely for the default case.
3. **Context reloading (N/A yet):** no loop exists.
4. Local-first default (Qwen3-0.6B/BGE-M3 via ort per landscape doc) is the primary cost control: marginal embedding cost ≈ 0. API providers = fallback behind the same trait.
5. No exact prices claimed — provider pricing changes; the audit records the mechanisms, not numbers.

## Unsafe-code audit (TESTED — grepped registry sources)

- hnsw_rs 0.3.5: **0** `unsafe` in `src/` — pure safe Rust.
- redb 4.4.0: 54 `unsafe` — concentrated in `tree_store/page_store` (mmap/file backend),
  `sync.rs`, `xxh3.rs`. Expected for a zero-copy page store; upstream is well-fuzzed (DOCUMENTED).
- tantivy 0.26.2: 15 `unsafe` — `mmap_directory`, postings `block_search`, lz4 store decode.
  Expected areas.
- zvec-rs: reference-only; not audited.

## Decisions summary

| Candidate | Decision |
|---|---|
| zvec-rs | REFERENCE ONLY (defer; maintenance hard-filter fail) |
| Qdrant | REFERENCE ONLY (mandated) |
| hnsw_rs 0.3.5 | **ADOPT** (MVP ANN core) |
| redb 4.4.0 | **ADOPT** (catalog/metadata/payload/job state) |
| tantivy 0.26.2 | **ADOPT** (lexical/BM25 sidecar) |
| markit (vendored) | **ADAPT** (behind converter adapter trait, Phase 4) |
| tokio/actix-web/reqwest/serde/thiserror/tracing | ADOPT (versions pinned in license audit) |
