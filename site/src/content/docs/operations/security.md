---
title: Security & network exposure
description: Threat model, authentication, rate limiting, and how to expose Polyvec safely.
---

## Supported versions

Polyvec is pre-release (v0.x). Security fixes land on the latest `main`;
there is no LTS branch yet.

To report a vulnerability: report privately — do not open a public issue.
Email the maintainer via the address on the GitHub profile of SRogDev, or
open a [GitHub private vulnerability report](https://github.com/SRogDev/Polyvec/security/advisories/new).
Include what is affected, steps to reproduce, and the impact. Expect an
acknowledgement within 7 days and a fix-or-plan within 30 days.

## Threat model (summary)

An inspect-and-reason boundary audit (2026-10-15) found, headline first:
**without `POLYVEC_API_KEY`, anyone who can reach the socket has full
read/write/delete.** The entire network posture exists to make that fact
safe by default:

- The server binds **localhost only**; non-loopback binds are refused
  without `POLYVEC_ALLOW_NONLOCAL_BIND=1`.
- Bearer-token auth (`POLYVEC_API_KEY`, constant-time compare, key
  redacted from logs and error responses; `/healthz`/`/readyz` stay open).
- Per-IP token-bucket rate limiting (`POLYVEC_RATE_LIMIT_RPS`, default
  1000, burst 4×; `429` + `Retry-After`).
- Path traversal is unrepresentable: collection ids are validated slugs
  (`[a-z0-9-_]`), and derived ids reject `/` and `\`.
- JSON bodies are capped at 16 MiB (`413`); nested metadata is rejected at
  the serde recursion limit; non-finite floats are `400`.
- Handlers log ids/counts/generations only — never vector values, never
  the API key.
- The vendored markit's HTTP fetcher is stubbed: the ingest API takes
  `source` as an opaque label plus content bytes — **Polyvec never
  fetches URLs** (no SSRF surface), and no endpoint reads
  operator-supplied filesystem paths.

Full findings with CONFIRMED/PLAUSIBLE/UNTESTED tiers and the Phase 7
delta are in `SECURITY.md` in the repo.

## Safe network exposure

If the server must be reachable beyond localhost:

1. **Set `POLYVEC_API_KEY`** to a long random secret and require
   `Authorization: Bearer <key>` on every `/v1` call. Without it, the
   server logs a loud startup warning when `POLYVEC_ALLOW_NONLOCAL_BIND=1`.
2. **Put it behind TLS** — Polyvec speaks plaintext HTTP/1.1 only. Use a
   reverse proxy (nginx, Caddy) for TLS termination.
3. **Rate-limit at the proxy too**: Polyvec's limiter is in-memory and
   per-process (resets on restart); behind a proxy it sees the proxy's IP.
4. **Firewall**: allow only the proxy to reach the Polyvec port.

```bash
POLYVEC_ALLOW_NONLOCAL_BIND=1 \
POLYVEC_BIND=0.0.0.0:8080 \
POLYVEC_API_KEY="$(openssl rand -hex 32)" \
polyvec-server
```

## Dependency hygiene

- `cargo deny check` enforces the license policy (MIT/Apache-2.0 +
  documented exceptions; copyleft banned). `deny.toml` is the policy.
- `cargo audit` runs in CI; in this sandbox advisory coverage comes from
  `cargo deny check` (`advisories`).
- Do not merge with a failing audit without a documented exception in
  `docs/decisions/`.

## Known limits

- Rate limiting is single-process and in-memory.
- `kill -9` recovery over HTTP is untested (storage-layer recovery is
  tested; graceful-shutdown recovery over HTTP is measured).
- `cargo audit` does not build in this sandbox (linker env quirk);
  advisory coverage comes from `cargo deny`.
