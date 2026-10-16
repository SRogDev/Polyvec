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
| 5 | Lexical and hybrid retrieval | done |
| 6 | Integrated relationship retrieval | done |
| 7 | Reliability, quality and performance | done |
| 8 | Documentation and AI-native DX | done (PR #12) |
| 9 | MVP release | in progress |

Progress is tracked in `STATUS.md`; each phase ships as its own PR.

## Install

```bash
git clone https://github.com/SRogDev/Polyvec.git && cd Polyvec
cargo build --release -p polyvec-server   # binary: target/release/polyvec-server
```

- Requires Rust **1.85+** (edition 2024; built and tested with **1.98.1**).
- Tested on **Linux x86_64**. Other OSes/architectures are untested —
  see "Known limitations".
- `Cargo.lock` is committed: the same dependency tree builds
  reproducibly (license policy enforced by `deny.toml`).

## Quickstart

```bash
# Start the server (defaults: 127.0.0.1:8080, ./polyvec-data)
./target/release/polyvec-server

curl localhost:8080/healthz   # {"status":"ok","service":"polyvec","version":"0.1.0"}

# Create a collection (201). metric: "l2" or "cosine"; dimension 1..4096.
curl -X POST localhost:8080/v1/collections \
  -H 'content-type: application/json' \
  -d '{"id":"kb","dimension":3,"metric":"cosine"}'

# Insert vectors (single or atomic batch {"vectors":[...]} — 201).
curl -X POST localhost:8080/v1/collections/kb/vectors \
  -H 'content-type: application/json' \
  -d '{"vectors":[{"id":"vec-1","values":[0.1,0.2,0.3]}]}'

# Search (k = 1..1000).
curl -X POST localhost:8080/v1/collections/kb/vectors/search \
  -H 'content-type: application/json' \
  -d '{"vector":[0.1,0.2,0.3],"k":10}'
# {"hits":[{"id":"vec-1","distance":0.0},...],"k":10,...}
```

Full reference: `docs/API.md`. Runnable live smoke test covering every
endpoint (success + error cases + SIGTERM→restart durability):
`./docs/e2e.sh`. Configuration reference (all 19 `POLYVEC_*` env vars):
`docs/API.md` and the site page *Configuration*.

## Layout

```text
crates/polyvec-core/      # domain types, IDs, config contracts, shared Error
crates/polyvec-index/     # hnsw_rs ANN wrapper, Polyvec-owned tombstones, exact-search baseline
crates/polyvec-storage/   # redb-backed CollectionStore: catalog, persistence layout, recovery
crates/polyvec-server/    # Actix Web HTTP API: /v1 routes, schemas, config, graceful shutdown
crates/polyvec-ingestion/ # ingestion pipeline: converter, canonical model, chunking, jobs
crates/polyvec-embeddings/ # embedding provider trait, stub + remote HTTP adapters
crates/polyvec-retrieval/  # lexical (tantivy) + hybrid (RRF) + relationship-aware retrieval
crates/polyvec-bench/     # benchmark harness: fixtures, Criterion benches, bench_report (layers A/B/C)
vendor/markit/            # vendored markit (pinned 4197c76a; HTTP stubbed, mimalloc removed)
docs/decisions/           # architecture decision records (ADRs)
docs/audit/               # crate & license audit reports
docs/API.md               # HTTP API reference with curl examples
docs/ingestion.md         # ingestion pipeline + cost-guard mechanisms
integrate-polyvec/        # versioned AI integration prompt (v1) + live test record
site/                     # Astro Starlight docs site (deploy-ready; Vercel import pending)
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
binds without explicit opt-in — **authentication is opt-in via
`POLYVEC_API_KEY`** (Phase 7; off by default, see `docs/API.md` and
`SECURITY.md`). Quickstart: `cargo run -p polyvec-server --bin polyvec-server`, then
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

**Phase 5 in one paragraph:** `polyvec-retrieval` adds the lexical
subsystem (tantivy 0.26.2, `default-features=false`, one index per
collection) and Reciprocal Rank Fusion (`k=60` default, configurable).
`POST /v1/collections/{id}/query` takes `mode`: `vector` (default — the
Phase 4 behavior), `lexical` (BM25 over chunk text, needs no embedding
provider), or `hybrid` (RRF over both rankings). Hits share one schema —
`rank/id/score/score_kind/path` plus chunk text, source, document
metadata, and provenance — with documented score semantics (scores are
never comparable across modes). Ingestion jobs index chunk text right
after the vector write in the same job; updates re-index and deletes
scrub the index, so stale content never surfaces lexically; the index
rebuilds from the catalog on every collection open. Exact-match metadata
filters work identically in all three modes. Full semantics in
`docs/retrieval.md`; live smoke test in `docs/e2e-retrieval.sh`.

**Phase 6 in one paragraph:** relations are explicitly supplied, typed,
directed edges between documents/chunks (`references`, `related_to`,
`supersedes` — the fixed initial vocabulary), stored in redb next to the
document catalog with provenance metadata and server-stamped `created_at`.
`POST/GET/DELETE /v1/collections/{id}/relations[/{rel_id}]` manages them
(referential integrity at creation; document delete cascades to relations
in both directions, so no orphaned relations are retrievable). Any query
mode accepts `expand`: base retrieval first, then bounded BFS
(depth ≤ 5, ≤ 1000 nodes, 64 fan-out, 2000 ms backstop) seeded from the
base hits, with reached nodes appended carrying `evidence`
(`{hops, path:[{relation_type, direction, relation_id}]}`). The committed
fixture proves the mechanism: three documents where only A mentions the
query term, `A → B → C` — lexical alone returns A, lexical + `expand`
returns A, B, C with exact evidence paths. Full semantics in
`docs/retrieval.md`; live smoke test in `docs/e2e-relations.sh`.

**Phase 7 in one paragraph:** reliability, quality, and performance —
no new product features except the two Phase 3-deferred security items.
New tests: concurrent read/write soaks, restart-during-writes recovery,
write-failure paths, duplicate/idempotent job submission under
concurrency, malformed vectors (NaN/Infinity, dimension mismatch),
deeply-nested metadata rejection, input-size limits, and a fuzz harness
over the canonicalizer/chunker (300 hostile rounds, fixed seed).
`POLYVEC_API_KEY` enables a bearer-token auth hook on `/v1` (constant-time
compare, key redacted from logs; `/healthz`/`/readyz` stay open) and
`POLYVEC_RATE_LIMIT_RPS` a per-IP token-bucket limiter (default 1000,
`429` + `Retry-After`). `crates/polyvec-bench` adds the benchmark harness:
Criterion benches, committed fixtures, and `bench_report` measuring three
layers — (A) vector engine (recall@10 vs exact ground truth, search
latency, QPS, RAM/disk per vector, bulk-insert throughput, rebuild and
recovery time), (B) knowledge pipeline (conversion by format, per-doc
stage timing, runner behavior under scripted provider failures), (C)
end-to-end over HTTP (self-query MRR/recall@5/nDCG@5, time-to-first-useful-result,
restart durability). Results in `BENCHMARKS.md` — measured, not claimed.

## Known limitations

- Only Linux x86_64 is tested; other platforms are unverified.
- The stub embedding provider is a hash function: retrieval quality
  numbers measure pipeline mechanics, not semantic search quality.
- Benchmarks are single-machine, single-workload, release profile;
  no cross-hardware or cross-system comparisons are published.
- Rate limiting is an in-memory, per-process token bucket (resets on
  restart; behind a proxy it sees the proxy's IP).
- Recovery after `kill -9` over HTTP is untested; restart recovery is
  covered at the storage layer and graceful-shutdown recovery over HTTP.
- `cargo audit` does not build in this sandbox (linker env quirk);
  advisory coverage comes from `cargo deny check` (`advisories`).
- CI (`.github/workflows/ci.yml`) is written but not yet committed —
  the stored GitHub token lacks the `workflows` scope; commit it with a
  full-scope token (pending Roger). The docs-site Vercel project is
  deploy-ready but must be created manually (Vercel connection degraded).
- No new features land after this point without a release-blocker
  justification; see `docs/RELEASE_NOTES_v0.1.0.md`.

## License

Dual-licensed: MIT (`LICENSE-MIT`) OR Apache-2.0 (`LICENSE-APACHE`).
