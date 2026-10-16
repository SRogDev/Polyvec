---
title: Relationship-aware retrieval
description: Typed, directed relations you supply, expanded by bounded BFS with evidence on any query mode.
---

Any query mode accepts `expand`: run the base retrieval, then bounded
breadth-first traversal over relations seeded from the base hits, appending
reached nodes with evidence (`{hops, path:[{relation_type, direction,
relation_id}]}`).

```bash
curl -X POST localhost:8080/v1/collections/kb/query \
  -H 'content-type: application/json' \
  -d '{"text":"xylophone","k":5,"mode":"lexical",
       "expand":{"depth":2,"max_nodes":100,"relation_types":["references"]}}'
```

## Relation model

Relations are **explicitly supplied only** — there is no automatic
extraction and no LLM anywhere in this subsystem. Each relation is a
directed edge `source → target` between two documents/chunks, with a
stable id (caller-chosen or server-allocated `rel-{n:08}`), a type from
the fixed vocabulary, provenance metadata (`created_by` names who/what
supplied it), and a server-stamped `created_at`.

Fixed type vocabulary (new types are a versioned-vocabulary change):

| Type | Meaning |
| ---- | ------- |
| `references` | source cites, links to, or builds on the target |
| `related_to` | source is topically related to the target (weak link) |
| `supersedes` | source supersedes / replaces the target |

Consistency is **referential integrity**: creating a relation requires
both endpoints to name existing documents/chunks (`404` otherwise), and
deleting a document removes every relation touching it or its chunks, in
both directions, in the same operation as the chunk/vector cascade — there
are no dangling relations by construction, no background repair.

Relations are **immutable after creation**: to change a relation's type
or endpoints, delete it and create a new one (ids are stable, so clients
can re-create with the same id).

## Traversal bounds

| Bound | Default | Hard cap | Rationale |
| ----- | ------- | -------- | --------- |
| `depth` (hops) | 3 | 5 | evidence paths beyond a few hops stop being interpretable |
| `max_nodes` | 100 | 1000 | bounds the merged response at query scale (`k ≤ 1000`) |
| fan-out per node | 64 | — | supernode protection (a doc cited by thousands) |
| wall-clock | 2000 ms | — | backstop; in-memory BFS over redb should never get near it |

Hitting any bound stops the walk and returns the nodes found so far — a
bound is not an error. Fan-out truncation happens after sorting edges by
`(type, id)`, so it is deterministic. `relation_types` narrows the walk
to a subset of the vocabulary.

Traversal walks edges in **both directions** (a stored `A → B` is walked
forward as `outgoing` and backward as `incoming`); the evidence marks the
direction per hop. Seeds are the base hits' chunk **and** document ids;
seeds are never returned; each node is visited once (first visit wins —
the recorded path is a shortest path); cycles terminate.

## Expanded hits

Expanded hits are appended **after** the base hits (base rank order is
never disturbed) in `(hops, id)` order. A node already present as a base
hit is not re-added. Each expanded hit carries:

- `path: ["relationship"]`, `score_kind: "relationship"`, `score` = hops
  as a float (lower is better). Like every other `score_kind`, this is a
  different universe from base-hit scores.
- `evidence: {hops, path: [{relation_type, direction, relation_id}, …]}`
  — the path that led to the hit.
- Chunk nodes resolve like base chunk hits (chunk id, text, source,
  metadata); document nodes carry document-level fields (no chunk text).
  Expanded hits never carry `vector_id`/`distance` (no vector origin).
- The response echoes the effective config as `expand: {depth,
  max_nodes}`.

An endpoint that vanishes mid-query (raced a delete) is skipped, never
served half-built — the same rule as base hits.

## What it answers that vector/hybrid cannot

The committed fixture: three documents where only A mentions the query
term, with `A --references--> B --references--> C`. Lexical alone returns
A; `lexical` + `expand: {depth: 2}` returns A, B (1 hop), and C (2 hops)
with exact evidence paths. B and C are textually distant from the query —
no term-overlap or embedding similarity would surface them; only the
supplied structure does.

Honest scope note: this demonstrates *structural* retrieval value, not
semantic quality. Whether relations improve answer quality on a real
corpus is unmeasured (no production corpus exists yet) — the subsystem
gives agents the mechanism (bounded, evidenced expansion), not a
relevance claim.

## Known limitations

- No automatic relation extraction (explicit supply only).
- No graph query language; traversal is bounded BFS only.
- Chunk-level relations are **version-pinned**: re-ingesting a document
  mints new chunk ids and drops the old chunk records without migrating
  relations, so a chunk→chunk relation points at the retired version
  after re-ingest. Document-level relations are stable (document ids are
  source-derived and survive re-ingest).
