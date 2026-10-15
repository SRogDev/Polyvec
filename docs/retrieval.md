# Retrieval (Phase 5)

How exact-term and semantic queries flow through Polyvec: the vector
index (HNSW over embeddings), the lexical index (tantivy BM25 over chunk
text), and hybrid fusion (Reciprocal Rank Fusion). Relationship-aware
retrieval arrives in Phase 6.

## Modes

`POST /v1/collections/{id}/query` takes `mode`:

| Mode | What runs | Needs embedding provider |
| ---- | --------- | ------------------------ |
| `vector` (default) | ANN search over embeddings — the Phase 4 behavior | yes (501 otherwise) |
| `lexical` | BM25 over chunk text (tantivy) | **no** |
| `hybrid` | RRF over the vector + lexical rankings | yes (for the vector side) |

The default is `vector`: Phase 4 clients keep working unchanged.

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
- **Query syntax:** tantivy's default query grammar (terms are ANDed;
  `"quoted phrases"` match phrases; `+`/`-` are operators). A query the
  grammar rejects is a `400 validation_error`, never a silent rewrite.
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

Every hit also carries `path` (`["vector"]`, `["lexical"]`, or both) —
the retrieval-path explanation — plus `rank` (1-based in this response).

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
