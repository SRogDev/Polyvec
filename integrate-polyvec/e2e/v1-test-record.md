# v1 live test record — 2026-10-15

Prompt version tested: **v1**. Tester followed the prompt exactly
(Steps 1–6), acting as the integrating agent on a throwaway project.

## Step 1 — Inspect the target project (findings, stated before continuing)

- Target: throwaway project `/tmp/pv-e2e-test` — Python 3.12, stdlib only
  (`urllib` + `json`), no framework, no secret manager; secrets via env vars.
- Polyvec fits as a sidecar process over HTTP
  (`127.0.0.1:8080`); there is no client SDK — HTTP is the only interface.
- Localhost bind keeps the no-auth default safe; no non-loopback bind, so
  `POLYVEC_API_KEY` is not set in this run.

## Step 2 — Integration pattern chosen

**D (ingest pipeline + stub provider)** for the vector side, plus a
lexical-mode query (needs no provider). Reason: no embedding API keys
exist in this environment; the stub exercises the full pipeline
(convert → canonicalize → chunk → embed → store → query) while its
mechanics-only nature is recorded as a limit. Pattern A (remote provider)
was explicitly not chosen because it cannot be verified without a key.

## Step 3 — Configure the endpoint

Server: `polyvec-server` debug binary built from this branch's HEAD
(`cargo build -p polyvec-server` clean, 22.9 s).

```bash
cd /tmp/pv-e2e-test
POLYVEC_BIND=127.0.0.1:8080 \
POLYVEC_DATA_DIR=./polyvec-data \
POLYVEC_EMBEDDING_PROVIDER=stub \
POLYVEC_STUB_DIMENSION=64 \
/home/hatch/workspace/polyvec/target/debug/polyvec-server
```

```bash
$ curl -s 127.0.0.1:8080/healthz
{"status":"ok","service":"polyvec","version":"0.1.0"}
$ curl -s 127.0.0.1:8080/readyz
{"status":"ready"}
```

Both probes passed before any `/v1` route was touched.

## Step 4 — Collection + test document

```bash
$ curl -X POST 127.0.0.1:8080/v1/collections \
    -H 'content-type: application/json' \
    -d '{"id":"pv-e2e","dimension":64,"metric":"cosine"}'
# → 201, {"id":"pv-e2e",...}
```

Ingested a real markdown document (`demo_doc.md`: "Zephyr Onboarding
Notes" — deployment pipeline, rollback procedure, contacts) via
`POST /v1/collections/pv-e2e/ingest` with
`metadata: {"team":"nimbus","doc":"zephyr-onboarding"}`.

```bash
# → 202 {"job_id":"job-00000001","state":"pending"}
$ curl -s 127.0.0.1:8080/v1/jobs/job-00000001
# → {"job_id":"job-00000001","state":"done","attempts":1,
#    "affected":{"document_id":"pv-e2e#54177b09d8e923e510b6ad4a",
#                "version":1,"chunks":3,"vectors":3,"skipped":false},
#    "error":null}
```

Job terminal on first attempt, 3 chunks, 3 vectors.

## Step 5 — Real queries and evidence verification

```bash
$ curl -X POST 127.0.0.1:8080/v1/collections/pv-e2e/query \
    -H 'content-type: application/json' \
    -d '{"text":"zephyr rollback procedure","k":5,"mode":"lexical"}'
```

Verified in the actual JSON response:

- 2 hits; rank 1 is the ingested document
  (`source == "pv-e2e://onboarding/zephyr-notes"`);
- `score_kind == "bm25"` (matches the requested mode);
- hit `text` contains the document's rollback content;
- hit `metadata.team == "nimbus"` (supplied at ingest).

Vector mode (`"mode":"vector"`, stub provider): 200, hits returned,
`score_kind == "distance"` — pipeline works end to end.

Negative check (`"text":"xylophone quantum banana"`, lexical):
**0 hits** — no fabricated rank-1 evidence.

## Step 6 — Error handling + tests

The client (`/tmp/pv-e2e-test/test_client.py`, stdlib only) wraps calls in
a typed `PolyvecError`: timeouts/connection failures surface as
`connection_error` (not a bare traceback); `429` is retried up to 3 times
honoring `Retry-After`; `401`/`404`/`501` are surfaced with the server's
`code`. A `GET /v1/collections/nope` correctly raised
`HTTP 404 [not_found]` and was asserted as a test.

Full run output (`python3 test_client.py`):

```text
[PASS] healthz 200 — {"status": "ok", "service": "polyvec", "version": "0.1.0"}
[PASS] readyz 200 — {"status": "ready"}
[PASS] create collection 201 — id=pv-e2e
[PASS] ingest 202 — job_id=job-00000001
[PASS] job done — state=done attempts=1
[PASS] job affected chunks>0 — document_id=pv-e2e#54177b09d8e923e510b6ad4a chunks=3
[PASS] lexical query 200 + hits — 2 hits
[PASS] rank1 is our document — rank=1 score_kind=bm25
[PASS] score_kind == bm25
[PASS] hit carries text
[PASS] hit carries metadata
[PASS] vector query 200 — score_kind=distance
[PASS] vector score_kind == distance
[INFO] negative query returned 0 hits; top source=None
[PASS] negative query does not fabricate rank-1 evidence
[PASS] 404 surfaced as PolyvecError — HTTP 404 [not_found]: not found: collection catalog at ./polyvec-data/nope

ALL CHECKS PASSED — prompt v1 steps 1-6 executed for real.
```

## Unverified assumptions (per the prompt's report contract)

- Pattern A (remote OpenAI-compatible provider) was **not** exercised — no
  API key exists in this environment. The 502-on-provider-failure path is
  covered by repo tests, not by this run.
- Auth (`POLYVEC_API_KEY`) and rate limiting were **not** exercised — the
  run stayed on localhost with the default posture (both are covered by
  repo tests).
- Retrieval *quality* is not claimed: the stub provider is a hash function,
  so "rank 1 is our document" proves pipeline mechanics, not semantic
  search quality.
- Restart durability and `kill -9` recovery were **not** re-verified in
  this run (covered by repo tests / `docs/e2e.sh`).
