# ADR-001 — Actix Web over Axum for the HTTP layer

- **Status:** accepted (Phase 0)
- **Date:** 2026-10-15
- **Deciders:** Roger (SRogDev)

## Context

Polyvec needs an HTTP API layer (plan Phase 5: `polyvec-api`) serving the
read-optimized query path to agents. The two realistic Rust choices are
**Actix Web** and **Axum**. The decision must be made in Phase 0 so later
phases build against one framework's patterns.

Constraints from the plan:

- Stack is fixed: **Tokio + Actix Web** (plan §2 / stack decision).
- Workload: massive concurrent cheap reads — throughput per core and
  connection-handling efficiency matter more than handler ergonomics.
- Team of one (+ AI builders): mature ecosystem and long-lived patterns
  reduce integration risk.

## Decision

Use **Actix Web** for the HTTP layer (`polyvec-api`), on Tokio.

## Consequences

- **Positive:** Actix Web is the most battle-tested high-throughput Rust HTTP
  framework; its actor-based request handling and extensive middleware
  ecosystem fit a read-heavy API. Decision aligns with the plan's fixed stack,
  so no phase re-litigates it.
- **Positive:** The boundary is contained: Actix types live only in
  `polyvec-api` (see `ARCHITECTURE.md` dependency rules) — the core, index,
  and storage crates never import it.
- **Negative:** Actix's API surface is larger and its learning curve steeper
  than Axum's; handler code must be disciplined to stay simple.
- **Negative:** If Axum's ecosystem later dominates (tower middleware,
  axum-extra), migrating is a rewrite of one crate — acceptable, because the
  boundary keeps the blast radius small.

## Alternatives considered

- **Axum**: cleaner ergonomics, growing fast, but the plan fixed Tokio +
  Actix Web and the workload favors Actix's throughput track record. Not
  chosen.
