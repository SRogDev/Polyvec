# Polyvec v0.1.0 — MVP release notes (draft, coordinator gates the tag)

Status: draft. The `v0.1.0` tag is created by the coordinator only after
the Phase 9 acceptance gate passes (plan §22, all 14 items evidenced).
This file ships with the tag and becomes the GitHub release body.

## What the MVP is

A self-hosted, open-source (MIT/Apache-2.0) vector database for company
knowledge bases, optimized for massive cheap reads by AI agents. One
Rust binary (`polyvec-server`), versioned `/v1` HTTP API, own
persistence and indexing path — no delegation to Qdrant/Pinecone or any
external vector engine.

### What it does

- **Collections**: create/delete with dimension, metric (L2/cosine), and
  HNSW parameters; persistent on disk.
- **Vectors**: single + atomic-batch insert, top-k ANN search (HNSW via
  `hnsw_rs`), get/update/delete. Deletes hide immediately (tombstones);
  index dumps are checksummed caches, rebuildable from the redb catalog.
- **Ingestion pipeline**: HTML/Markdown/text → vendored markit converter
  → deterministic canonicalization → section-aware chunking →
  embedding provider (pluggable trait: stub / OpenAI-compatible remote /
  precomputed supplied vectors) → atomic store write. Jobs persist with
  bounded retries, backoff, idempotency, backpressure (429).
- **Retrieval**: vector (default), lexical (tantivy BM25, no provider
  needed), hybrid (RRF), and bounded relationship-aware expansion —
  explicitly supplied typed relations (`references`, `related_to`,
  `supersedes`) with bounded BFS and evidence (`hops`, `path`).
- **Propagation**: updates tombstone the old version, deletes cascade
  (documents → chunks → vectors → relations); stale content is
  unsearchable immediately after the write returns (tested).
- **Reliability**: crash-recovery tests at the storage layer, SIGTERM
  graceful shutdown with per-collection flush, bearer-token auth
  (opt-in, `POLYVEC_API_KEY`) + per-IP rate limiting (default 1000 RPS).
- **Docs**: Astro Starlight site (`site/`, deploy-ready), `docs/API.md`
  reference, versioned AI integration prompt (`integrate-polyvec/v1.md`)
  tested end to end against a live server (16/16 checks).
- **Evidence**: full test suite green, clippy `-D warnings` clean,
  `cargo fmt --check` clean, `cargo deny check` green
  (bans/licenses/advisories/sources). Benchmarks in `BENCHMARKS.md`
  are measured and re-runnable.

### What it does not do (non-goals for v0.1.0)

- No SaaS, no multi-tenancy, no billing, no hosted auth, no admin
  dashboard — see `SaaS.md` (future requirements notes only).
- No clustering, replication, or sharding; single process, single host.
- No client SDKs — HTTP is the only interface.
- No conversational/episodic memory layer (out of scope by thesis).
- No GUI; no observability platform beyond structured errors and
  `/healthz`/`/readyz`.
- Benchmarks are single-machine and stub-based; no Qdrant/comparison
  claims are made (`BENCHMARKS.md` says so plainly).

## How to verify this release

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # Rust 1.85+; built with 1.98.1
cargo build --workspace                # clean build
cargo test --workspace                 # full suite (all crates) green
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check                      # note: no --all (vendor/markit pinned, see AGENTS.md)
cargo deny check                       # bans + licenses + advisories + sources
./docs/e2e.sh                          # 31 checks: API surface + SIGTERM→restart durability
./docs/e2e-ingestion.sh                # 8 checks: ingest → skip → update → delete
./docs/e2e-retrieval.sh                # 25 checks: vector/lexical/hybrid, propagation, restart
./docs/e2e-relations.sh                # 24 checks: CRUD, multi-hop evidence, cascade
cd site && npm ci && npm run build     # docs site builds (21 pages + search index)
```

Checklist: 14/14 release items evidenced on the `phase-9-release`
branch (see coordinator log; per-item verdicts recorded by the
Phase 9 gate). Dependency tree contains no vector-DB engine:
`cargo tree | grep -iE 'qdrant|pinecone|chroma|lancedb|milvus|weaviate|pgvector'`
returns nothing.

## License

Dual-licensed MIT (`LICENSE-MIT`) OR Apache-2.0 (`LICENSE-APACHE`).
Dependency audit: `docs/audit/license-audit.md` + `deny.toml` policy
(MIT/Apache-2.0 preferred; explicitly-reviewed permissive exceptions
documented). One non-MIT/Apache dependency class is flagged:
**MPL-2.0** (`cssparser`/`selectors` stack via the vendored markit's
HTML parser — weak file-scoped copyleft, reversible by dropping HTML
conversion; flagged for Roger's review).

## Known gaps (recorded, not hidden)

1. **CI not yet committed**: `.github/workflows/ci.yml` is written but
   lives only in the local tree — the stored GitHub token lacks the
   `workflows` scope. It must be committed with a full-scope token
   (pending Roger). Until then, the gates above are run by hand.
2. **Docs site not yet deployed**: `site/` builds clean and is
   Vercel-ready (manual import documented in `site/README.md`), but the
   Vercel project has not been created (Vercel connection degraded —
   pending Roger).
3. **`cargo audit` does not build in this sandbox** (linker env quirk,
   reproduced since Phase 0); advisory coverage comes from
   `cargo deny check` (`advisories`).
4. **Stub provider = mechanics only**: the deterministic stub
   embedding provider is a hash function. Retrieval-quality numbers
   (recall, MRR, latency) validate pipeline mechanics, not semantic
   search quality. Do not cite them as search-quality claims.
5. **Recovery after `kill -9` over HTTP is untested**; restart and
   graceful-shutdown recovery are tested.
6. **Only Linux x86_64 is tested**; other platforms unverified.
7. Rate limiting is an in-memory per-process token bucket (resets on
   restart; behind a proxy it sees the proxy's IP).
8. Tantivy lexical query semantics are OR over bare terms (verified
   against tantivy 0.26.2 sources); the lexical index rebuilds from the
   catalog on every collection open.
9. Vectors are stored as JSON inside redb (not space-efficient);
   marked as future work.
