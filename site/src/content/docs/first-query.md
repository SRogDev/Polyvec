---
title: "First collection, first query"
description: Create a collection, insert vectors directly, and search — the raw-vector path.
---

The [Quickstart](/quickstart/) used the ingest pipeline. This page uses the
raw-vector path: you supply the vectors (your own embeddings) and query by
vector. Both paths land in the same collections and the same search.

Prerequisite: the server running (see [Quickstart](/quickstart/) step 1).

## 1. Create a collection

```bash
curl -X POST localhost:8080/v1/collections \
  -H 'content-type: application/json' \
  -d '{"id":"demo","dimension":3,"metric":"cosine",
       "model":{"name":"my-model","version":"1.0"}}'
# → 201
```

`dimension` is 1–4096 and fixed for the collection's lifetime. The `model`
tag records which embedding model produced the vectors; it is stamped
once — a different model is refused later, never silently mixed (see
[Canonical model](/data/canonical-model/)).

## 2. Insert vectors

Single or atomic batch (`{"vectors":[...]}` — all or nothing):

```bash
curl -X POST localhost:8080/v1/collections/demo/vectors \
  -H 'content-type: application/json' \
  -d '{"vectors":[
        {"id":"vec-1","values":[0.1,0.2,0.3]},
        {"id":"vec-2","values":[0.9,0.1,0.0]}]}'
# → 201
```

Rules: values must match the collection dimension and be finite (no
NaN/Inf) — violations are `400`. Duplicate ids are `409`. A single object
(not wrapped in `vectors`) also works.

## 3. Search

```bash
curl -X POST localhost:8080/v1/collections/demo/vectors/search \
  -H 'content-type: application/json' \
  -d '{"vector":[0.1,0.2,0.3],"k":10,"ef":64}'
# → {"hits":[{"id":"vec-1","distance":0.0},...],"k":10,"ef":64}
```

- `k` is 1–1000; `ef` is 8–10000 (default: the collection's
  `hnsw.ef_search`). Higher `ef` = higher recall, slower search.
- `distance` is the raw metric distance (L2 / cosine distance) — lower is
  better. It is never comparable with BM25 or RRF scores.

## 4. Update, get, delete

```bash
curl localhost:8080/v1/collections/demo/vectors/vec-1          # get
curl -X PUT localhost:8080/v1/collections/demo/vectors/vec-1 \
  -H 'content-type: application/json' -d '{"values":[0.5,0.5,0.5]}'
curl -X DELETE localhost:8080/v1/collections/demo/vectors/vec-1 # 204, idempotent
```

Updates are tombstone + reinsert; deletes hide the vector from search
immediately and are compacted on rebuild. Deleting twice is fine (204
both times).

## 5. Attach documents with metadata

Vectors alone have no text. To get text search and filtering, store
documents (the ingest pipeline does this automatically):

```bash
curl -X PUT localhost:8080/v1/collections/demo/documents \
  -H 'content-type: application/json' \
  -d '{"source":"demo://notes/1",
       "metadata":{"author":"roger"},
       "chunks":[{"text":"hello world","char_start":0,"char_end":11}]}'
# → 200 {"id":"demo#a1b2...","version":1,"chunk_ids":[...]}
```

Upsert is by `source` (idempotent re-ingestion key); the document id is
server-derived (`{collection}#{sha256}`). Query with a filter:

```bash
curl -X POST localhost:8080/v1/collections/demo/query \
  -H 'content-type: application/json' \
  -d '{"text":"hello world","k":5,"mode":"lexical","filter":{"author":"roger"}}'
```

Filters are exact-match on document metadata and behave identically in
`vector`, `lexical`, and `hybrid` modes — see
[Hybrid retrieval + filtering](/search/hybrid-filtering/).

## Clean up

```bash
curl -X DELETE localhost:8080/v1/collections/demo   # 204, removes all data
```

Next: the full [API reference](/api-reference/).
