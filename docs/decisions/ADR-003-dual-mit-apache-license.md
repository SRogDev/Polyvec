# ADR-003 — Dual MIT/Apache-2.0 licensing

- **Status:** accepted (Phase 0)
- **Date:** 2026-10-15
- **Deciders:** Roger (SRogDev)

## Context

Polyvec is an open-source, self-hostable vector database engine meant for
broad adoption — including embedding in commercial products (it is the future
knowledge layer of Polygrow). The license must:

1. Allow commercial use, modification, and redistribution without friction.
2. Keep the dependency tree license-clean: no copyleft leaking in through a
   transitive crate.
3. Follow Rust ecosystem convention so contributors need no legal onboarding.

## Decision

Dual-license under **MIT OR Apache-2.0** (files `LICENSE-MIT`,
`LICENSE-APACHE`; `license = "MIT OR Apache-2.0"` in every crate).

Consequence of the choice: the dependency policy is **MIT/Apache-2.0 only** —
enforced by `deny.toml` (`copyleft = "deny"`, explicit allow-list) and checked
in CI. Any dependency under another license needs an ADR-level exception; in
practice, the answer is "find another crate".

## Consequences

- **Positive:** Maximum adoption surface — the same dual license as the Rust
  toolchain itself; companies can evaluate without legal review.
- **Positive:** The allow-list in `deny.toml` makes the policy machine-checked,
  not tribal knowledge.
- **Negative:** Some useful crates are copyleft (GPL/AGPL) or carry
  non-standard licenses and are permanently off the table — the Phase 1 crate
  audit must verify licenses before technical fit.
- **Negative:** Dual licensing requires keeping both license files accurate;
  trivial, but it is a standing obligation on every release.

## Alternatives considered

- **Elastic License 2.0** (used by some of Roger's other projects): rejected
  for Polyvec — source-available with commercial restrictions would fight the
  "broadly embeddable knowledge layer" goal.
- **AGPL-3.0**: rejected — would force SaaS operators to open-source,
  shrinking the adoption surface the project optimizes for.
- **MIT-only / Apache-2.0-only**: both fine individually; dual is the Rust
  convention and strictly more permissive in combination.
