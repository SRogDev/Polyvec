# Releasing Polyvec

Reproducible procedure for cutting a release (e.g. `v0.2.0`). The
coordinator owns the gate; the tag lands only after it passes.

## Preconditions

- `main` is green: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` (no `--all` — vendor/markit stays byte-identical),
  `cargo deny check`, plus live re-runs of `docs/e2e.sh`,
  `docs/e2e-ingestion.sh`, `docs/e2e-retrieval.sh`,
  `docs/e2e-relations.sh`, and the site build
  (`cd site && npm ci && npm run build`).
- Rust 1.85+ required (edition 2024); releases are built with the
  current stable on the release machine (v0.1.0: 1.98.1). Record the
  actual toolchain in the notes.
- Release notes drafted at `docs/RELEASE_NOTES_vX.Y.Z.md` following the
  v0.1.0 template: what it does / what it doesn't / how to verify /
  license / known gaps. No invented numbers — only measured or "not
  measured".

## Steps

1. **Branch**: `git checkout -b release-vX.Y.Z` from `main`.
2. **Bump version**: workspace `version` in root `Cargo.toml`; the
   server's `/healthz` reads it — verify.
3. **Lock dependencies**: `cargo update` only for reviewed bumps
   (re-run `cargo deny check`); commit the resulting `Cargo.lock`.
4. **Docs**: update `README.md` (status/phase table, install,
   quickstart, known limitations — English), `STATUS.md`,
   `docs/PROJECT_BRIEF.md`, and `docs/audit/license-audit.md` if the
   dependency tree changed. Touch up `BENCHMARKS.md` only with freshly
   measured runs.
5. **Verify**: full gate above on the branch; commit everything with
   conventional commits. One PR against `main`, squash-merge
   (coordinator merges own PRs per standing authorization).
6. **Acceptance gate (coordinator)**: re-runs build, tests, e2e scripts,
   site build independently; checks the 14 release items with evidence;
   confirms `cargo tree` contains no vector-DB engine.
7. **Tag**: only after the gate passes. Create via the GitHub API
   (`POST /repos/{owner}/{repo}/git/refs` — `refs/tags/vX.Y.Z`) —
   a tag ref, not a main push. Never push directly to `main`.
8. **GitHub release**: publish from the tag; body = the release notes
   file. Attach nothing (source release; binaries via local build).

## Standing constraints (every release)

- One PR = one logical change; squash-merge keeps `main` linear.
- `unsafe` forbidden; clippy all+pedantic; `fmt` clean; `cargo deny`
  green (bans/licenses/advisories/sources).
- `deny.toml` policy: MIT/Apache-2.0 preferred; any new license class
  needs an explicit exception + flag in the notes for Roger's review.
- `.github/workflows/ci.yml` cannot be committed via the stored token
  (lacks `workflows` scope) — commit with a full-scope token or a local
  git remote, and record it in the release notes until done.
- The docs site (`site/`) is deployed by importing the repo in the
  Vercel dashboard (manual until the Vercel connection is restored).
