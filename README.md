# Polyvec

A company knowledge-base vector database, built from scratch in Rust.

## Thesis

Agents should be able to query a company's entire knowledge base constantly —
thousands of reads per day per agent — at negligible cost. Existing vector DBs
optimize for the general case; Polyvec optimizes for one: **massive, cheap,
high-recall reads over company knowledge**, as the knowledge layer of
[Polygrow](https://github.com/SRogDev) and future businesses.

## Status

Research phase. See `docs/research/` for the landscape survey before any code.

## Non-goals (for now)

- Being a general-purpose Pinecone/Qdrant replacement.
- The memory layer (episodic/conversational memory stays separate — e.g. Supermemory).
