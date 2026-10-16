---
title: Configuration & env vars
description: Every environment variable Polyvec reads, its default, and what it means.
---

Polyvec is configured entirely through environment variables — no config
files, no CLI flags. Every variable below was verified against
`crates/polyvec-server/src/config.rs` (and the ingestion/embeddings
crates); if the code and this table disagree, the code wins and the table
is a bug — please report it.

## Server

| Variable | Default | Meaning |
| -------- | ------- | ------- |
| `POLYVEC_BIND` | `127.0.0.1:8080` | `IP:port` to listen on (`localhost` accepted as alias for `127.0.0.1`) |
| `POLYVEC_DATA_DIR` | `./polyvec-data` | One sub-directory per collection; created on startup |
| `POLYVEC_JSON_LIMIT_BYTES` | `16777216` (16 MiB) | Max JSON body; larger → `413` |
| `POLYVEC_WORKERS` | (CPU count) | HTTP worker threads |
| `POLYVEC_ALLOW_NONLOCAL_BIND` | unset | Set to `1` to allow a non-loopback bind |
| `POLYVEC_API_KEY` | unset | Bearer token required on every `/v1` route when set (auth off by default) |
| `POLYVEC_RATE_LIMIT_RPS` | `1000` | Per-IP token bucket on `/v1` routes (burst 4×); `0` disables |

Binding a non-loopback address without `POLYVEC_ALLOW_NONLOCAL_BIND=1`
refuses to start (exit 2 with a refusal message). Without
`POLYVEC_API_KEY`, anyone who can reach the socket has full
read/write/delete — set the key, or keep the server on localhost behind a
reverse proxy / firewall you trust. See [Security](/operations/security/).

## Embedding providers

| Variable | Default | Meaning |
| -------- | ------- | ------- |
| `POLYVEC_EMBEDDING_PROVIDER` | `none` | `none` \| `stub` (dev/test only) \| `remote` (OpenAI-compatible) |
| `POLYVEC_STUB_DIMENSION` | `64` | Stub provider dimension (**must match the collection**) |
| `POLYVEC_EMBEDDING_BASE_URL` | — | Remote endpoint base URL (required for `remote`) |
| `POLYVEC_EMBEDDING_API_KEY` | — | Remote API key (**env only, never logged**, never in errors) |
| `POLYVEC_EMBEDDING_MODEL` | — | Remote model name (required; stamped as the collection's model tag) |
| `POLYVEC_EMBEDDING_MODEL_VERSION` | `1` | Remote model version (part of the model tag) |
| `POLYVEC_EMBEDDING_DIMENSION` | — | Expected embedding dimension (required) |
| `POLYVEC_EMBEDDING_BATCH_SIZE` | `64` | Texts per HTTP call |
| `POLYVEC_EMBEDDING_TIMEOUT_SECS` | `30` | Per-batch timeout |

Notes:

- With `none` (the default), text ingest and text query return `501
  unsupported` — supply `vectors` explicitly, or configure a provider.
- The `stub` provider is a deterministic hash for dev/test. Retrieval
  behavior with the stub measures pipeline mechanics, never semantic
  quality.
- The remote provider uses rustls with the platform's webpki roots. Behind
  a TLS-intercepting corporate proxy, set `SSL_CERT_FILE` to the proxy CA
  bundle.

## Ingestion jobs

| Variable | Default | Meaning |
| -------- | ------- | ------- |
| `POLYVEC_JOB_QUEUE_CAPACITY` | `64` | Submit queue bound (`429` past this) |
| `POLYVEC_JOB_CONCURRENCY` | `4` | Jobs running concurrently |
| `POLYVEC_JOB_MAX_ATTEMPTS` | `5` | Attempt budget per job |

Retries use exponential backoff (1s · 2ⁿ, capped at 30s); a 429
`Retry-After` from the provider is honored (capped at 60s).

## Collection config (per collection, at creation)

Not env vars — set in the `POST /v1/collections` body and fixed for the
collection's lifetime:

- `dimension` (1–4096, required), `metric` (`"l2"` | `"cosine"`, required)
- `model` (`{"name","version"}`) — stamped once; a different model is
  refused later, never silently mixed
- `hnsw` (`{"m","ef_construction","ef_search","max_elements"}`) — index
  tuning; `ef` on search is 8–10000, `k` is 1–1000
