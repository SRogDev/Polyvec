# Polyvec — Project Brief

> Full project context in one file. Hand this to ANOTHER AI (GPT, etc.) for planning
> and ideation, then bring the refined specs back. Keep this file accurate — it is the handoff doc.
> For the current timeline see `STATUS.md`. For how to work in this repo see `AGENTS.md`.

## One-liner
Polyvec is Roger's from-scratch vector database in Rust — the future knowledge layer of Polygrow.

## Problem & audience
Agent workloads need massive cheap vector reads; existing vector DBs are either heavyweight, expensive at scale, or not designed for agent access patterns.

## Product (what it is / is not)
A company knowledge-base vector DB in Rust, optimized for massive cheap reads by agents. It is NOT a general-purpose database and NOT a wrapper over an existing engine — it's built from scratch to own the stack.

## Key decisions (locked)
- Rust, from scratch (own the substrate — same thesis as Rogis).
- Read-optimized for agent workloads; future knowledge layer of Polygrow.

## Stack
Rust (crates/TBD from research).

## Business model
Infrastructure for Polygrow — no standalone business model yet.

## Open questions
- Which index structures (HNSW variants, quantization) fit the read-heavy agent pattern.
- API shape: how agents will query it.
