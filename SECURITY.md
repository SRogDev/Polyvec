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

---

## Phase 3 HTTP threat model (boundary-breaker audit, 2026-10-15)

Method: trust-boundary inversion (inspect-and-reason; no active
exploitation). Findings are tiered **CONFIRMED** (tested or traced
end-to-end in code) / **PLAUSIBLE** (reasoned, with a named confirmation
test) / **UNTESTED** (hypothesis). The headline first: **Phase 3 has no
authentication — anyone who can reach the socket has full read/write/delete.**
The entire network posture below exists to make that fact safe by default.

### Boundary map

| # | Boundary | Trusts | Crosses it |
|---|----------|--------|------------|
| B1 | Network → server | Only the operator reaches the socket | TCP connections, HTTP requests |
| B2 | JSON body → domain | Bodies are well-formed, sized, finite | Request JSON |
| B3 | Path `{id}` → filesystem | Ids are benign | Collection/vector/document ids → `data_dir.join(id)` |
| B4 | Concurrent requests → shared store | Requests don't interleave destructively | Mutations on the same collection |
| B5 | Disk files → open/recovery | Files are as the server left them | redb catalog, hnsw dumps, sidecar |
| B6 | Server → logs | Logs don't leak secrets | Error details, request metadata |
| B7 | Shutdown signal → durability | SIGTERM leaves a clean state | In-flight requests, unflushed index |
| B8 | Supply chain → binary | Dependencies are benign | crates.io packages |

### Findings

| Finding | Tier | Evidence / why not higher | Impact | Mitigation | Verification |
|---|---|---|---|---|---|
| No auth: socket reach = full R/W/D | CONFIRMED | By design; code has no auth path | Data theft/destruction by anyone on the network | Localhost-only default; non-loopback bind refused without `POLYVEC_ALLOW_NONLOCAL_BIND=1` | Live test: `0.0.0.0` bind exits 2 with refusal message; opt-in binds OK |
| Path traversal via `{id}` | CONFIRMED | Traced: `CollectionId::new` slug charset `[a-z0-9-_]` makes `..` unrepresentable; derived ids also reject `/` `\` | Read/write outside data dir | Validation by construction + defense-in-depth separator rejection | Test: `/v1/collections/../evil/stats` and `%2e%2e` → 404/400, no dir created outside data dir |
| Oversized JSON body | CONFIRMED | `JsonConfig::limit` + custom handler | Memory exhaustion | 413 with structured body (both actix `Overflow` variants) | Test: >8KB body → 413 `payload_too_large` |
| Non-finite floats (`1e300` → inf) | CONFIRMED | `Vector::new` finiteness check | NaN/Inf poisoning the index | 400 `validation_error` | Test: raw-JSON `1e300` → 400 |
| Malformed JSON / wrong types | CONFIRMED | Custom `json_error_handler` | 500s / panics on bad input | 400 `invalid_json` structured | Test |
| Nested metadata objects | CONFIRMED | `metadata_from_json` closed value model | Schema confusion downstream | 400 `validation_error` | Test |
| Corrupt/Storage/Internal detail leakage | CONFIRMED | `From<CoreError> for ApiError` maps to safe messages; detail only via `log::error!` | Path / io-error disclosure | Safe client message, full detail server-side | Unit test renders the body, asserts no path leaks and `"detail":null` |
| Vector values in logs | CONFIRMED | Code-traced: handlers log ids/counts/generations only | Embedding disclosure via logs | Logging discipline (documented in `routes.rs`) | `grep` for `log!` — no values logged |
| Slow insert stalls `/healthz` | CONFIRMED | All store ops in `web::block`; mutex never held across `.await` | Availability | Blocking-pool isolation | Test: healthz < 1s during multi-second batch insert |
| redb double-open on list | CONFIRMED | redb errors "Database already open"; `Registry::list` serves open stores from memory | 500 on `GET /v1/collections` | Memory-first listing; disk reads only for unopened dirs | Failing test → fix → green |
| `PUT` document counter inflation | CONFIRMED | `put_document` re-put double-counted | Wrong stats (worse than none) | Fixed counters on overwrite | Storage test |
| h2 advisory RUSTSEC-2026-0258 | CONFIRMED | `cargo deny check` output | Unbounded memory via HTTP/2 DATA frames | Unreachable: plaintext HTTP/1.1 only, no TLS (HTTP/2 needs ALPN+TLS); documented ignore | `cargo deny check` green |
| Delete-vs-write race | PLAUSIBLE | Reasoned: loser holds `Arc`, finishes on unlinked dir; redb handle stays valid | Honest 500s, no corruption (weakest link: untested live) | Documented behavior | Confirmation test: concurrent delete+insert soak, assert no panic + sane final state |
| Create-vs-create same-id race | PLAUSIBLE | Reasoned: map check + `create` dir check; pathological interleave → 500 not corruption | Brief 500 for the loser | Documented | Confirmation test: barrier-synchronized concurrent creates |
| Huge under-limit batch (16MB) | PLAUSIBLE | Reasoned: one atomic batch holds the collection mutex for minutes | Availability (not corruption) | Operator-tunable JSON limit | Confirmation test: time a 16MB batch insert |
| `ALLOW_NONLOCAL_BIND=1` w/o firewall | PLAUSIBLE | Reasoned: opt-in is explicit but the network may not be trusted | Unauthenticated exposure | Documented warning in config docs + refusal message | External port scan (needs network scope — not performed) |
| serde_json nesting depth | UNTESTED | Default recursion limit (128) believed safe; not asserted | Stack exhaustion on hostile nesting | Default limit | Assert with a 200-deep payload test |
| `kill -9` mid-batch over HTTP | UNTESTED | Covered at storage layer (Phase 2 recovery tests), not over HTTP | None expected (ADR-004) | Recovery on open | Live kill -9 + reopen test |

### Residual notes

- There is no rate limiting in Phase 3 (localhost threat model). If the
  server is ever exposed beyond localhost, add auth + rate limiting first —
  tracked as a Phase 7 concern.
- `RUST_LOG=debug` increases log volume but handlers never log bodies or
  vector values at any level.
