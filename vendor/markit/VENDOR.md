# Vendored markit — Polyvec notes

## Provenance

- Upstream: <https://github.com/shift-labs-ai/markit> (MIT license — see `LICENSE`)
- Pinned commit: `4197c76a651607421c5dcd05c0f94555ad1d6a9c` (2026-08-18)
- Vendored subtree: the upstream `rust/` directory only.
- Integration: path dependency from `polyvec-ingestion`
  (`markit = { path = "../../vendor/markit", default-features = false }`).
  Not a workspace member — it keeps its own lints and is built as a
  dependency, not a crate we own.
- Reproduction: `git clone https://github.com/shift-labs-ai/markit.git`
  then `git checkout 4197c76a651607421c5dcd05c0f94555ad1d6a9c`;
  copy `rust/` here and re-apply the patches below.

## Why vendored

markit's `Cargo.toml` lives in the `rust/` subdirectory, so it cannot be a
cargo git dependency. The Phase 1 crate audit (`docs/audit/crates-audit.md`,
§F) decided: ADAPT — vendor behind the `Converter` adapter trait defined in
`polyvec-ingestion`, so markit stays replaceable.

## Patches vs upstream (all in this directory)

### 1. HTTP fetching disabled (license-motivated)

- Removed the `ureq = "3.4.0"` dependency from `Cargo.toml`.
- Replaced `UreqFetcher` with `DisabledHttpFetcher` in
  `src/markit.rs` and `src/converters/github.rs`. Any network path
  (`convert_url`, `llms.txt` discovery, GitHub API fetch) now returns a
  loud error instead of reaching the network.

Rationale: Polyvec feeds bytes directly to `Markit::convert` — it never
needs URL fetching. Dropping `ureq` removes its TLS stack from the tree,
including `subtle` (BSD-3-Clause), exactly as the license audit anticipated
(`docs/audit/license-audit.md`, markit row: "Phase 4 call"). This keeps the
dependency tree closer to the MIT/Apache-2.0 policy without a `deny.toml`
exception for the ureq subtree.

### 2. Global allocator removed (behavior-motivated)

- Removed the `#[global_allocator] static GLOBAL: mimalloc::MiMalloc` from
  `src/lib.rs` and the `mimalloc = "0.1.52"` dependency from `Cargo.toml`.

Rationale: as a library dependency, markit's global allocator would
silently replace the allocator of every binary linking it (including
`polyvec-server`). Polyvec keeps the system allocator. This also drops a C
build dependency from the tree.

### 3. Workspace opt-out (build-motivated)

- Appended an empty `[workspace]` table to `Cargo.toml`.

Rationale: the vendored directory sits inside Polyvec's workspace tree but is
deliberately NOT a workspace member (it keeps its own lints and must not be
subject to `cargo fmt --check` / workspace clippy runs). The empty table
tells Cargo it is its own workspace root; as a path dependency of
`polyvec-ingestion` it still resolves inside Polyvec's `Cargo.lock`.

## What is NOT changed

Everything else is byte-identical to upstream at the pinned commit —
converters, the PDF pipeline, HTML→Markdown engine, ZIP handling. The
`Converter` adapter trait in `polyvec-ingestion` is the seam: swapping
markit for another backend means implementing that trait, not touching this
directory.
