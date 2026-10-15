# Polyvec

A company knowledge-base vector database, built from scratch in Rust — optimized for **massive, cheap, high-recall reads by agents**.

## Thesis

Agents should be able to query a company's entire knowledge base constantly — thousands of reads per day per agent — at negligible cost. Existing vector DBs optimize for the general case; Polyvec optimizes for one: **massive, cheap, high-recall reads over company knowledge**, as the knowledge layer of Polygrow and future businesses.

## What it is not (for now)

- A general-purpose Pinecone/Qdrant replacement.
- The memory layer (episodic/conversational memory stays separate).

## Status

**Implementation plan: Phases 0–9** (definitive plan, 23 sections — research
phase is done, execution is running):

| Phase | Scope | Status |
| ----- | ----- | ------ |
| 0 | Repository & architecture foundation | in progress |
| 1 | Crate audit (index / storage / HTTP / embedding candidates) | next |
| 2 | Vector index (HNSW et al., bounded graph retrieval) | planned |
| 3 | Durable segment storage + manifest + WAL | planned |
| 4 | Embedding provider adapters (pluggable) | planned |
| 5 | Actix Web HTTP API | planned |
| 6 | Server binary (wiring, config, graceful shutdown) | planned |
| 7 | Benchmark harnesses + first measured results | planned |
| 8 | Astro Starlight docs site | planned |
| 9 | MVP acceptance: self-hosted engine, measured read path | planned |

Progress is tracked in `STATUS.md`; each phase ships as its own PR.

## Layout

```text
crates/polyvec-core/   # domain types, IDs, config contracts, shared Error
docs/decisions/        # architecture decision records (ADRs)
docs/audit/            # crate & license audit reports
```

`deny.toml` enforces the dependency policy (MIT/Apache-2.0 only — see
`CONTRIBUTING.md`); `cargo audit` runs in CI.

## License

Dual-licensed: MIT (`LICENSE-MIT`) OR Apache-2.0 (`LICENSE-APACHE`).
