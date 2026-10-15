# Polyvec HTTP API (Phase 7)

Base URL: `http://127.0.0.1:8080`. All `/v1` paths return JSON.
The server binds **localhost only** by default; see [Configuration](#configuration).

Error responses always have this shape:

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
| `payload_too_large` | 413 | Body exceeds `POLYVEC_JSON_LIMIT_BYTES` |
| `unsupported` | 501 | Operation the engine cannot honor |
| `corrupt` | 500 | Stored data failed an integrity check (safe message; details in server logs) |
| `storage_error` | 500 | Storage failure (safe message; details in server logs) |
| `internal_error` | 500 | Bug (safe message; details in server logs) |
| `service_unavailable` | 503 | Data directory not accessible (`/readyz`) |
| `unauthorized` | 401 | Missing/invalid bearer token (only when `POLYVEC_API_KEY` is set) |
| `rate_limited` | 429 | Per-IP rate limit exceeded; `Retry-After` header tells when to retry |

## Authentication & rate limiting (Phase 7)

Off by default — the localhost-only bind remains the protection, as in
earlier phases. To require a credential on every `/v1` route:

```bash
POLYVEC_API_KEY="a-long-random-secret" polyvec-server
curl -H "Authorization: Bearer a-long-random-secret" localhost:8080/v1/collections
```

Wrong or missing token → `401 {"error":{"code":"unauthorized",…}}` plus a
`WWW-Authenticate: Bearer` header. `/healthz` and `/readyz` never require
a token.

Rate limiting is a per-IP token bucket on `/v1` routes
(`POLYVEC_RATE_LIMIT_RPS`, default `1000`, burst 4×; `0` disables).
Exceeded → `429 {"error":{"code":"rate_limited",…}}` with `Retry-After: 1`.
If you bind beyond localhost (`POLYVEC_ALLOW_NONLOCAL_BIND=1`) without
setting `POLYVEC_API_KEY`, the server logs a warning at startup — the API
is then reachable with no credential.

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

curl localhost:8080/v1/collections            # {"collections":[...],"count":1}
curl localhost:8080/v1/collections/kb-main    # summary
curl localhost:8080/v1/collections/kb-main/stats   # index state + counts
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
# {"hits":[{"id":"vec-1","distance":0.0},...],"k":10,"ef":64}

curl localhost:8080/v1/collections/kb-main/vectors/vec-1   # {"id","values","chunk_id"}
curl -X PUT localhost:8080/v1/collections/kb-main/vectors/vec-1 \
  -H 'content-type: application/json' \
  -d '{"values":[0.5,0.5,0.5]}'
curl -X DELETE localhost:8080/v1/collections/kb-main/vectors/vec-1  # 204 (idempotent)
```

Vectors must match the collection dimension and be finite (no NaN/Inf).
Duplicate ids are 409; unknown ids are 404.

## Documents

```bash
# Upsert by `source` (idempotent re-ingestion key). 200.
# The document id is derived: {collection}#{sha256}. Chunk ids are
# {document}#c{index:06}; the server derives them from array position.
curl -X PUT localhost:8080/v1/collections/kb-main/documents \
  -H 'content-type: application/json' \
  -d '{"source":"https://example.com/guide",
       "metadata":{"author":"roger","pages":3},
       "chunks":[{"text":"hello world","char_start":0,"char_end":11}]}'
# {"id":"kb-main#a1b2...","version":1,"chunk_ids":[...]}

# The id contains '#': percent-encode it in the path.
curl 'localhost:8080/v1/collections/kb-main/documents/kb-main%23a1b2...'
curl -X DELETE 'localhost:8080/v1/collections/kb-main/documents/kb-main%23a1b2...'  # 204, cascades
```

`content_hash` (64 hex chars) is optional; it defaults to the SHA-256 of the
concatenated chunk texts. `metadata` accepts flat string/number/boolean
values only. DELETE cascades to chunks and their vectors.

## Relations (Phase 6)

Typed, directed relations between documents/chunks — explicitly supplied,
never extracted. Fixed vocabulary: `references`, `related_to`,
`supersedes`. Document ids contain `#`: percent-encode them in paths.

```bash
# Create (201). Both endpoints must exist (404 otherwise); self-loops are
# 400; duplicate ids are 409. Omit "id" for a server-allocated rel-{n:08}.
curl -X POST localhost:8080/v1/collections/kb-main/relations \
  -H 'content-type: application/json' \
  -d '{"id":"rel-guide-advanced","type":"references",
       "source":"kb-main#a1b2...","target":"kb-main#c3d4...",
       "metadata":{"created_by":"roger"}}'
# {"id":"rel-guide-advanced","type":"references","source":"...","target":"...",
#  "metadata":{"created_by":"roger"},"created_at":1790000000}

# List/query (200): filter by endpoint and/or type; id order; paginated
# (limit default 100, max 1000; offset default 0).
curl 'localhost:8080/v1/collections/kb-main/relations?type=references&limit=50'
curl 'localhost:8080/v1/collections/kb-main/relations?source=kb-main%23a1b2...'
# {"relations":[...],"count":2,"limit":50,"offset":0}

# Delete (204, idempotent).
curl -X DELETE localhost:8080/v1/collections/kb-main/relations/rel-guide-advanced
```

Deleting a document cascades to its relations (both directions, including
relations on its chunks) — no orphaned relations are retrievable
afterwards. Chunk-level relations are version-pinned: re-ingesting a
document mints new chunk ids, so chunk→chunk edges keep pointing at the
retired version; document-level relations are stable across re-ingests.

Relations are immutable after creation: to change a relation's type or
endpoints, delete it and create a new one (ids are stable, so clients can
re-create with the same id).

## Relationship-aware retrieval (Phase 6)

Any query mode accepts `expand`: run the base retrieval, then bounded
BFS over relations seeded from the base hits, appending reached nodes
with evidence (`{hops, path:[{relation_type, direction, relation_id}]}`).

```bash
curl -X POST localhost:8080/v1/collections/kb-main/query \
  -H 'content-type: application/json' \
  -d '{"text":"hello world","k":5,"mode":"lexical",
       "expand":{"depth":2,"max_nodes":100,"relation_types":["references"]}}'
# {"mode":"lexical","k":5,"expand":{"depth":2,"max_nodes":100},
#  "hits":[ {"rank":1, ... base hit ...},
#           {"rank":2,"id":"kb-main#c3d4...","score":1.0,
#            "score_kind":"relationship","path":["relationship"],
#            "document_id":"kb-main#c3d4...","source":"...",
#            "metadata":{...},
#            "evidence":{"hops":1,"path":[
#              {"relation_id":"rel-guide-advanced","relation_type":"references",
#               "direction":"outgoing"}]}} ]}
```

Defaults: `depth` 3 (max 5), `max_nodes` 100 (max 1000); a per-node
fan-out cap of 64 and a 2000 ms wall-clock backstop apply always.
Expanded hits never carry `vector_id`/`distance`; `score` is the hop
count (lower is better) and is never comparable with base-hit scores.
Full semantics: `docs/retrieval.md`; live smoke test:
`docs/e2e-relations.sh`.

## Ingestion (Phase 4)

```bash
# Submit an ingest job: HTML bytes → markdown → chunks → vectors. 202.
curl -X POST localhost:8080/v1/collections/kb-main/ingest \
  -H 'content-type: application/json' \
  -d '{"source":"https://example.com/guide",
       "filename":"guide.html","mime":"text/html",
       "content":{"base64":"PGgxPkhlbGxvPC9oMT4="},
       "metadata":{"author":"roger"}}'
# {"job_id":"job-00000001","state":"pending"}

# Or inline text:
#   "content":{"text":"# Hello\n\nworld"}

# Or client-supplied vectors (no provider needed; 1:1 with chunks):
#   "vectors":[[0.1,0.2,...],...]

# Inspect the job (200; 404 for unknown jobs).
curl localhost:8080/v1/jobs/job-00000001
# {"job_id":"...","state":"done","attempts":1,...,
#  "affected":{"document_id":"...","version":1,"chunks":3,"vectors":3,"skipped":false},
#  "error":null}
# Failed jobs keep {"kind":"transient"|"permanent","message":"...","attempt":N}.

# List recent jobs for a collection (newest first; ?limit=, default 100, max 1000).
curl 'localhost:8080/v1/collections/kb-main/jobs?limit=10'

# Text query: retrieval modes (Phase 5)
#
# mode: "vector" (default) | "lexical" | "hybrid".
# Lexical needs no embedding provider; vector/hybrid do (501 otherwise).
curl -X POST localhost:8080/v1/collections/kb-main/query \
  -H 'content-type: application/json' \
  -d '{"text":"hello world","k":5,"mode":"hybrid","rrf_k":60}'
# {"mode":"hybrid","k":5,"ef":32,"rrf_k":60.0,
#  "hits":[{"rank":1,"id":"...","score":0.0325,"score_kind":"rrf",
#            "path":["vector","lexical"],
#            "chunk_id":"...#v1#c000000","document_id":"...","vector_id":"...",
#            "distance":0.12,"source":"https://example.com/guide",
#            "text":"chunk text...","metadata":{"author":"roger"}}]}
#
# Modes:
#   vector  (default): embed via the provider, then ANN search. score_kind
#             "distance" (lower is better); id is the vector id.
#   lexical: BM25 over chunk text (tantivy). No provider needed. score_kind
#             "bm25" (higher is better); id is the chunk id.
#   hybrid:  Reciprocal Rank Fusion of the vector + lexical rankings
#             (rrf_k default 60). score_kind "rrf" (higher is better).
# Scores are NEVER comparable across modes — only the rank order within one
# response is meaningful. Full semantics: docs/retrieval.md.
#
# Exact-match metadata filter, honored by all three modes:
#   -d '{"text":"...","k":5,"mode":"lexical","filter":{"author":"roger"}}'
# Vector/hybrid apply filters post-retrieval (fewer than k hits may
# return); lexical pushes them into the tantivy query.
#
# A failing subsystem fails the whole query (502 provider / 500 storage) —
# never a silent fallback. One side with no matches contributes an empty
# list; both empty → 200 with "hits": [].
#
# Legacy note: the Phase 4 query response ({"hits":[{"id","distance",
# "chunk_id"}],"k","ef"}) is superseded by the schema above; "distance"
# and the vector-id "id" are preserved inside vector-mode hits.
```

Re-ingesting unchanged content returns `done` with `"skipped":true` — no new
version, no new vectors. Changed content bumps the version and tombstones
the old vectors immediately. Statuses: `429` (+ `Retry-After`) when the job
queue is full; `501` (`unsupported`) when text ingest/query needs an embedding
provider and none is configured — set `POLYVEC_EMBEDDING_PROVIDER=stub`
(dev/test only) or `remote`, or supply `vectors`.

## Configuration

| Variable | Default | Meaning |
| -------- | ------- | ------- |
| `POLYVEC_BIND` | `127.0.0.1:8080` | `IP:port` to listen on (`localhost` accepted as alias for `127.0.0.1`) |
| `POLYVEC_DATA_DIR` | `./polyvec-data` | One sub-directory per collection; created on startup |
| `POLYVEC_JSON_LIMIT_BYTES` | `16777216` (16 MiB) | Max JSON body; larger → `413` |
| `POLYVEC_WORKERS` | (CPU count) | HTTP worker threads |
| `POLYVEC_ALLOW_NONLOCAL_BIND` | unset | Set to `1` to allow a non-loopback bind |
| `POLYVEC_EMBEDDING_PROVIDER` | `none` | `none` \| `stub` (dev/test only) \| `remote` (OpenAI-compatible) |
| `POLYVEC_STUB_DIMENSION` | `64` | Stub provider dimension (must match the collection) |
| `POLYVEC_EMBEDDING_BASE_URL` | — | Remote endpoint base URL (required for `remote`) |
| `POLYVEC_EMBEDDING_API_KEY` | — | Remote API key (env only, never logged) |
| `POLYVEC_EMBEDDING_MODEL` | — | Remote model name (required; stamped as the collection's model tag) |
| `POLYVEC_EMBEDDING_MODEL_VERSION` | `1` | Remote model version |
| `POLYVEC_EMBEDDING_DIMENSION` | — | Expected embedding dimension (required) |
| `POLYVEC_EMBEDDING_BATCH_SIZE` | `64` | Texts per HTTP call |
| `POLYVEC_EMBEDDING_TIMEOUT_SECS` | `30` | Per-batch timeout |
| `POLYVEC_JOB_QUEUE_CAPACITY` | `64` | Submit queue bound (`429` past this) |
| `POLYVEC_JOB_CONCURRENCY` | `4` | Jobs running concurrently |
| `POLYVEC_JOB_MAX_ATTEMPTS` | `5` | Attempt budget per job |

Binding a non-loopback address without `POLYVEC_ALLOW_NONLOCAL_BIND=1`
refuses to start. **Phase 3 has no authentication** — anyone who can reach
the socket has full read/write/delete. Keep it on localhost, or put it
behind a reverse proxy / firewall you trust. See `SECURITY.md`.

## Durability & shutdown

- Every write commits to redb first: **durable immediately**, no flush needed.
- Search sees writes at once (in-memory index); deletes hide immediately.
- SIGTERM/SIGINT → finish in-flight requests → flush (checkpoint) every open
  collection → exit. A `kill -9` loses nothing committed: the next open
  rebuilds the index from redb (see `docs/decisions/ADR-004-*`).
- `POST /v1/collections/{id}/flush` checkpoints on demand (useful before backups).

## CPU isolation

HNSW insert/search are CPU-bound. All storage work runs on Tokio's blocking
thread pool (`web::block`); async request workers never block, so a heavy
batch insert does not stall `/healthz` or other requests. The per-collection
mutex is held only inside the blocking task, never across `.await`.
