# docs/audit

Audit reports from the Phase 1 crate audit (capability-scout + cost-guard)
and later reviews.

Contents:

- `crates-audit.md` — candidate Rust crates (index/storage/lexical/conversion):
  rubric, TESTED evidence, and adopt/adapt/reference/defer/reject verdicts.
  **Decisions:** hnsw_rs 0.3.5 ADOPT (ANN), redb 4.4.0 ADOPT (catalog),
  tantivy 0.26.2 ADOPT (lexical), markit ADAPT (vendored, adapter trait),
  zvec-rs REFERENCE ONLY (TESTED broken), Qdrant REFERENCE ONLY (mandated).
- `license-audit.md` — license inventory of the selected stack against the
  dependency policy; permissive-license exceptions are documented here and in
  `deny.toml`. **No copyleft anywhere in the 340-package spike tree.**
- `spike-phase1/` — the executable prototype behind the audit: ANN head-to-head
  (hnsw_rs vs zvec-rs, recall@10 vs brute force), redb durability, tantivy
  BM25/delete/reopen, markit API compile-test. See its README for commands
  and measured results. `vendor/markit-src` intentionally excluded (see README).

Naming: `audit-YYYY-MM-DD-<topic>.md` for later ad-hoc audits.
