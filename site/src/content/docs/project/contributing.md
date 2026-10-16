---
title: Contributing & dev setup
description: How to work in the Polyvec repo — discipline, commands, PR workflow.
---

Polyvec is built by AI agents under a strict working agreement
(`AGENTS.md` in the repo). Humans are welcome too — the same rules apply.

## Engineering discipline

- **ALL coding work**: ODD routing (route by size) → TDD (test evidence)
  → RDD (diff self-review before delivery). No exceptions.
- **ALL frontend/UI work**: design-system-first — resolve style, palette,
  fonts, UX guidelines before implementing.
- **Python**: `uv` only (`uv venv`, `uv sync`, `uv.lock` committed) —
  not that Polyvec has Python; this is for tooling around it.
- **JS/TS**: check the latest versions before installing; prefer Biome
  over ESLint/Prettier.
- **Conventional commits**: `feat:` / `fix:` / `chore:` / `docs:` /
  `refactor:` / `test:`.

## Dev setup

```bash
git clone https://github.com/SRogDev/Polyvec.git
cd Polyvec
export PATH="$HOME/.cargo/bin:$PATH"   # rustc 1.98.1
cargo build --workspace
```

Commands that must be green before any PR:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check        # no --all: it descends into vendor/markit
cargo deny check
```

Rust edition 2024. Workspace lints: `unsafe` forbidden, `missing_docs`
warned, Clippy `all` + `pedantic`. `deny.toml` enforces the license policy
(MIT/Apache-2.0 + documented exceptions; copyleft banned). `cargo audit`
runs in CI.

## PR workflow

- Every change ships as a PR against `main`. One PR = one logical change;
  squash-merge so `main` stays linear — one clean commit per change. Never
  push directly to `main`.
- PR description must include verifiable evidence: test counts, build
  results, commit SHAs. Never invent numbers — write "not measured" when
  unknown.
- When a phase/milestone completes, update `STATUS.md` in the same PR.
- **README rule**: every significant change ships a README touch-up in the
  same PR — READMEs in English and current.

## Context docs (keep them current)

- `README.md` — human/contributor-facing overview.
- `docs/PROJECT_BRIEF.md` — full project context for handing to another AI.
- `STATUS.md` — timeline: done / doing / next. Read it before starting
  work; update it when reality changes.
- `ARCHITECTURE.md` — the codebase map (grows with the workspace).
- `docs/decisions/` — ADRs; `docs/audit/` — crate & license audit reports.

## What not to do

- No speculative abstractions, "just in case" features, or parallel systems.
- Don't reformat whole files for style; keep diffs reviewable.
- Never commit secrets, `.env` files, or credentials.
- Don't invent metrics, benchmarks, or test results.
