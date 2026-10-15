# Security Policy

## Supported versions

Polyvec is pre-release (v0.x). Security fixes land on the latest `main`;
there is no LTS branch yet. Once versions are tagged, this table will list
which releases receive security patches.

| Version | Supported |
| ------- | --------- |
| main    | yes       |
| < 0.1   | no        |

## Reporting a vulnerability

Report privately — do not open a public issue for a suspected vulnerability.

- Email the maintainer via the address on the GitHub profile of SRogDev,
  or open a [GitHub private vulnerability report](https://github.com/SRogDev/Polyvec/security/advisories/new)
  on SRogDev/Polyvec.
- Include: what is affected, steps to reproduce, and the impact you see.
- Expect an acknowledgement within 7 days and a fix-or-plan within 30 days.
  If you hear nothing, a public issue is acceptable as a last resort.

## What is covered

- Memory-safety issues (Polyvec forbids `unsafe` workspace-wide; any soundness
  hole is treated as critical).
- Authentication/authorization bypasses in the HTTP API (Phase 3+).
- Deserialization or input-validation flaws that allow crashes or escapes.
- Dependency vulnerabilities flagged by `cargo audit` / `cargo deny`
  (`deny.toml` + `audit.toml` are enforced in CI).

## Dependency hygiene (everyone's job)

- `cargo deny check` — license policy: **MIT/Apache-2.0 only**, copyleft denied.
- `cargo audit` — known advisories in the dependency tree.
- Both run in CI on every push. Do not merge with a failing audit without a
  documented exception in `docs/decisions/`.

## Out of scope

- Social engineering, physical access, or attacks on contributors' machines.
- Vulnerabilities in third-party services (embedding providers, cloud hosts).
