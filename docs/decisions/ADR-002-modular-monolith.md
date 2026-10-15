# ADR-002 — Modular monolith over microservices or a flat crate

- **Status:** accepted (Phase 0)
- **Date:** 2026-10-15
- **Deciders:** Roger (SRogDev)

## Context

The plan (§4) defines seven areas with distinct responsibilities: core domain
types, vector index, storage engine, embedding adapters, HTTP API, server
binary, benchmarks. The repo structure must express those boundaries without
creating operational or maintenance overhead for a one-person (+ AI) team.

## Decision

**Modular monolith**: one Cargo workspace, many crates, one repo.

- Each responsibility is a crate with a public API surface (`ARCHITECTURE.md`
  lists the seven).
- Dependencies point inward only (`polyvec-core` at the bottom, framework
  code only in `polyvec-api`); the compiler enforces the layering.
- Crates are created **when their phase first needs them** — no empty
  placeholder crates (see `ARCHITECTURE.md`). Phase 0 ships only
  `polyvec-core`.

## Consequences

- **Positive:** Boundaries are real (separate crates, separate APIs) but
  refactoring across them is one commit — no versioning dance, no network
  hops, no separate deploys.
- **Positive:** `unsafe_code = "forbid"` and shared Clippy lints apply
  workspace-wide from day one.
- **Positive:** The "no placeholder crates" rule keeps the tree honest: every
  directory that exists does something.
- **Negative:** A crate boundary crossed too often suggests the split is
  wrong — the team must watch for chatty APIs between crates and merge or
  move code rather than layer workarounds.
- **Negative:** One repo means one release story; if a crate ever needs
  independent versioning (unlikely pre-1.0), the workspace can still publish
  crates independently — the option stays open.

## Alternatives considered

- **Microservices**: separate repos/processes per area. Rejected — the
  workload is a single self-hosted engine; network boundaries would add
  latency and ops cost with no scaling benefit at this stage.
- **Single flat crate**: rejected — the seven responsibilities would blur
  within months; the layered dependency rule ("core has no Actix") would be
  unenforceable.
