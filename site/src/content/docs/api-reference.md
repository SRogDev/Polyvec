---
title: API reference
description: The complete Polyvec HTTP API — routes, schemas, errors, auth.
---

Base URL: `http://127.0.0.1:8080`. All `/v1` paths return JSON. The server
binds **localhost only** by default (see [Configuration](/configuration/)).

## Error shape

Every error response has this shape:

```json
{ "error": { "code": "not_found", "message": "not found: vector 'x'", "detail": null } }
```

| Code | Status | Meaning |
| ---- | ------ | ------- |
| `validation_error` | 400 | Bad value: dimension mismatch, non-finite floats, malformed id, bad `k`/`ef`, … |
| `config_error` | 400 | Bad collection config: dimension 0, unknown metric, bad HNSW params |
| `invalid_json` | 400 | Malformed JSON body |
| `invalid_content_type` | 400 | Body is not `application/json` |
| `not_found` | 404 | Unknown collection / vector / document |
| `already_exists` | 409 | Collection or vector id already taken |
| `payload_too_large` | 413 | Body exceeds `POLYVEC_JSON_LIMIT_BYTES` (16 MiB default) |
| `unsupported` | 501 | Operation the engine cannot honor (e.g. text query with no provider) |
| `corrupt` | 500 | Stored data failed an integrity check (safe message; details server-side) |
| `storage_error` | 500 | Storage failure (safe message; details server-side) |
| `internal_error` | 500 | Bug (safe message; details server-side) |
| `service_unavailable` | 503 | Data directory not accessible (`/readyz`) |
| `unauthorized` | 401 | Missing/invalid bearer token (only when `POLYVEC_API_KEY` is set) |
| `rate_limited` | 429 | Per-IP rate limit exceeded; `Retry-After` header tells when to retry |

## Authentication & rate limiting

Off by default — the localhost-only bind remains the protection. To require
a credential on every `/v1` route:

```bash
POLYVEC_API_KEY="a-long-random-secret" polyvec-server
curl -H "Authorization: Bearer a-long-random-secret" localhost:8080/v1/collections
```

Wrong or missing token → `401` plus a `WWW-Authenticate: Bearer` header.
`/healthz` and `/readyz` never require a token. The key is compared in
constant time, never logged, and never appears in error responses.

Rate limiting is a per-IP token bucket on `/v1` routes
(`POLYVEC_RATE_LIMIT_RPS`, default `1000`, burst 4×; `0` disables).
Exceeded → `429` with `Retry-After: 1`. It is in-memory and per-process
(resets on restart; behind a proxy it sees the proxy's IP). If you bind
beyond localhost without setting `POLYVEC_API_KEY`, the server logs a
warning at startup.

## Health & readiness

```bash
curl localhost:8080/healthz   # {"status":"ok","service":"polyvec","version":"0.1.0"}
curl localhost:8080/readyz    # {"status":"ready"} — 503 if the data dir is unusable
```

## Collections

```bash
# Create (201). metric: "l2" or "cosine". model/hnsw optional.
curl -X POST localhost:8080/v1/collections \
  -H 'content-type: application/json' \
  -d '{"id":"kb-main","dimension":384,"metric":"cosine",
       "model":{"name":"bge-m3","version":"1.0"},
       "hnsw":{"m":16,"ef_construction":200,"ef_search":64,"max_elements":1000000}}'

curl localhost:8080/v1/collections               # {"collections":[...],"count":1}
curl localhost:8080/v1/collections/kb-main       # summary
curl localhost:8080/v1/collections/kb-main/stats # index state + counts
curl -X POST localhost:8080/v1/collections/kb-main/flush  # checkpoint index to disk
curl -X DELETE localhost:8080/v1/collections/kb-main      # 204, removes all data
```

Collection ids are slugs: `[a-z0-9-_]`, 1–64 chars. `dimension` is 1–4096.

## Vectors

```bash
# Insert one (single object) or many ({"vectors":[...]} — atomic). 201.
curl -X POST localhost:8080/v1/collections/kb-main/vectors \
  -H 'content-type: application/json' \
  -d '{"vectors":[
        {"id":"vec-1","values":[0.1,0.2,0.3]},
        {"id":"vec-2","values":[0.9,0.1,0.0]}]}'

# Search: k = 1..1000, ef = 8..10000 (default: collection hnsw.ef_search).
curl -X POST localhost:8080/v1/collections/kb-main/vectors/search \
  -H 'content-type: application/json' \
  -d '{"vector":[0.1,0.2,0.3],"k":10,"ef":64}'
# → {"hits":[{"id":"vec-1","distance":0.0},...],"k":10,"ef":64}

curl localhost:8080/v1/collections/kb-main/vectors/vec-1   # {"id","values","chunk_id"}
curl -X PUT localhost:8080/v1/collections/kb-main/vectors/vec-1 \
  -H 'content-type: application/json' -d '{"values":[0.5,0.5,0.5]}'
curl -X DELETE localhost:8080/v1/collections/kb-main/vectors/vec-1  # 204, idempotent
```

Vectors must match the collection dimension and be finite. Duplicate ids
are 409; unknown ids are 404.

## Documents

```bash
# Upsert by `source` (idempotent re-ingestion key). 200.
# Document id is derived: {collection}#{sha256}.
curl -X PUT localhost:8080/v1/collections/kb-main/documents \
  -H 'content-type: application/json' \
  -d '{"source":"https://example.com/guide",
       "metadata":{"author":"roger","pages":3},
       "chunks":[{"text":"hello world","char_start":0,"char_end":11}]}'

# The id contains '#': percent-encode it in the path.
curl 'localhost:8080/v1/collections/kb-main/documents/kb-main%23a1b2...'
curl -X DELETE 'localhost:8080/v1/collections/kb-main/documents/kb-main%23a1b2...'  # 204, cascades
```

`content_hash` (64 hex chars) is optional; defaults to the SHA-256 of the
concatenated chunk texts. `metadata` accepts flat string/number/boolean
values only (nested objects are 400). DELETE cascades to chunks, their
vectors, and relations (see [Relationships](/data/relationships/)).

## Relations

Typed, directed relations between documents/chunks — explicitly supplied,
never extracted. Fixed vocabulary: `references`, `related_to`,
`supersedes`.

```bash
# Create (201). Both endpoints must exist (404 otherwise); self-loops 400;
# duplicate ids 409. Omit "id" for a server-allocated rel-{n:08}.
curl -X POST localhost:8080/v1/collections/kb-main/relations \
  -H 'content-type: application/json' \
  -d '{"id":"rel-guide-advanced","type":"references",
       "source":"kb-main#a1b2...","target":"kb-main#c3d4...",
       "metadata":{"created_by":"roger"}}'

# List/query (200): filter by endpoint and/or type; id order; paginated
# (limit default 100, max 1000; offset default 0).
curl 'localhost:8080/v1/collections/kb-main/relations?type=references&limit=50'
curl 'localhost:8080/v1/collections/kb-main/relations?source=kb-main%23a1b2...'

# Delete (204, idempotent).
curl -X DELETE localhost:8080/v1/collections/kb-main/relations/rel-guide-advanced
```

Relations are immutable: to change one, delete and re-create (ids are
stable, so the same id can be reused). Deleting a document cascades to its
relations in both directions. Full semantics: [Relationships](/data/relationships/).

## Ingest jobs

```bash
# Submit: HTML bytes → markdown → chunks → vectors. 202.
curl -X POST localhost:8080/v1/collections/kb-main/ingest \
  -H 'content-type: application/json' \
  -d '{"source":"https://example.com/guide",
       "filename":"guide.html","mime":"text/html",
       "content":{"base64":"PGgxPkhlbGxvPC9oMT4="},
       "metadata":{"author":"roger"}}'
# → {"job_id":"job-00000001","state":"pending"}

# Or inline text:   "content":{"text":"# Hello\n\nworld"}
# Or client-supplied vectors (no provider needed; 1:1 with chunks):
#   "vectors":[[0.1,0.2,...],...]

# Inspect the job (200; 404 for unknown jobs).
curl localhost:8080/v1/jobs/job-00000001
# Failed jobs keep {"kind":"transient"|"permanent","message":"...","attempt":N}.

# List recent jobs for a collection (newest first; ?limit= default 100, max 1000).
curl 'localhost:8080/v1/collections/kb-main/jobs?limit=10'
```

Re-ingesting unchanged content returns `done` with `"skipped":true`.
`429` (+ `Retry-After`) when the job queue is full; `501` when text
ingest/query needs an embedding provider and none is configured. Full
semantics: [Ingestion](/data/ingestion/).

## Text query

```bash
curl -X POST localhost:8080/v1/collections/kb-main/query \
  -H 'content-type: application/json' \
  -d '{"text":"hello world","k":5,"mode":"hybrid","rrf_k":60}'
```

- `mode`: `"vector"` (default) | `"lexical"` | `"hybrid"`. Any mode
  accepts `expand` for relationship-aware retrieval and `filter` for
  exact-match metadata filtering.
- `k`: 1–1000. `ef`: 8–10000 (vector/hybrid only). `rrf_k`: 1–10000,
  default 60 (hybrid only).
- Hits share one schema: `rank/id/score/score_kind/path` plus chunk text,
  source, metadata, provenance. **Scores are never comparable across
  modes** — only the rank order within one response is meaningful.
- A failing subsystem fails the whole query (502 provider / 500 storage)
  — never a silent fallback. Both sides empty → 200 with `"hits": []`.

Full semantics: [Vector + lexical](/search/vector-lexical/),
[Hybrid + filtering](/search/hybrid-filtering/),
[Relationships](/data/relationships/).

## Durability & shutdown

- Every write commits to redb first: **durable immediately**, no flush needed.
- Search sees writes at once (in-memory index); deletes hide immediately.
- SIGTERM/SIGINT → finish in-flight requests → flush every open
  collection → exit. A `kill -9` loses nothing committed: the next open
  rebuilds the index from redb.
- `POST /v1/collections/{id}/flush` checkpoints on demand (useful before
  backups).

## CPU isolation

All storage work runs on Tokio's blocking thread pool (`web::block`); the
per-collection mutex is held only inside the blocking task, never across
`.await` — a heavy batch insert never stalls `/healthz` or other requests.
