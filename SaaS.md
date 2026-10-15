# SaaS — Future Requirements (notes only)

> Per plan §20: Polyvec is an **open-source, self-hostable** engine. This file
> records what a future hosted SaaS offering would need — it is **not a
> roadmap and nothing here is being built**. Any line in this file becoming a
> task requires an explicit owner decision first.

## Why these notes exist

Self-hosting is the product; a managed tier is a possible future business
layer on top of the same engine. Capturing the requirements now keeps the
engine's seams honest (multi-tenancy must not be retrofitted later), without
spending a single line of code on SaaS today.

## Requirements a future SaaS would need

- **Multi-tenancy**: tenant-scoped collections and indexes; noisy-neighbor
  isolation on the read path; per-tenant quotas (vectors, QPS, storage).
- **Billing**: usage metering (reads/writes/storage) with an auditable ledger;
  the engine must expose cheap counters, not invent them later.
- **AuthN/AuthZ**: API keys per tenant, scoped tokens, key rotation; admin
  surface separated from the data plane.
- **Operations**: managed upgrades with zero-downtime index rebuilds, backups
  and point-in-time restore, per-tenant SLOs and status visibility.
- **Compliance**: data residency options, deletion guarantees (right to be
  forgotten must actually delete vectors *and* index artifacts), audit logs.
- **Onboarding**: hosted embeddings option for tenants without a provider key;
  migration path from self-hosted to managed without re-indexing.

## Explicit non-goals (today)

No billing code, no tenant model in the schema, no hosted auth service, no
admin dashboard, no pricing page. The engine ships self-hosted; SaaS stays a
paragraph until it earns a plan of its own.
