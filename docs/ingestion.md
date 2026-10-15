# Ingestion pipeline (Phase 4)

How source bytes become searchable vectors: conversion → canonicalization →
change detection → chunking → embedding → atomic store write, executed as
persisted jobs with bounded retries, backpressure, and idempotency.

## Data flow

```text
POST /v1/collections/{id}/ingest {source, filename?, mime?, content.{text|base64},
                                   metadata?, vectors?, idempotency_key?}
  │ 202 {job_id, state: "pending"}          (404 unknown collection,
  │                                          429 + Retry-After when the queue is full,
  │                                          501 when text needs a provider and none is configured)
  ▼
JobRunner (bounded mpsc queue, semaphore-bounded concurrency)
  │ JobStore (jobs.redb): pending → running → done | failed (+ retrying)
  ▼  (on Tokio's blocking pool)
1. Convert      markit (vendored) → Markdown. Trait `Converter` keeps the
                backend replaceable; Polyvec feeds bytes directly (markit's
                HTTP fetch is stubbed out in the vendored build).
2. Canonicalize deterministic Markdown normalization (no LLM rewriting).
                Produces original + canonical SHA-256 hashes and preserves
                source URI, filename, format, title, conversion metadata.
3. Change detection  unchanged canonical hash + same chunking version →
                skip (no new version, no new vectors). A `vectors_present`
                guard catches partially-completed attempts (document stored,
                vectors missing) and finishes them at the same version.
4. Chunk        section-aware (ATX headings); sections are atomic; oversized
                sections hard-split at `target_chars` with `overlap_chars`
                overlap. Chunk ids: `{doc}#v{version}#c{index:06}`.
5. Embed        supplied vectors (1:1 with chunks, no provider needed) or the
                configured provider (batched). Dimension must equal the
                collection dimension; the collection's model tag is stamped
                once — a different model is refused, never silently mixed.
6. Store        `ingest_document`: tombstones the previous version's vectors,
                removes its chunks, stores the new doc/chunks/vectors
                atomically. Stale evidence is unsearchable immediately.
```

Job status: `GET /v1/jobs/{job_id}`, `GET /v1/collections/{id}/jobs?limit=`.
Text query: `POST /v1/collections/{id}/query {text, k, ef?, mode?, filter?, rrf_k?}`
— retrieval modes (`vector` default, `lexical`, `hybrid`); see
`docs/retrieval.md`. `501` when a vector-side mode has no provider
configured.

## Supported formats (tested)

| Input | Result |
|---|---|
| Markdown (`.md`) | Passes through; ATX headings drive section chunking |
| HTML (`.html`) | Converted to Markdown (headings, code blocks, links preserved) |
| Plain text (`.txt`) | Passes through as Markdown |
| CSV (`.csv`) | Converted to a Markdown table (header + rows) |

Empty input is rejected. Format detection uses the filename extension /
MIME hint — markit's internal dispatch is not probed.

## cost-guard mechanisms

- **Batching**: the remote provider groups texts into batches (default 64,
  `POLYVEC_EMBEDDING_BATCH_SIZE`) — one HTTP call per batch, never
  one-call-per-text. Empty input makes no calls.
- **Error classification before retrying**: 429/5xx/timeout → transient
  (retried); 4xx/auth/dimension-mismatch/malformed → permanent (never
  retried). The job record keeps the classification and the attempt that
  produced it.
- **Bounded retries + budgets**: at most `POLYVEC_JOB_MAX_ATTEMPTS`
  (default 5) attempts per job, exponential backoff (1s · 2ⁿ, capped at 30s).
  A 429 `Retry-After` header is honored (capped at 60s) instead of
  hammering the provider. No infinite retries, no retry storms.
- **Backpressure**: the submit queue is bounded (`POLYVEC_JOB_QUEUE_CAPACITY`,
  default 64). Submits past capacity get `429` with a `Retry-After` header —
  never an unbounded queue, never a silent drop.
- **Idempotency**: submissions carry an idempotency key (derived from the
  content when absent). A duplicate key while a job is in flight returns the
  existing job instead of queueing a duplicate. Terminal jobs may be
  resubmitted — unchanged content is a side-effect-free no-op via the
  content-hash check.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `POLYVEC_EMBEDDING_PROVIDER` | `none` | `none` \| `stub` (deterministic, dev/test only — **not production**) \| `remote` (OpenAI-compatible `/embeddings`) |
| `POLYVEC_STUB_DIMENSION` | `64` | Stub provider dimension (must match the collection) |
| `POLYVEC_EMBEDDING_BASE_URL` | — | Remote endpoint base URL (required for `remote`) |
| `POLYVEC_EMBEDDING_API_KEY` | — | Remote API key (env only, never logged) |
| `POLYVEC_EMBEDDING_MODEL` | — | Remote model name (required; stamped as the collection's model tag) |
| `POLYVEC_EMBEDDING_MODEL_VERSION` | `1` | Remote model version (part of the model tag) |
| `POLYVEC_EMBEDDING_DIMENSION` | — | Expected embedding dimension (required) |
| `POLYVEC_EMBEDDING_BATCH_SIZE` | `64` | Texts per HTTP call |
| `POLYVEC_EMBEDDING_TIMEOUT_SECS` | `30` | Per-batch timeout |
| `POLYVEC_JOB_QUEUE_CAPACITY` | `64` | Submit queue bound (429 past this) |
| `POLYVEC_JOB_CONCURRENCY` | `4` | Jobs running concurrently |
| `POLYVEC_JOB_MAX_ATTEMPTS` | `5` | Attempt budget per job |

Note: the remote provider uses rustls with the platform's webpki roots. If
the endpoint sits behind a TLS-intercepting corporate proxy, set
`SSL_CERT_FILE` to the proxy CA bundle.

## Failure semantics

- A failed job stays failed: the error kind (`transient`/`permanent`), the
  message, and the attempt are persisted on the job record. Partial progress
  is never reported as complete (see the `vectors_present` guard above).
- In-flight jobs (`running`/`retrying`) found at startup are marked failed
  with "resubmit" — their input payload lives only in memory, so they cannot
  be resumed; resubmission is idempotent.
- Provider outages surface as `502 provider_error` (query path) or failed
  jobs with transient errors (ingest path). The API key is never in the
  error detail.

## Known limitations

- Two ingests of the same source running concurrently: last-writer-wins;
  the loser's vector insert fails loudly (no silent corruption).
- Job listing is a linear scan (fine for an operator-facing endpoint).
