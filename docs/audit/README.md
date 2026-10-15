# docs/audit

Audit reports produced by the Phase 1 crate audit (capability-scout +
cost-guard) and later reviews land here.

Expected contents (Phase 1):

- `crates-audit.md` — candidate Rust crates for index/storage/HTTP/embeddings:
  what each does, maintenance health, license check, and the accept/reject
  verdict with reasoning.
- `license-audit.md` — license inventory of accepted candidates against the
  MIT/Apache-2.0-only policy (`deny.toml`); any exception needs an ADR.

Naming: `audit-YYYY-MM-DD-<topic>.md` for later ad-hoc audits.
