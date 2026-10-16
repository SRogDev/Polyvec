---
title: Hybrid retrieval + filtering
description: Reciprocal Rank Fusion over vector + lexical rankings, and exact-match metadata filters.
---

## Hybrid fusion (RRF)

```bash
curl -X POST localhost:8080/v1/collections/kb/query \
  -H 'content-type: application/json' \
  -d '{"text":"vacation rollover","k":10,"mode":"hybrid","rrf_k":60}'
```

Reciprocal Rank Fusion with `k = 60` (default; configurable per query via
`rrf_k`, valid range 1–10000):

```
score(chunk) = Σ_lists 1 / (k + rank)      # ranks are 1-based
```

- Each subsystem contributes its top-`k` (filtered) ranking; a chunk ranked
  highly by **both** sides outranks one ranked highly by only one. Ties
  break deterministically by chunk id; the fused list is truncated to `k`.
- Fusion keys on **chunk id**. Vector hits with no chunk (raw vectors
  inserted via `/vectors`) cannot fuse — they are **excluded** from hybrid
  results. Hybrid retrieval is document/chunk retrieval; raw vector search
  stays in `vector` mode.
- Hit `score_kind: "rrf"` (**higher is better**), `path: ["vector",
  "lexical"]` (or whichever sides contributed).
- If one subsystem has no matches it contributes an empty list; both empty
  → `200` with `"hits": []`. A failing subsystem fails the whole query —
  never a silent fallback.

## Filters

`filter` is an exact-equality match on **document** metadata, honored
identically by all three modes:

```bash
-d '{"text":"...","k":5,"mode":"lexical","filter":{"author":"roger","pages":3}}'
```

Semantics:

- Every entry must be present with an equal value. Missing key never
  matches. Floats compare exactly — filters are identity checks, not
  similarity. Supported value types: string, integer, float, boolean.
- An empty/absent filter matches everything.

How each mode applies it:

- **lexical:** pushed into the tantivy query as `Must` term queries —
  pre-retrieval, so up to `k` filtered hits return.
- **vector / hybrid:** applied post-retrieval (the ANN index has no filter
  pushdown). The vector side over-fetches `min(k × 4, 1000)` candidates
  and returns the first `k` survivors — **fewer than `k` hits may return**
  when the filter excludes candidates.
- Raw vectors (no document metadata) never satisfy a filter and are
  excluded.

## Result schema (all modes)

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
  follow-up work driven by measurements, not guesses.
- Rebuild-on-open reindexes every collection once per server start —
  acceptable at MVP scale, revisited when measured.
