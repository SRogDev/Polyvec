---
title: Vector + lexical search
description: HNSW vector search and tantivy BM25 lexical search — semantics, scores, failure behavior.
---

`POST /v1/collections/{id}/query` takes `mode`:

| Mode | What runs | Needs embedding provider |
| ---- | --------- | ------------------------ |
| `vector` (default) | ANN search over embeddings — HNSW via hnsw_rs | yes (501 otherwise) |
| `lexical` | BM25 over chunk text (tantivy) | **no** |
| `hybrid` | RRF over the vector + lexical rankings | yes (for the vector side) |

The default is `vector`: clients that omit `mode` get the original text-query
behavior. Any mode accepts `expand` ([Relationships](/data/relationships/))
and `filter` ([Hybrid + filtering](/search/hybrid-filtering/)).

## Vector search

```bash
curl -X POST localhost:8080/v1/collections/kb/query \
  -H 'content-type: application/json' \
  -d '{"text":"vacation rollover policy","k":10,"ef":64,"mode":"vector"}'
```

The text is embedded with the configured provider on the blocking pool
(the remote HTTP adapter is blocking), then HNSW search runs:

- `k` 1–1000; `ef` 8–10000 (default: the collection's `hnsw.ef_search`).
  Higher `ef` explores more of the graph — higher recall, higher latency.
- Hit `id` is the **vector id** (a raw vector inserted via `/vectors` may
  have no chunk — the Phase 4 response shape is preserved inside hits).
- `score_kind: "distance"` — raw metric distance (L2 / cosine distance),
  **lower is better**. Only the rank order within one response is meaningful.

If the embedding provider fails, the whole query fails (`502
provider_error`) — there is never a silent lexical fallback.

## Lexical search

```bash
curl -X POST localhost:8080/v1/collections/kb/query \
  -H 'content-type: application/json' \
  -d '{"text":"vacation +rollover -sick","k":10,"mode":"lexical"}'
```

- **Engine:** tantivy 0.26.2 (`default-features = false`), one index per
  collection at `<data-dir>/<collection>/lexical/`.
- **What is indexed:** chunk text with tantivy's default tokenizer
  (lowercases, splits on non-alphanumeric runs). No stemmers, no custom
  analyzers, no field boosts. Hit text is joined back from the catalog —
  the index never duplicates document content.
- **Query syntax:** tantivy's default grammar — bare terms are **OR**ed
  (`happy tax payer` ≡ `happy OR tax OR payer`); `+term` requires a term,
  `-term` excludes it; `"quoted phrases"` match phrases. A query the
  grammar rejects is `400 validation_error`, never a silent rewrite.
- Hit `id` is the **chunk id**; `score_kind: "bm25"` — tantivy BM25,
  **higher is better**, query-dependent scale.
- Needs **no embedding provider** — lexical works with
  `POLYVEC_EMBEDDING_PROVIDER=none`.

### Consistency

The redb catalog is the source of truth; the tantivy index is derived and
disposable:

- Ingestion jobs index chunk text right after the vector write, in the
  same job; updates re-index and deletes scrub the index — stale content
  never surfaces lexically.
- If the lexical write fails after the vector write, the job fails and the
  retry re-ingests (new version) instead of skipping, so the two views
  converge.
- The index **rebuilds from the catalog on every collection open** (lazy,
  once per process lifetime). A stale or corrupt tantivy directory can
  never serve ghost results; the cost is one full reindex per collection
  per server start.

## Score semantics

Scores are **never comparable across modes** — not across `score_kind`s,
not across queries. Only the rank order *within one response* is
meaningful.

| `score_kind` | Direction | Meaning |
| ------------ | --------- | ------- |
| `distance` | lower is better | Raw metric distance to the query vector |
| `bm25` | higher is better | tantivy BM25 score, query-dependent scale |
| `rrf` | higher is better | RRF fused score (hybrid mode) |
| `relationship` | lower is better | Hops from the nearest base hit (`expand` only) |

Every hit also carries `path` (`["vector"]`, `["lexical"]`, both, or
`["relationship"]`) — the retrieval-path explanation — plus 1-based
`rank`. Fields that do not apply are omitted, never null.

## Failure behavior

| Situation | Behavior |
| --------- | -------- |
| Embedding provider fails (vector/hybrid) | whole query fails (`502 provider_error`) |
| tantivy fails (lexical/hybrid) | whole query fails (`500 storage_error`) |
| Invalid lexical query syntax | `400 validation_error` |
| Text query in vector/hybrid with no provider | `501 unsupported` |
| A hit's chunk/document vanished mid-query | skipped (logged server-side), never half-built |

## When to use which

- **`vector`** when you have a configured embedding provider and want
  semantic similarity over your corpus.
- **`lexical`** when you need exact-term matching, have no provider, or
  want deterministic term behavior (ids, error codes, rare terms).
- **`hybrid`** when you want both rankings fused — see
  [Hybrid retrieval + filtering](/search/hybrid-filtering/).
