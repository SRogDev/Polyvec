# ARCHITECTURE — Polyvec

> How the codebase is organized and why. Decisions live in `docs/decisions/`
> as ADRs; this file is the map, not the minutes.

## Thesis

Polyvec is a from-scratch vector database engine in Rust, optimized for one
workload: **massive, cheap, high-recall reads of company knowledge by AI
agents**. Every architectural choice is scored against read cost at scale —
not write throughput, not feature breadth.

## Workspace layout

```text
Polyvec/
├── Cargo.toml                  # [workspace]: members, pinned deps, shared lints
├── rustfmt.toml                # edition = "2024"
├── deny.toml                   # license policy (MIT/Apache-2.0 only)
├── audit.toml                  # cargo-audit config
├── crates/
│   └── polyvec-core/           # Phase 0. The ONLY crate so far.
├── docs/
│   ├── PROJECT_BRIEF.md        # full context handoff for another AI
│   ├── decisions/              # ADRs (ADR-001, ADR-002, ADR-003, …)
│   ├── audit/                 # crate & license audit reports (Phase 1+)
│   └── research/               # pre-plan landscape research
├── ARCHITECTURE.md             # this file
├── BENCHMARKS.md               # benchmark methodology (results: not measured yet)
├── CONTRIBUTING.md
├── SECURITY.md
├── SaaS.md                     # future SaaS requirements — notes only, not built
└── .github/workflows/ci.yml    # local-only until token scope is resolved
```

## Crate responsibilities (plan §4)

The definitive plan defines seven crates with fixed responsibilities.
**Only `polyvec-core` exists in Phase 0** — the other six are created in the
phase that first needs them (see "No empty placeholder crates" below).

| Crate | Responsibility (plan §4) | Created in |
| ----- | ------------------------ | ---------- |
| `polyvec-core` | Domain vocabulary: types, validated IDs, config contracts, shared `Error` | Phase 0 (now) |
| `polyvec-index` | Vector index implementations (HNSW et al.), bounded graph retrieval | Phase 2 |
| `polyvec-store` | Durable segment storage, manifest, WAL | Phase 3 |
| `polyvec-embed` | Embedding provider adapters (pluggable) | Phase 4 |
| `polyvec-api` | Actix Web HTTP surface | Phase 5 |
| `polyvec-serve` | Binary: wiring, config loading, graceful shutdown | Phase 6 |
| `polyvec-bench` | Benchmark harnesses backing `BENCHMARKS.md` | Phase 7 |

Phase numbers are indicative of first need, not a promise; a phase that
discovers it doesn't need its crate doesn't create it.

## Dependency boundaries (the one rule that matters)

Dependencies point **inward only**. Arrows mean "depends on":

```text
polyvec-serve
    └── polyvec-api          (Actix Web lives HERE — and only here)
            └── polyvec-embed       (provider SDKs, HTTP clients)
            └── polyvec-index       (index algorithms)
            └── polyvec-store       (storage engine)
                    └── polyvec-core   (domain types, IDs, config, Error)
```

- **`polyvec-core` has no framework dependencies.** No Actix, no Tokio, no
  HTTP, no provider SDKs — only `thiserror` today. It must stay compilable as
  a pure library: embed it in tests, CLIs, and future crates without dragging
  a web stack along.
- Each crate depends only on crates below it. If a lower crate needs
  something from above, the abstraction is in the wrong place — move the
  trait definition down, not the dependency up.
- Shared lints are inherited from the workspace root (`[lints] workspace =
  true` in each crate): `unsafe` forbidden, `missing_docs` warned, Clippy
  `all` + `pedantic`.

## No empty placeholder crates

A deliberate decision, not laziness: creating six empty crates with
`lib.rs` stubs would add maintenance surface (lints, docs, version churn)
with zero behavior. Per the plan's "no abstraction without a concrete use
case" principle, each crate is born in the PR that gives it its first real
responsibility. `ARCHITECTURE.md` grows with the workspace.

## Key design decisions (ADRs)

- **ADR-001** — Actix Web over Axum for the HTTP layer (`docs/decisions/`).
- **ADR-002** — Modular monolith (one repo, many crates) over microservices
  or a single flat crate.
- **ADR-003** — Dual MIT/Apache-2.0 licensing and the MIT/Apache-2.0-only
  dependency policy it forces.

## Data flow (as of Phase 0)

There is no data flow yet — only the foundation. The first real flow
(index build → segment write → read path) arrives with Phases 2–3 and will
be documented here when it exists, not before.
