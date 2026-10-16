---
title: Ingestion pipeline & formats
description: How source bytes become searchable vectors — conversion, canonicalization, chunking, jobs, cost guards.
---

How source bytes become searchable vectors: conversion → canonicalization →
change detection → chunking → embedding → atomic store write, executed as
persisted jobs with bounded retries, backpressure, and idempotency.

## Data flow

```text
POST /v1/collections/{id}/ingest {source, content.{text|base64}, metadata?, vectors?, idempotency_key?}
  │ 202 {job_id, state: "pending"}          (404 unknown collection,
  │                                          429 + Retry-After when the queue is full,
  │                                          501 when text needs a provider and none is configured)
  ▼
JobRunner (bounded mpsc queue, semaphore-bounded concurrency)
  │ JobStore (jobs.redb): pending → running → done | failed (+ retrying)
  ▼  (on Tokio's blocking pool)
1. Convert      markit (vendored) → Markdown. Polyvec feeds bytes directly
                (markit's HTTP fetch is stubbed out in the vendored build).
2. Canonicalize deterministic Markdown normalization (no LLM rewriting).
                Produces original + canonical SHA-256 hashes.
3. Change detection  unchanged canonical hash + same chunking version →
                skip (no new version, no new vectors). A `vectors_present`
                guard finishes partially-completed attempts instead of
                reporting them skipped.
4. Chunk        section-aware (ATX headings); sections are atomic; oversized
                sections hard-split at `target_chars` with `overlap_chars`
                overlap. Chunk ids: `{doc}#v{version}#c{index:06}`.
5. Embed        supplied vectors (1:1 with chunks, no provider needed) or the
                configured provider (batched). Dimension must equal the
                collection dimension; the collection's model tag is stamped once.
6. Store        atomic write: old version tombstoned, stale evidence
                unsearchable immediately.
```

Job status: `GET /v1/jobs/{job_id}`, `GET /v1/collections/{id}/jobs?limit=`.
Text query: `POST /v1/collections/{id}/query` — retrieval modes (`vector`
default, `lexical`, `hybrid`); `501` when a vector-side mode has no
provider configured.

## Supported formats (tested)

| Input | Result |
| ----- | ------ |
| Markdown (`.md`) | Passes through; ATX headings drive section chunking |
| HTML (`.html`) | Converted to Markdown (headings, code blocks, links preserved) |
| Plain text (`.txt`) | Passes through as Markdown |
| CSV (`.csv`) | Converted to a Markdown table (header + rows) |

Empty input is rejected. Format detection uses the filename extension /
MIME hint — markit's internal dispatch is not probed. The vendored markit
is pinned (`4197c76a`) with its HTTP fetcher stubbed: **Polyvec never
fetches URLs** — `source` is an opaque label and you supply the bytes.

## cost-guard mechanisms

- **Batching**: the remote provider groups texts into batches (default 64,
  `POLYVEC_EMBEDDING_BATCH_SIZE`) — one HTTP call per batch, never
  one-call-per-text. Empty input makes no calls.
- **Error classification before retrying**: 429/5xx/timeout → transient
  (retried); 4xx/auth/dimension-mismatch/malformed → permanent (never
  retried). The job record keeps the classification and the attempt that
  produced it.
- **Bounded retries + budgets**: at most `POLYVEC_JOB_MAX_ATTEMPTS`
  (default 5) attempts per job, exponential backoff (1s · 2ⁿ, capped at
  30s). A 429 `Retry-After` header is honored (capped at 60s). No infinite
  retries, no retry storms.
- **Backpressure**: the submit queue is bounded
  (`POLYVEC_JOB_QUEUE_CAPACITY`, default 64). Submits past capacity get
  `429` with a `Retry-After` header — never an unbounded queue, never a
  silent drop.
- **Idempotency**: submissions carry an idempotency key (derived from the
  content when absent). A duplicate key while a job is in flight returns the
  existing job instead of queueing a duplicate. Terminal jobs may be
  resubmitted — unchanged content is a side-effect-free no-op via the
  content-hash check.

## Failure semantics

- A failed job stays failed: the error kind (`transient`/`permanent`), the
  message, and the attempt are persisted on the job record. Partial
  progress is never reported as complete.
- In-flight jobs (`running`/`retrying`) found at startup are marked failed
  with "resubmit" — their input payload lives only in memory, so they
  cannot be resumed; resubmission is idempotent.
- Provider outages surface as `502 provider_error` (query path) or failed
  jobs with transient errors (ingest path). The API key is never in the
  error detail.

## Known limitations

- Two ingests of the same source running concurrently: last-writer-wins;
  the loser's vector insert fails loudly (no silent corruption).
- Job listing is a linear scan (fine for an operator-facing endpoint).
