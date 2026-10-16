---
title: Installation
description: Build Polyvec from source and run the server.
---

There are no prebuilt binaries or tagged releases yet — Polyvec installs by
building from source with Cargo. (Releases arrive with Phase 9; see
[Limitations & roadmap](/project/limitations/).)

## Prerequisites

- **Rust 1.98.1** (the pinned toolchain; `rustup` recommended).
- A C toolchain (`cc`) — the vendored `markit` converter and tantivy build
  native code.
- ~2 GB free disk for build artifacts on first compile.

```bash
rustc --version   # rustc 1.98.1
```

## Build

```bash
git clone https://github.com/SRogDev/Polyvec.git
cd Polyvec
cargo build --release -p polyvec-server
```

The server binary lands at `target/release/polyvec-server`.
A debug build (`cargo build -p polyvec-server`) is fine for trying things
out; quote only release-profile numbers (see
[Performance](/operations/performance/)).

## Run

```bash
./target/release/polyvec-server
```

Defaults: binds `127.0.0.1:8080`, stores data in `./polyvec-data`, no
embedding provider configured (`none`). Verify it's alive:

```bash
curl localhost:8080/healthz   # {"status":"ok","service":"polyvec","version":"0.1.0"}
curl localhost:8080/readyz    # {"status":"ready"}
```

Then continue to the [Quickstart](/quickstart/). All runtime knobs are in
[Configuration & env vars](/configuration/).

## Verify your checkout (optional)

```bash
cargo test --workspace        # 244 tests green at Phase 7
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Note: `cargo fmt --check` without `--all` — `--all` descends into the
pinned `vendor/markit` tree and must never be committed reformatted.

## Docker / containers

There is no official container image yet. `cargo audit` coverage is via
`cargo deny check` in this environment; see [Contributing](/project/contributing/).
