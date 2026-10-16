---
title: Embedding providers
description: none, stub, and remote (OpenAI-compatible) providers — how embeddings reach Polyvec.
---

Polyvec needs vectors from somewhere. Three ways in, configured by
`POLYVEC_EMBEDDING_PROVIDER` (`none` by default):

| Provider | Value | Embeds text | Use for |
| -------- | ----- | ----------- | ------- |
| none | `none` | never — text paths return `501` | bring-your-own vectors |
| stub | `stub` | deterministic hash | **local dev / tests only** |
| remote | `remote` | OpenAI-compatible `/embeddings` HTTP API | production documents |

## Supplied vectors (no provider needed)

With `none`, pass `vectors` alongside the content — 1:1 with chunks:

```bash
curl -X POST localhost:8080/v1/collections/kb/ingest \
  -H 'content-type: application/json' \
  -d '{"source":"kb://notes/1","content":{"text":"# Hello\n\nworld"},
       "vectors":[[0.1,0.2,...],[0.3,0.4,...]]}'
```

Each vector must match the collection dimension and be finite. This is the
pattern for apps that embed text themselves (their own model, a framework,
etc.) — Polyvec never sees the model.

## Stub provider (dev/test only)

```bash
POLYVEC_EMBEDDING_PROVIDER=stub POLYVEC_STUB_DIMENSION=64 polyvec-server
```

Deterministic: identical text → identical vector. `POLYVEC_STUB_DIMENSION`
**must match the collection dimension**. The stub is a hash function, not
an embedding model — retrieval behavior with the stub proves the pipeline
works; it says nothing about semantic search quality. Never use it for
production data.

## Remote provider (OpenAI-compatible)

```bash
POLYVEC_EMBEDDING_PROVIDER=remote \
POLYVEC_EMBEDDING_BASE_URL="https://api.example.com/v1" \
POLYVEC_EMBEDDING_MODEL="text-embedding-3-small" \
POLYVEC_EMBEDDING_MODEL_VERSION="1" \
POLYVEC_EMBEDDING_DIMENSION=1536 \
POLYVEC_EMBEDDING_API_KEY="$EMBEDDING_API_KEY" \
polyvec-server
```

- The key comes from env only (`POLYVEC_EMBEDDING_API_KEY`) — it is never
  logged and never appears in error responses.
- Texts are batched (`POLYVEC_EMBEDDING_BATCH_SIZE`, default 64 — one HTTP
  call per batch, never one-call-per-text); per-batch timeout
  `POLYVEC_EMBEDDING_TIMEOUT_SECS` (default 30).
- Failures are classified **before** retrying: 429/5xx/timeout →
  transient (retried); 4xx/auth/dimension-mismatch/malformed → permanent
  (never retried). The job record keeps the classification and the attempt
  that produced it.
- Uses rustls with the platform's webpki roots. Behind a TLS-intercepting
  corporate proxy, set `SSL_CERT_FILE` to the proxy CA bundle.

## The model tag (one model per collection)

The configured model (`name` + `version`) is **stamped once** on the
collection. Ingesting or querying with a different model is refused —
models are never silently mixed in one index. Changing models means a new
collection (the explicit migration policy; see
[Canonical model](/data/canonical-model/)).
