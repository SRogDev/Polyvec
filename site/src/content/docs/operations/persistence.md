---
title: Persistence, backups & recovery
description: The commit protocol, on-disk layout, backup procedure, and recovery guarantees.
---

## Commit protocol (ADR-004)

Every write commits to **redb first** — redb is the source of truth and
is durable immediately, no flush needed. The HNSW index dump is a
checksummed, rebuildable cache — never the authority. There is no fake
cross-file atomicity; the protocol is documented honestly in
`docs/decisions/ADR-004-storage-commit-protocol.md`.

## On-disk layout

One directory per collection under `POLYVEC_DATA_DIR`:

```text
polyvec-data/
├── kb-main/
│   ├── catalog.redb          # documents, chunks, vectors, relations — source of truth
│   ├── index.hnsw.graph      # ANN index dump (rebuildable cache)
│   ├── index.hnsw.data
│   ├── sidecar.json           # SHA-256 checksums + generation
│   └── lexical/              # tantivy index (derived, disposable)
└── jobs.redb                 # ingestion job records
```

Search sees writes at once (in-memory index); deletes hide immediately.

## Backups

1. `POST /v1/collections/{id}/flush` — checkpoints the index to disk on
   demand.
2. Copy the collection directory (and `jobs.redb` if you care about job
   history). A plain filesystem copy is sufficient: redb files are
   consistent at rest once no writer holds them — stop the server or flush
   first.

## Recovery

On open, the server validates the index dump against the sidecar
checksums and generation:

- Dump missing, stale, or after a `kill -9`: the index **rebuilds from
  redb** (`RebuiltFromCatalog`) — nothing committed is lost.
- Checksum/version mismatch: `Error::Corrupt` — a clear error with
  recovery instructions, never a panic, never silent corruption.

Graceful shutdown (SIGTERM/SIGINT) finishes in-flight requests, then
flushes every open collection before exiting. The lexical index rebuilds
from the catalog on every collection open — a stale or corrupt tantivy
directory can never serve ghost results.

## Rebuilds

Explicit rebuild compacts tombstones: deletes and old versions hide from
search immediately via the tombstone set, and a rebuild reclaims the
space. Rebuilds also happen automatically during `flush` when tombstones
exceed 1/5 of live vectors (minimum 16).

## Known limits

- `kill -9` recovery over HTTP is untested; restart recovery is covered at
  the storage layer and graceful-shutdown recovery over HTTP is measured
  (see [Performance](/operations/performance/)).
- Vectors are stored as JSON in redb — not space-efficient; future work.
