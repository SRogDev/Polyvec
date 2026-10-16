---
title: Quickstart
description: From zero to a useful query in five minutes, using only the docs.
---

This page is the acceptance test for the docs: everything here runs
against a stock build with no API keys and no extra services. The stub
embedding provider exercises the whole pipeline; treat its retrieval
behavior as mechanics only (see the note at the end).

## 1. Start the server

```bash
POLYVEC_EMBEDDING_PROVIDER=stub \
POLYVEC_STUB_DIMENSION=64 \
./target/release/polyvec-server
```

Keep this terminal running. In another terminal, confirm it's alive:

```bash
curl localhost:8080/healthz
```

## 2. Create a collection

```bash
curl -X POST localhost:8080/v1/collections \
  -H 'content-type: application/json' \
  -d '{"id":"handbook","dimension":64,"metric":"cosine"}'
# → 201
```

`dimension` must equal the embedding dimension (`64` for the stub default);
`metric` is `"l2"` or `"cosine"`. Collection ids are slugs (`[a-z0-9-_]`,
1–64 chars).

## 3. Ingest a document

```bash
curl -X POST localhost:8080/v1/collections/handbook/ingest \
  -H 'content-type: application/json' \
  -d '{"source":"quickstart://vacation-policy",
       "filename":"vacation.md",
       "content":{"text":"# Vacation policy\n\nEmployees get 25 days of paid vacation per year. Unused days roll over up to 5 days.\n\n## Requesting time off\n\nSubmit requests two weeks in advance through the HR portal."},
       "metadata":{"team":"hr"}}'
# → 202 {"job_id":"job-00000001","state":"pending"}
```

Ingestion is a background job. Poll until it is `done`:

```bash
curl localhost:8080/v1/jobs/job-00000001
# → {"job_id":"...","state":"done","attempts":1,
#    "affected":{"document_id":"handbook#...","version":1,"chunks":2,"vectors":2,"skipped":false},
#    "error":null}
```

If the job is `failed`, read its `error` object — never proceed on a
failed job.

## 4. Run a useful query

Lexical search needs no embedding provider:

```bash
curl -X POST localhost:8080/v1/collections/handbook/query \
  -H 'content-type: application/json' \
  -d '{"text":"how many vacation days roll over","k":5,"mode":"lexical"}'
```

You get back `hits` with `rank`, `score` (`score_kind: "bm25"`), the
chunk `text`, `source`, and your `metadata`. Vector search works too —
the stub provider embeds the query:

```bash
curl -X POST localhost:8080/v1/collections/handbook/query \
  -H 'content-type: application/json' \
  -d '{"text":"paid time off policy","k":5,"mode":"vector"}'
```

## What you just proved

- The server runs, the collection stores vectors durably (restart it and
  query again — the data survives; see [Persistence](/operations/persistence/)).
- The ingest pipeline converts → canonicalizes → chunks → embeds → stores,
  and the query path returns evidenced hits.

## Honest note about the stub provider

`POLYVEC_EMBEDDING_PROVIDER=stub` is a **deterministic hash, not an
embedding model** — for local development and tests only. Retrieval
behavior with the stub proves the pipeline works; it says nothing about
semantic search quality. For real documents use a `remote`
(OpenAI-compatible) provider or supply your own vectors — see
[Embedding providers](/data/embedding-providers/).

Next: [First collection, first query](/first-query/) walks the same flow
with raw vectors and metadata filters, or jump to the full
[API reference](/api-reference/).
