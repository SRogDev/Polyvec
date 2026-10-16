---
title: Troubleshooting
description: Common failure modes, what they mean, and how to fix them.
---

## The server refuses to start on a non-loopback bind

```
refusing non-loopback bind without POLYVEC_ALLOW_NONLOCAL_BIND=1 (exit 2)
```

Set `POLYVEC_ALLOW_NONLOCAL_BIND=1` — and set `POLYVEC_API_KEY` too,
unless you have another auth layer. See [Security](/operations/security/).

## `501 unsupported` on text ingest or query

No embedding provider is configured. Either set
`POLYVEC_EMBEDDING_PROVIDER=stub` (dev/test) or `remote` with its
required vars, or supply `vectors` explicitly with the ingest. See
[Embedding providers](/data/embedding-providers/).

## Ingest job is `failed`

```bash
curl localhost:8080/v1/jobs/job-00000001
```

Read the job's `error` object: `kind` is `transient` (429/5xx/timeout —
safe to resubmit; bounded retries already ran) or `permanent`
(4xx/auth/dimension-mismatch/malformed — fix the input or config first).
The API key is never in the error detail. In-flight jobs found at startup
are marked failed with "resubmit" — resubmission is idempotent.

## `409` on collection/vector/relation create

The id is taken. Collections: pick another slug. Vectors: the id already
exists — `PUT` to update it. Relations: ids are stable; `DELETE` then
re-create to change one.

## Query returns fewer than `k` hits with a filter

Expected: vector/hybrid apply filters post-retrieval (the ANN index has
no filter pushdown), over-fetching `min(k × 4, 1000)` candidates. If the
filter excludes most candidates, fewer than `k` survive. Lexical pushes
filters into the query and returns up to `k`. See
[Hybrid + filtering](/search/hybrid-filtering/).

## `400 validation_error` on a lexical query

The text was rejected by tantivy's query grammar. Quote phrases, escape
special characters, or use `+`/`-` prefixes deliberately — the engine
never silently rewrites your query.

## Chunk→chunk relation points at an old version

Expected and documented: re-ingesting a document mints new chunk ids
without migrating chunk-level relations ([Relationships](/data/relationships/)).
Use document-level relations when the edge must survive re-ingests.

## Data looks wrong after an upgrade

The catalog format is forward-compatible so far (`Catalog::open` creates
missing tables idempotently — no migration step). If the server reports
`corrupt` on open, the sidecar checksum or generation disagrees with the
dump: delete the `index.hnsw.*` files and restart — the index rebuilds
from redb. Never delete `catalog.redb`.

## Something else

Check the server log first (`RUST_LOG=debug` increases volume; bodies and
vector values are never logged). The repo's `docs/e2e*.sh` scripts are
runnable end-to-end checks you can run against your own build to isolate
whether the problem is your client or the server.
