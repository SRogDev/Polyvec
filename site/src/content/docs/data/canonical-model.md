---
title: Canonical model, provenance & versioning
description: The document model every retrieval path reads — versions, hashes, model tags, provenance.
---

Every retrieval path — vector, lexical, hybrid, relationship-expand —
reads the same canonical document model. There is one representation of a
document; the indexes are derived views over it.

## The model

A **document** is identified by `{collection}#{sha256(source)}` —
server-derived from the `source` you supply, so re-ingesting the same
source upserts the same document (idempotent re-ingestion key).

Each document carries:

- **Original + canonical SHA-256 hashes.** The original hash covers the
  source bytes; the canonical hash covers the deterministically normalized
  Markdown. Change detection compares the canonical hash: unchanged →
  `skipped` (no new version, no new vectors); changed → new version.
- **Version** (`v1`, `v2`, …). A re-ingest with changed content bumps the
  version, tombstones the old version's vectors immediately (stale evidence
  is unsearchable at once), and mints new chunk ids.
- **Provenance metadata**: source URI, filename, format, title, conversion
  metadata, plus your `metadata` (flat string/number/boolean values only —
  nested objects are `400`).
- **Chunks** with deterministic ids `{doc}#v{version}#c{index:06}`,
  section-aware (ATX headings; sections are atomic; oversized sections
  hard-split at `target_chars` with `overlap_chars` overlap).

## The model tag (one model per collection)

The embedding model (`name` + `version`) is **stamped once** on the
collection — whether it comes from the `remote` provider config or the
`model` field of a direct collection create. A later ingest or query with
a different model is refused, never silently mixed. **Changing embedding
models means a new collection** — the explicit migration policy. This is
deliberate: mixing embedding spaces in one index corrupts distance
semantics silently.

## What this buys you

- **Reproducibility**: the canonical hash makes "did this document change?"
  a pure function of bytes — no timestamps, no heuristics.
- **No ghost results**: version bumps tombstone old vectors before the new
  ones land; lexical deletes scrub in the same job; every query joins hit
  text back from the catalog, so a hit can never be half-built.
- **Auditability**: `source` + version + hashes + `created_at` (relations)
  give every retrieved chunk a paper trail back to the bytes it came from.
