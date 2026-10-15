# ADR-004 — Storage commit protocol and consistency model

> Status: accepted 2026-10-15 (Phase 2).

## Context

Polyvec persists each collection in two different substrates:

* **redb** (`catalog.redb`) — collection catalog, documents, chunks, vector
  records, tombstone flags, generation counters. Transactional.
* **hnsw_rs dump files** (`index.hnsw.graph` + `index.hnsw.data`) plus a
  Polyvec-owned `sidecar.json` (format version, SHA-256 checksums,
  generation, counts).

There is no atomic commit across two separate files on a POSIX filesystem
without a write-ahead log, and Phase 2 deliberately does not build one.
This ADR records the protocol we *do* implement and the consistency model
it yields — honestly, without promising atomicity we don't have.

## Decision

### Commit protocol

1. **redb transaction commits first.** Every vector write
   (insert/update/delete) is a redb write transaction covering the vector
   record, the tombstone flag, and the `generation` counter. When it
   commits, the write is durable — even if the process dies a nanosecond
   later. **redb is the source of truth; the ANN dump is a rebuildable
   cache.**
2. **The in-memory index mutates second.** Crash between 1 and 2 → the next
   open detects a stale dump (sidecar generation < catalog generation) and
   rebuilds the index from redb.
3. **`flush()` checkpoints.** The graph is dumped to *temp* files
   (`index.tmp.hnsw.*`), renamed into place, checksummed into the sidecar
   (temp file + atomic rename), and only then is `synced_generation`
   advanced in redb.

### Recovery on open

| Observation | Action |
|---|---|
| Sidecar or dump files missing, or dump older than catalog | Rebuild index from redb, checkpoint → `Recovery::RebuiltFromCatalog` |
| Dump current but `synced_generation` lags (crash inside `flush`) | Complete the mark-synced transaction → `Recovery::CompletedFlush` |
| Sidecar format/dimension/metric mismatch | `Error::Corrupt` |
| SHA-256 checksum mismatch | `Error::Corrupt` with a recovery hint (delete dump files + sidecar, reopen to rebuild from catalog) |
| Dump generation newer than catalog | `Error::Corrupt` (impossible through this API) |

### Why corrupt dumps are an error, not a silent rebuild

A checksum mismatch means the files are damaged *or belong to something
else*. The catalog could rebuild them safely (it is the source of truth),
but silently discarding evidence of disk corruption hides a potentially
sick disk/operator error. So: stop with a clear error naming the failed
check and the exact recovery procedure. Corrupt input never reaches
hnsw_rs's loader (which asserts on magic/dimension — a panic) because the
sidecar checksum gate runs first.

### Deletion model

hnsw_rs has no delete API. Polyvec owns a tombstone set:

* The tombstone flag commits in the same redb transaction as the delete,
  so **deletes hide from search immediately** (post-filter), before any
  flush.
* Updates are *tombstone old node + insert new node* under a fresh node
  id; the `VectorId` stays stable.
* `rebuild()` (and `flush()` past a 1/5 tombstone-to-live ratio with ≥16
  tombstones) compacts: a fresh graph is built from live vectors only.

### Consequences

* Reads are always consistent with the catalog; the ANN index may lag the
  catalog between writes and `flush()`, but search results are filtered
  through the in-memory tombstone mirror, so deletes are never visible
  after their redb commit.
* `flush()` is the durability point for the *index*; vector data is
  durable at redb-commit time.
* The residual hole: a crash between the dump-file rename and the sidecar
  rename leaves new files + old sidecar → checksum mismatch → `Error::Corrupt`
  with recovery instructions. Documented here instead of hidden.
* `Box::leak` of one `HnswIo` per collection open (see `polyvec-index`
  docs): hnsw_rs's `load_hnsw` ties the loaded index's lifetime to the
  `&mut HnswIo` handle; leaking the small handle struct is the pragmatic
  alternative to a self-referential-struct dependency. No `unsafe`.

## Alternatives considered

* **WAL across both substrates** — correct but disproportionate for Phase 2;
  the rebuild-from-catalog slow path already gives us crash safety.
* **Silent rebuild on checksum mismatch** — rejected: hides corruption
  evidence (see above).
* **Single-file storage for vectors too** — rejected: vectors live in the
  ANN layer for read performance (per the Phase 1 audit's "best tool per
  job" rationale); redb keeps a copy as the rebuild source.
