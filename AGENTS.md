# AGENTS.md — Polyvec

Working agreement for any AI agent operating in this repo (Muse, GPT, Codex, Cursor, etc.).
Owner: Roger (SRogDev). Discussion language: Spanish. Implementation plans for coding agents: English.

## How Roger works — read this first
- Incremental MVPs over big up-front design. Architecture emerges from concrete needs; no speculative complexity.
- "Learning by building": implement the minimum that works, learn when real problems arise, ship artifacts.
- Always inspect the repo before making architectural or file-level decisions.
- Reuse existing systems instead of creating parallel abstractions.
- Many functional, basic MVPs first; polish later.
- Explanations: only what's needed to build and solve the immediate problem. No theory dumps.
- Be an auditor, not a cheerleader: evidence-only assessments, uncomfortable findings stated plainly, always paired with concrete next options.
- Corrections are final: adopt Roger's version without debate and keep moving.
- No unprompted initiatives outside the approved scope — offer options instead.

## Engineering discipline
- ALL coding work: ODD routing (route by size) → TDD (test evidence) → RDD (diff self-review before delivery). No exceptions. (Muse: use the `gentle-ai` workspace skill.)
- ALL frontend/UI work: design-system-first — resolve style, palette, fonts, UX guidelines BEFORE implementing. No exceptions. (Muse: use the `ui-ux-pro-max` workspace skill, run its design-system search first.)
- Python: `uv` ONLY — `uv venv`, deps in `pyproject.toml [project]`, `uv sync`, `uv.lock` committed. Never `python -m venv` + pip, never bare `requirements.txt`.
- JS/TS: check the latest Next.js version before installing (16.x is current as of 2026-09); use the official codemod for upgrades. Prefer Biome over ESLint/Prettier.
- Conventional commits: `feat:` / `fix:` / `chore:` / `docs:` / `refactor:` / `test:`.

## PR workflow & history (mandatory)
- Every change ships as a PR against `main`. Muse is authorized to merge his own PRs.
- One PR = one logical change. Squash-merge so `main` stays linear and readable — one clean commit per change.
- Never push directly to `main`.
- PR description must include verifiable evidence: test counts, build results, commit SHAs. Never invent numbers — write "not measured" when unknown.
- When a phase/milestone completes, update STATUS.md in the same PR.

## Context docs (keep them current)
- `README.md` — human/contributor-facing overview.
- `docs/PROJECT_BRIEF.md` — full project context in one file, written to be handed to ANOTHER AI for planning/ideation. Keep it accurate; it is the handoff doc.
- `STATUS.md` — timeline: done / doing / next. Read it before starting work; update it when reality changes.

## Stack
- Rust, from-scratch vector database.
- Purpose: company knowledge-base vector DB optimized for massive cheap reads by agents; future knowledge layer of Polygrow.
- Status: early research phase on Rust pieces/algorithms (see STATUS.md).

## Repo map
- Repo scaffold created 2026-09-26; layout to be defined as research concludes.
- Commands: `cargo build` / `cargo test` / `cargo clippy`

## What NOT to do
- No speculative abstractions, "just in case" features, or parallel systems.
- Don't reformat whole files for style; keep diffs reviewable.
- Never commit secrets, `.env` files, or credentials.
- Don't invent metrics, benchmarks, or test results.
