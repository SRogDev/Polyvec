# Polyvec docs site

Astro Starlight documentation site for Polyvec. Pinned versions:
`astro 7.4.0`, `@astrojs/starlight 0.42.6` (checked 2026-10-15).
`package-lock.json` is committed — builds are reproducible via `npm ci`.

## Develop

```bash
npm ci
npm run dev      # http://localhost:4321
npm run build    # static output in dist/
npm run preview
```

## Deploy (Vercel)

The site is fully deploy-ready: static output, no adapter, no
`vercel.json` needed (Vercel auto-detects Astro; build command
`npm run build`, output directory `dist`).

**Pending (manual — the Vercel connection is degraded):** creating the
Vercel project is a manual step until the connection is repaired:

1. In Vercel: Add New → Project → Import `SRogDev/Polyvec`.
2. Root Directory: `site`. Framework preset: Astro (auto-detected).
3. Build & Development Settings: leave defaults
   (`npm run build` → `dist`).
4. Deploy.

Do not block on this — the site builds and can be previewed locally.

## Content rules

- Every endpoint, env var, default, and limit must match the implemented
  code (`crates/*/src`) and the repo docs (`docs/*.md`, `README.md`,
  `ARCHITECTURE.md`). No advertising future functionality.
- `project/ai-integration.md` mirrors `../integrate-polyvec/v1.md` —
  if they disagree, the repo file wins; regenerate the page when the
  prompt version bumps.
- Honesty ledger items (stub = mechanics-only, no invented metrics,
  MPL-2.0 flag) apply to every page — see the repo `STATUS.md`.
