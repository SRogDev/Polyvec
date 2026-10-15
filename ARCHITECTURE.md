# ARCHITECTURE — Polyvec

> How the codebase is organized and why. Decisions live in `docs/decisions/`
> as ADRs; this file is the map, not the minutes.

## Thesis

Polyvec is a from-scratch vector database engine in Rust, optimized for one
workload: **massive, cheap, high-recall reads of company knowledge by AI
agents**. Every architectural choice is scored against read cost at scale —
not write throughput, not feature breadth.

## Workspace layout

```text
Polyvec/
├── Cargo.toml                  # [workspace]: members, pinned deps, shared lints
├── Cargo.lock                  # committed (first binary shipped in Phase 3)
├── rustfmt.toml                # edition = "2024"
├── deny.toml                   # license policy (MIT/Apache-2.0 + documented exceptions)
├── audit.toml                  # cargo-audit config
├── crates/
│   ├── polyvec-core/           # Phase 0. Domain vocabulary (see below).
│   ├── polyvec-index/          # Phase 2. ANN index layer (see below).
│   ├── polyvec-storage/        # Phase 2. Durable storage layer (see below).
│   ├── polyvec-server/         # Phase 3. Actix Web HTTP API (see below).
│   ├── polyvec-ingestion/      # Phase 4. Canonical model, chunking, jobs.
│   ├── polyvec-embeddings/     # Phase 4. Provider trait, stub + remote adapters.
│   └── polyvec-retrieval/      # Phase 5. tantivy lexical index, RRF fusion.
│   └── polyvec-bench/          # Phase 7. fixtures, Criterion benches, bench_report.
├── vendor/markit/              # vendored markit (pinned 4197c76a, HTTP stubbed — Phase 4)
├── integrate-polyvec/          # versioned AI integration prompt + live test record (Phase 8)
├── docs/
│   ├── PROJECT_BRIEF.md        # full context handoff for another AI
│   ├── API.md                  # HTTP API reference + curl examples
│   ├── ingestion.md            # ingestion pipeline + cost-guard mechanisms
│   ├── retrieval.md            # retrieval modes, scores, filters, failure behavior
│   ├── e2e.sh                  # runnable end-to-end smoke test (Phase 3)
│   ├── decisions/              # ADRs (ADR-001 … ADR-004)
│   ├── audit/                 # crate & license audit reports (Phase 1+)
│   └── research/               # pre-plan landscape research
├── ARCHITECTURE.md             # this file
├── BENCHMARKS.md               # benchmark methodology + measured results (Phase 7)
├── CONTRIBUTING.md
├── SECURITY.md                 # policy + Phase 3 HTTP threat model + Phase 7 security delta
├── SaaS.md                     # future SaaS requirements — notes only, not built
└── .github/workflows/ci.yml    # local-only until token scope is resolved
```

## Crate responsibilities (plan §4)

The definitive plan defines seven crates with fixed responsibilities.
**Only `polyvec-core` exists in Phase 0** — the other six are created in the
phase that first needs them (see "No empty placeholder crates" below).

| Crate | Responsibility (plan §4) | First needed |
| ----- | ------------------------ | ------------ |
| `polyvec-core` | Domain vocabulary: types, validated IDs, config contracts, shared `Error` | Phase 0 (exists) |
| `polyvec-index` | Vector index: hnsw_rs wrapper, Polyvec-owned tombstones, exact-search baseline | Phase 2 (exists) |
| `polyvec-storage` | Durable catalog: redb-backed collections/documents/metadata, persistence, recovery | Phase 2 (exists) |
| `polyvec-server` | Actix Web routes, API schemas, config, graceful shutdown | Phase 3 (exists) |
| `polyvec-ingestion` | Canonical model, chunking, ingestion jobs, converter adapter (markit) | Phase 4 (exists) |
| `polyvec-embeddings` | Provider interface, adapters, batching, dimension validation | Phase 4 (exists) |
| `polyvec-retrieval` | Lexical (tantivy), hybrid fusion, result contract (Phase 5); bounded graph retrieval (Phase 6) | Phase 5 (exists) |

Phase numbers are first-need per the definitive plan (Phases 0–9); a phase that
discovers it doesn't need its crate doesn't create it.

## Dependency boundaries (the one rule that matters)

Dependencies point **inward only**. Arrows mean "depends on":

```text
polyvec-server
    └── polyvec-retrieval       (tantivy lexical, hybrid fusion — Phase 5+)
            └── polyvec-embeddings    (provider SDKs, HTTP clients — Phase 4+)
            └── polyvec-ingestion    (canonical model, jobs — Phase 4+)
            └── polyvec-index        (hnsw_rs ANN wrapper — Phase 2+)
            └── polyvec-storage      (redb catalog — Phase 2+)
                    └── polyvec-core   (domain types, IDs, config, Error)
```

- **`polyvec-core` has no framework dependencies.** No Actix, no Tokio, no
  HTTP, no provider SDKs — only `thiserror` today. It must stay compilable as
  a pure library: embed it in tests, CLIs, and future crates without dragging
  a web stack along.
- Each crate depends only on crates below it. If a lower crate needs
  something from above, the abstraction is in the wrong place — move the
  trait definition down, not the dependency up.
- Shared lints are inherited from the workspace root (`[lints] workspace =
  true` in each crate): `unsafe` forbidden, `missing_docs` warned, Clippy
  `all` + `pedantic`.

## No empty placeholder crates

A deliberate decision, not laziness: creating six empty crates with
`lib.rs` stubs would add maintenance surface (lints, docs, version churn)
with zero behavior. Per the plan's "no abstraction without a concrete use
case" principle, each crate is born in the PR that gives it its first real
responsibility. `ARCHITECTURE.md` grows with the workspace.

## Data flow (as of Phase 2)

The first real flow exists — write path and read path through
`CollectionStore` (sync Rust API; HTTP arrives in Phase 3):

```text
insert_vector(v)
  │  1. validate (dimension, finiteness)          [polyvec-core]
  │  2. redb txn: vector record + generation++    [polyvec-storage catalog]
  └─ 3. hnsw insert (node id ← monotonic counter) [polyvec-index AnnIndex]

search(query, k)
  │  1. validate query                            [polyvec-core]
  │  2. hnsw search(k + tombstones), post-filter  [polyvec-index]
  └─ 3. node id → VectorId                        [polyvec-storage reverse map]

delete_vector(id)
  │  1. redb txn: tombstone flag + generation++   (durable immediately)
  └─ 2. tombstone set insert                      (hidden from search at once)

flush()
  │  1. maybe rebuild()  (tombstones > 1/5 of live, ≥ 16)
  │  2. dump index → temp files, rename into place
  │  3. sidecar.json (SHA-256 checksums, generation)
  └─ 4. redb txn: synced_generation = generation
```

One directory per collection: `catalog.redb`, `index.hnsw.{graph,data}`,
`sidecar.json`. Recovery on open follows ADR-004 (rebuild from redb when
the dump is missing/stale; `Error::Corrupt` — never a panic — on
version/checksum mismatch).

## Data flow (as of Phase 3 — HTTP)

The server crate adds the network edge. Actix Web workers handle HTTP;
**no handler runs storage code on an async worker** — every
`CollectionStore` call goes through `web::block` on Tokio's blocking pool
(HNSW insert/search are CPU-bound), with the per-collection `Mutex` held
only inside the blocking closure, never across `.await`.

```text
POST /v1/collections/{id}/vectors/search
  │  0. SecurityMiddleware: bearer token (401 when POLYVEC_API_KEY is set) +
  │     per-IP token bucket (429 + Retry-After)         [polyvec-server security]
  │  1. route + Json extract (16 MiB limit → 413)      [polyvec-server routes]
  │  2. id/path validation via core constructors        [polyvec-core]
  │     (CollectionId::new, VectorId::parse; k=1..=1000, ef=8..=10000)
  │  3. web::block → blocking pool:
  │       registry.get_or_open(id)   (lazy open + recovery)
  │       Mutex<CollectionStore> op  (search → SearchHit)
  └─ 4. JSON response DTO                               [polyvec-server schemas]

error path: polyvec_core::Error → ApiError → { "error": { "code",
"message", "detail" } } (Validation/Config→400, NotFound→404,
AlreadyExists→409, Unsupported→501, Corrupt/Storage/Internal→500 with
safe messages — details go to the server log only)
```

### Ingestion data flow (Phase 4)

```text
POST /v1/collections/{id}/ingest {source, content.{text|base64}, metadata?, vectors?}
  │  1. route + Json extract                       [polyvec-server routes]
  │  2. body validation; 501 fail-fast when text needs a provider and none
  │     is configured; 404 when the collection is unknown
  │  3. JobRunner.try_submit_ingest → 202 {job_id}  [polyvec-ingestion]
  │     (bounded queue → 429 + Retry-After when full; idempotency key
  │      dedups in-flight duplicates)
  │  4. accept loop → blocking pool per job:
  │       convert (markit → Markdown) → canonicalize (deterministic;
  │         original + canonical hashes) → change-detect (unchanged →
  │         skip; vectors_present guard finishes partial writes) →
  │       chunk (section-aware, deterministic ids) →
  │       embed (supplied vectors or provider, batched; dimension checked;
  │         collection model tag stamped once) →
  │       ingest_document (atomic: old version tombstoned, new stored)
  │  5. GET /v1/jobs/{job_id} → state + error detail / affected records
```

`JobRunner` owns a `JobStore` (`{data_dir}/jobs.redb`): the state machine
`pending → running → done | failed` (+ `retrying`, bounded attempts with
exponential backoff honoring 429 `Retry-After`). Transient (429/5xx/
timeout) vs permanent (4xx/auth/dimension) classification — permanent is
never retried. `POST /v1/collections/{id}/query` answers in `vector`
(default), `lexical`, or `hybrid` mode — see "Retrieval data flow
(Phase 5)" below and `docs/retrieval.md`. Full reference: `docs/API.md`,
`docs/ingestion.md`.

`Registry` (in `polyvec-server`) holds `data_dir` +
`Mutex<HashMap<CollectionId, CollectionHandle>>`, where a
`CollectionHandle` is the vector store plus the tantivy lexical index for
one collection. Collections open lazily on first use; `GET /v1/collections`
lists from memory plus cheap catalog config reads (`read_collection_config`
— no index load; redb forbids opening a database twice in one process, so
open stores are never re-opened from disk). The binary (`polyvec-server`)
parses env config, refuses non-loopback binds without explicit opt-in, and
on SIGTERM/SIGINT finishes in-flight requests (`HttpServer::stop(true)`)
then flushes every open collection. Full endpoint reference:
`docs/API.md`; threat model: `SECURITY.md`.

### Retrieval data flow (Phase 5)

```text
POST /v1/collections/{id}/query {text, k, ef?, mode?, filter?, rrf_k?}
  │  1. route + Json extract; mode parse (default "vector"),
  │     filter/rrf_k validation                            [polyvec-server routes]
  │  2. vector/hybrid: 501 when no provider; else embed on the
  │     blocking pool (remote HTTP is blocking)            [polyvec-embeddings]
  │  3. web::block → blocking pool, one CollectionHandle:
  │       lexical ≠ vector: tantivy BM25 search(text, k, filter)
  │                         (filters pushed into the query) [polyvec-retrieval]
  │       vector ≠ lexical: HNSW search(k×4 when filtered, ef)
  │                         → post-filter on document metadata,
  │                         truncate to k                     [polyvec-index]
  │       hybrid: RRF(k=60 default) over both rankings,
  │               fused on chunk id, top-k kept        [polyvec-retrieval]
  │       enrich: chunk text + source + metadata joined from
  │               the catalog (stale hits skipped, never half-built)
  └─ 4. common result schema: rank/id/score/score_kind/path +
        chunk/document/vector ids, distance, source, text, metadata
```

The lexical index (`<collection>/lexical/`) is rebuilt from the catalog
on every collection open — the catalog is the source of truth, tantivy is
derived. Ingestion jobs index chunk text right after the vector write in
the same job (`DocStore::index_lexical`); a lexical failure fails the job
and retries re-ingest rather than skip, so the two views converge. The
direct `PUT`/`DELETE /documents` endpoints maintain the lexical index in
the same request. Full semantics: `docs/retrieval.md`.

### Relation model and traversal bounds (Phase 6)

Relations are catalog-like, provenance-bearing records — so redb is
their home, next to the document catalog (not a separate store):

- Tables: `relations` (`rel_id → Relation` JSON) plus `rel_by_source` /
  `rel_by_target` secondary indexes with composite keys
  `{endpoint}\0{relation_id}` (prefix scans serve incident-edge lookups
  and cascade deletes). Every relation write maintains all three rows in
  one redb transaction.
- No catalog format-version bump: `Catalog::open` creates missing tables
  idempotently, so pre-Phase-6 catalogs keep opening with no migration
  step (the value encodings of existing tables are untouched).
- The id counter (`next_relation_id` in `meta`) is durable — server
  restarts never re-allocate a relation id.

Consistency is **referential integrity** (chosen over recoverable
consistency): endpoints must exist at creation; `delete_document` drops
every relation touching the document or its chunks, in both directions,
in the same operation path as the chunk/vector cascade (this covers the
HTTP route and the ingestion runner — both funnel through
`CollectionStore::delete_document`). There are no dangling relations by
construction.

Traversal (`polyvec-retrieval::relations::traverse`) is a pure function
over a caller-supplied adjacency closure — no redb inside, unit-tested
with in-memory graphs. Bounded BFS: visited set (cycle protection,
shortest-path evidence), `depth ≤ 5` (default 3), `max_nodes ≤ 1000`
(default 100) per query, fan-out cap 64 per node (deterministic
`(type, id)`-ordered truncation), 2000 ms wall-clock backstop. Edges are
walked in both directions with the direction recorded per hop; seeds are
the base hits' chunk + document ids; reached nodes append after the base
hits with `score_kind: "relationship"` and `evidence: {hops, path}`.
No graph query language exists — bounded BFS only. Full semantics:
`docs/retrieval.md`.

## Key design decisions (ADRs)

- **ADR-001** — Actix Web over Axum for the HTTP layer (`docs/decisions/`).
- **ADR-002** — Modular monolith (one repo, many crates) over microservices
  or a single flat crate.
- **ADR-003** — Dual MIT/Apache-2.0 licensing and the dependency policy it
  forces (MIT/Apache-2.0 preferred; explicitly-reviewed permissive licenses
  via documented `deny.toml` exceptions; copyleft banned).
- **ADR-004** — Storage commit protocol and consistency model (redb commits
  first as source of truth; ANN dump is a checksummed, rebuildable cache;
  honest recovery rules).
