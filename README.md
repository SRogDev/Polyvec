# Polyvec

A company knowledge-base vector database, built from scratch in Rust — optimized for **massive, cheap, high-recall reads by agents**.

## Thesis

Agents should be able to query a company's entire knowledge base constantly — thousands of reads per day per agent — at negligible cost. Existing vector DBs optimize for the general case; Polyvec optimizes for one: **massive, cheap, high-recall reads over company knowledge**, as the knowledge layer of Polygrow and future businesses.

## What it is not (for now)

- A general-purpose Pinecone/Qdrant replacement.
- The memory layer (episodic/conversational memory stays separate).

## Status

**Research phase** (as of 2026-09-26): evaluating Rust crates and algorithms for massive cheap reads by agents. No code yet — see `docs/research/` for the landscape survey.

**Next:** scaffold the DB core once research concludes.

## License

No license file yet.
