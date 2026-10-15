# Retrieval (Phase 6)

How exact-term and semantic queries flow through Polyvec: the vector
index (HNSW over embeddings), the lexical index (tantivy BM25 over chunk
text), hybrid fusion (Reciprocal Rank Fusion), and relationship-aware
retrieval (bounded traversal over supplied relations — Phase 6).

## Modes

`POST /v1/collections/{id}/query` takes `mode`:

| Mode | What runs | Needs embedding provider |
| ---- | --------- | ------------------------ |
| `vector` (default) | ANN search over embeddings — the Phase 4 behavior | yes (501 otherwise) |
| `lexical` | BM25 over chunk text (tantivy) | **no** |
| `hybrid` | RRF over the vector + lexical rankings | yes (for the vector side) |

The default is `vector`: Phase 4 clients keep working unchanged. Any mode
accepts `expand` for relationship-aware retrieval (Phase 6, below).

## Lexical index

- **Engine:** tantivy 0.26.2 (`default-features = false`), one index per
  collection at `<data-dir>/<collection>/lexical/`.
- **Schema (fixed):**
  - `chunk_id` — `STRING | STORED`: the chunk's id, one exact token.
  - `doc_id` — `STRING | STORED`: owning document; the upsert/delete key.
  - `text` — `TEXT`: chunk text with tantivy's **default tokenizer**
    (lowercases, splits on non-alphanumeric runs). Indexed, **not stored** —
    hit text is joined back from the catalog, so the index never duplicates
    document content.
  - `meta` — `STRING`, multi-valued: one `key=<JSON value>` term per
    document metadata entry (`author="roger"`, `pages=3`, `ratio=0.5`,
    `draft=false`). Exact-match only; see Filters.
- **Tokenization:** the default tokenizer only. No stemmers (dropped with
  `default-features = false`), no custom analyzers, no field boosts —
  tuning is follow-up work driven by measurements, not guesses.
- **Query syntax:** tantivy's default query grammar (bare terms are ORed —
  tantivy's documented default: `happy tax payer` ≡ `happy OR tax OR payer`;
  use `+term` to require a term, `-term` to exclude it; `"quoted phrases"`
  match phrases). A query the grammar rejects is a `400 validation_error`,
  never a silent rewrite.
- **Threading:** tantivy work runs on Tokio's blocking pool (`web::block`),
  same as HNSW — never on async request workers.

### Consistency model

The redb catalog is the source of truth; the tantivy index is derived and
disposable:

- **Writes:** the ingestion job indexes chunk text right after the vector
  write, in the same job (`DocStore::index_lexical`); deletes scrub the
  index (`DocStore::remove_lexical`), including on the missing-document
  path. The direct `PUT`/`DELETE /documents` endpoints do the same in the
  same request.
- **Failure:** if the lexical write fails after the vector write, the job
  fails and the doc record is never written — the retry re-ingests (new
  version) instead of skipping, so the lexical view converges. The catalog
  always wins disagreements.
- **Open:** the lexical index is **rebuilt from the catalog on every
  collection open** (lazy, once per process lifetime). A stale or corrupt
  tantivy directory can never serve ghost results; the cost is one full
  reindex per collection per server start.

## Hybrid fusion (RRF)

Reciprocal Rank Fusion with `k = 60` (default, configurable per query via
`rrf_k`, valid range 1–10000):

```
score(chunk) = Σ_lists 1 / (k + rank)      # ranks are 1-based
```

Each subsystem contributes its ranking; a chunk ranked highly by *both*
sides outranks one ranked highly by only one. Ties break deterministically
by chunk id. The fused list is truncated to the requested `k`.

Notes:

- Fusion keys on **chunk id**. Vector hits with no chunk (raw vectors
  inserted via `/vectors`) cannot fuse — they are excluded from hybrid
  results. Hybrid retrieval is document/chunk retrieval; raw vector search
  stays in `vector` mode.
- Each side contributes its top-`k` (filtered) ranking, so both subsystems
  do the same amount of work regardless of `rrf_k`.

## Score semantics

Scores are **never comparable across modes** — not across `score_kind`s,
not across queries. Only the rank order *within one response* is
meaningful.

| `score_kind` | Direction | Meaning |
| ------------ | --------- | ------- |
| `distance` | lower is better | Raw metric distance (L2 / cosine distance) to the query vector. |
| `bm25` | higher is better | tantivy BM25 score. Scale is query-dependent. |
| `rrf` | higher is better | RRF fused score. Depends on the fused lists; meaningless across queries. |
| `relationship` | lower is better | Hops from the nearest base hit (`expand` only). Never compared with base-hit scores. |

Every hit also carries `path` (`["vector"]`, `["lexical"]`, both, or
`["relationship"]` for hits added by `expand`) — the retrieval-path
explanation — plus `rank` (1-based in this response).

`id` is the hit's primary address: the **vector id** in `vector` mode (a
raw vector may have no chunk — Phase 4 compatibility), the **chunk id** in
`lexical`/`hybrid` modes.

## Filters

`filter` is an exact-equality match on **document** metadata, e.g.
`{"author": "roger", "pages": 3}`. Supported value types: string, integer,
float, boolean. Semantics are identical in all three modes:

- every entry must be present with an equal value (missing key never
  matches; floats compare exactly — filters are identity checks, not
  similarity);
- an empty/absent filter matches everything.

How each mode applies it:

- **lexical:** pushed into the tantivy query as `Must` term queries —
  pre-retrieval, so up to `k` filtered hits return.
- **vector / hybrid:** applied post-retrieval (the ANN index has no filter
  pushdown in Phase 5). The vector side over-fetches `min(k × 4, 1000)`
  candidates and returns the first `k` survivors — **fewer than `k` hits
  may return** when the filter excludes candidates. Raw vectors (no
  document metadata) never satisfy a filter and are excluded.

## Failure behavior

Predictable, never silently degraded:

| Situation | Behavior |
| --------- | -------- |
| Embedding provider fails (vector/hybrid) | the whole query fails (`502 provider_error`) — no silent lexical fallback |
| tantivy fails (lexical/hybrid) | the whole query fails (`500 storage_error`) |
| One subsystem has no matches | it contributes an empty list; fusion proceeds with the other side |
| Both subsystems empty | `200` with `"hits": []` |
| Invalid lexical query syntax | `400 validation_error` |
| Invalid `mode` / `rrf_k` | `400 validation_error` |
| `mode` omitted | `vector` (Phase 4 behavior) |
| Text query in vector/hybrid with no provider | `501 unsupported` |
| A hit's chunk/document vanished mid-query | the hit is skipped (logged server-side), never served half-built |

## Result schema

```json
{
  "mode": "hybrid",
  "k": 5,
  "ef": 64,
  "rrf_k": 60.0,
  "hits": [
    {
      "rank": 1,
      "id": "kb#a1b2#v2#c000000",
      "score": 0.0325,
      "score_kind": "rrf",
      "path": ["vector", "lexical"],
      "chunk_id": "kb#a1b2#v2#c000000",
      "document_id": "kb#a1b2",
      "vector_id": "kb#a1b2#v2#c000000#v",
      "distance": 0.12,
      "source": "https://example.com/guide",
      "text": "chunk text…",
      "metadata": {"author": "roger"}
    }
  ]
}
```

`ef` appears for `vector`/`hybrid` only; `rrf_k` for `hybrid` only.
`vector_id`/`distance` appear when the hit has a vector origin;
`chunk_id`/`document_id`/`source`/`text`/`metadata` when it resolves to a
chunk. Fields that do not apply are omitted (not null).

## Known limitations

- No reranking (stays out of the MVP critical path).
- No filter pushdown into HNSW (post-retrieval filtering; see above).
- Lexical relevance tuning (tokenizers beyond default, field boosts) is
  follow-up work driven by measurements.
- Rebuild-on-open reindexes every collection once per server start —
  acceptable at MVP scale, revisit with incremental open when measured.
- Chunk-level relations are version-pinned (see below).

## Relationship-aware retrieval (Phase 6)

`POST /v1/collections/{id}/query` accepts an `expand` object alongside
any mode (`vector`, `lexical`, `hybrid`):

```json
{
  "text": "xylophone",
  "k": 5,
  "mode": "lexical",
  "expand": {"depth": 2, "max_nodes": 100, "relation_types": ["references"]}
}
```

The base retrieval runs first (unchanged), then a bounded breadth-first
traversal walks the supplied relations seeded from the base hits' chunk
**and** document ids, and the reached nodes are appended after the base
hits with path evidence. Omitting `expand` keeps Phase 5 behavior
exactly — no separate `relationship` mode exists, because expansion
without a base retrieval has no seeds.

### Relation model

Relations are **explicitly supplied only** — there is no automatic
extraction and no LLM anywhere in this phase. Each relation is a directed
edge `source → target` between two documents/chunks, with a stable id
(caller-chosen or server-allocated `rel-{n:08}`), a type from the fixed
vocabulary, provenance metadata (`created_by` names who/what supplied
it), and a server-stamped `created_at`.

Fixed type vocabulary (new types are a versioned-vocabulary change):

| Type | Meaning |
| ---- | ------- |
| `references` | source cites, links to, or builds on the target |
| `related_to` | source is topically related to the target (weak link) |
| `supersedes` | source supersedes / replaces the target |

Consistency is **referential integrity**: creating a relation requires
both endpoints to name existing documents/chunks (`404` otherwise), and
deleting a document removes every relation touching it or its chunks, in
both directions, in the same operation as the chunk/vector cascade —
there are no dangling relations by construction, no background repair.

Traversal walks edges in **both directions** (a stored `A → B` is walked
forward as `outgoing` and backward as `incoming`); the evidence marks
the direction per hop. Seeds are never returned, each node is visited
once (first visit wins — the recorded path is a shortest path), and
cycles terminate.

### Traversal bounds

| Bound | Default | Hard cap | Rationale |
| ----- | ------- | -------- | --------- |
| `depth` (hops) | 3 | 5 | evidence paths beyond a few hops stop being interpretable |
| `max_nodes` | 100 | 1000 | bounds the merged response at query scale (`k ≤ 1000`) |
| fan-out per node | 64 | — | supernode protection (a doc cited by thousands) |
| wall-clock | 2000 ms | — | backstop; in-memory BFS over redb should never get near it |

Hitting any bound stops the walk and returns the nodes found so far — a
bound is not an error. Fan-out truncation happens after sorting edges by
`(type, id)`, so it is deterministic. `relation_types` narrows the walk
to a subset of the vocabulary.

### Expanded hits

Expanded hits are appended **after** the base hits (base rank order is
never disturbed) in `(hops, id)` order. A node already present as a base
hit is not re-added. Each expanded hit carries:

- `path: ["relationship"]`, `score_kind: "relationship"`, `score` = hops
  as a float (lower is better). Like every other `score_kind`, this is a
  different universe from base-hit scores — only the rank order within
  one response is meaningful.
- `evidence: {hops, path: [{relation_type, direction, relation_id}, …]}`
  — the path that led to the hit.
- Chunk nodes resolve like base chunk hits (chunk id, text, source,
  metadata); document nodes carry document-level fields (no chunk text).
  Expanded hits never carry `vector_id`/`distance` (no vector origin).
- The response echoes the effective config as `expand: {depth,
  max_nodes}`.

An endpoint that vanishes mid-query (raced a delete) is skipped, never
served half-built — the same rule as base hits.

### What it answers that vector/hybrid cannot

The committed fixture (`crates/polyvec-server/tests/relations.rs`,
mirrored live in `docs/e2e-relations.sh`): three documents where only A
mentions the query term, with `A --references--> B --references--> C`.
Lexical alone returns A; `lexical` + `expand: {depth: 2}` returns A, B
(1 hop), and C (2 hops) with exact evidence paths. B and C are
textually distant from the query — no term-overlap or embedding
similarity would surface them; only the supplied structure does.

Honest scope note: this demonstrates *structural* retrieval value, not
semantic quality. Whether relations improve answer quality on a real
corpus is unmeasured (no production corpus exists yet) — the subsystem
gives agents the mechanism (bounded, evidenced expansion), not a
relevance claim.

### Known limitations (Phase 6)

- No automatic relation extraction (explicit supply only).
- No graph query language; traversal is bounded BFS only.
- Chunk-level relations are **version-pinned**: re-ingesting a document
  mints new chunk ids and drops the old chunk records without migrating
  relations, so a chunk→chunk relation points at the retired version
  after re-ingest. Document-level relations are stable (document ids are
  source-derived and survive re-ingest).
