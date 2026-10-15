# integrate-polyvec — versioned AI integration prompt

The current version is **v1** (`integrate-polyvec/v1.md`).

## How to use this

If you are an AI agent asked to integrate Polyvec into a project:
1. Open the **latest** versioned prompt (`v1.md` today).
2. Follow it end to end. It **requires** a real end-to-end verification run —
   a prompt run that never executes a live query against a running server
   is not a completed integration.
3. When you are done, report: what changed, the exact verification
   commands you ran and their results, and your unverified assumptions.

Do not invent a "newer" version. If a behavior changed, the maintainers
bump the version header; the old one stays for reproducibility.

## Live test evidence

- `integrate-polyvec/e2e/v1-test-record.md` — the recorded run of a
  throwaway Python client (stdlib only) following v1 against a real local
  `polyvec-server`, with commands and outputs.
