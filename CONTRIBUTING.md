# Contributing to Polyvec

Polyvec is an open-source, self-hostable vector database engine in Rust. This
file sets the working agreement for contributors (human and AI alike).

## Ground rules

- **One PR = one logical change**, against `main`. Never push directly to `main`.
- **Conventional commits**: `feat:` / `fix:` / `chore:` / `docs:` / `refactor:` / `test:`.
- **PR bodies carry verifiable evidence**: test counts, build results, commit SHAs.
  Never invent numbers — write "not measured" when unknown.
- **README touch-up**: every significant change ships a README update in the same PR.
- **STATUS.md**: update it in the same PR when reality changes (phase done, new phase started).

## Engineering discipline

- **ODD → TDD → RDD** on all coding work: route by size, write the failing test
  first, self-review the diff before delivery. No exceptions.
- **Formatter + linter run before every PR**, zero errors:
  `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`.
- **Tests must exist for new behavior.** CI runs build + test + clippy + fmt + audit
  on every push.
- **No dead code, no TODOs masquerading as finished work.** Label stubs honestly
  or don't ship them.
- **No speculative abstractions.** Crates, modules, and features are created when
  a concrete use case needs them — never "just in case" (see
  `docs/decisions/ADR-002-modular-monolith.md`).

## Dependency policy (hard)

- **MIT or Apache-2.0 only.** Enforced by `deny.toml` (`cargo deny check`);
  CI fails the PR on any other license, including copyleft.
- Dependencies are workspace-pinned in root `Cargo.toml` `[workspace.dependencies]`.
- Before adding a dependency, check: is it maintained, does the license fit, and
  is there a real need? Prefer std over crates; prefer small, focused crates over
  frameworks.

## Security

- Run `cargo audit` locally before opening a PR.
- **Never commit secrets**: `.env` files, API keys, tokens, credentials.
  The repo `.gitignore` excludes `.env`; keys arrive via environment variables.
- Validate external input at boundaries (see `SECURITY.md` for reporting).

## Code style notes (Rust)

- Edition 2024, `rustfmt.toml` at the root. `unsafe` is forbidden workspace-wide.
- Public APIs carry doc comments (`missing_docs` is a workspace warning).
- Errors use `thiserror`; crate errors convert into `polyvec_core::Error`
  where they cross crate boundaries.

## Docs site

Phase 8 builds an Astro Starlight docs site. Markdown in `docs/` should be
site-ready: one `#` title per file, relative links, no absolute repo paths.
