# Phase 1 Spike — Polyvec critical-crate audit

Executable evidence for `docs/audit/crates-audit.md`. All numbers MEASURED on this
machine (rustc 1.98.1, release profile); nothing here is a README claim.

## Layout

| Crate | What it tests |
|---|---|
| `crates/ann_compare` | hnsw_rs vs zvec-rs head-to-head: 10k×128f32 unit vectors (fixed seed), recall@10 vs brute-force exact ground truth (100 queries), dump→reopen→re-query, update, delete, zvec filter |
| `crates/ann_compare/src/bin/diag.rs` | hnsw_rs param sweep (M, ef_construction, ef_search) + DistL2 sanity, N=2000 |
| `crates/ann_compare/src/bin/zdiag.rs` | zvec-rs per-metric diagnostic (Cosine/L2/IP), N=2000 |
| `crates/store_redb` | redb 4.4.0: 10k blobs in 1 txn, close→reopen durability, update, delete, uncommitted-txn rollback |
| `crates/lexical_tantivy` | tantivy 0.26.2: 10k docs, BM25 correctness, term delete + update, reopen persistence |
| `crates/convert_markit` | markit (vendored `vendor/markit-src`, commit `4197c76a` (2026-08-18) — pin this SHA when vendoring): Rust lib API usability |
| `vendor/markit-src` | NOT committed here (see below) — shallow clone of `https://github.com/shift-labs-ai/markit` @ 2026-10-15 (for path dep; its `Cargo.toml` lives in `rust/`, so a cargo git dep is impossible) |

## Commands

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd docs/audit/spike-phase1
cargo run --release -p ann_compare        # ~90s: the head-to-head
cargo run --release -p store_redb         # ~10s
cargo run --release -p lexical_tantivy    # ~60s (first build compiles tantivy)
cargo run --release -p convert_markit     # ~10s after first build
# diagnostics (optional):
./target/release/diag    # hnsw_rs ef sweep
./target/release/zdiag   # zvec-rs metric diagnostic
```

Data dirs: `/tmp/polyvec-spike/{hnsw,zvec,store.redb,tantivy}` (recreated each run,
except `ann_compare` reuses the zvec dir — see the reopen-panic finding).

## Results (measured 2026-10-15)

### ANN head-to-head — recall@10 vs brute force (N=10k, dim=128, 100 queries)

| Engine | recall@10 | Insert 10k | Query | Restart | Update | Delete |
|---|---|---|---|---|---|---|
| hnsw_rs 0.3.5 (M=16, efC=200, ef=200, DistCosine) | **0.9700** | 31–45s (parallel) | 5.1 ms/q | recall identical (asserted) | n/a (no API) | **no API** — tombstones+rebuild needed |
| zvec-rs master (Cosine) | **0.0160** | 35.9s | 0.02 ms/q | doc_count preserved | OK | OK (observable) |

zvec-rs per-metric diagnostic (N=2000): Cosine 0.072 / IP 0.070 / **L2 0.810**.
Exact-vector query returns the WRONG doc at rank 1 for Cosine/IP (doc-1515/doc-370
instead of doc-42) — the HNSW search path mishandles "higher is better" metrics.
**Additionally, reopening a flushed zvec-rs collection and inserting more docs
panics** (`hnsw/graph.rs:273` index-out-of-bounds on level bookkeeping) — TESTED,
twice reproducible (fresh dir works, reopened dir panics on first upsert).

### redb 4.4.0

- 10k × 512B blobs + 3 meta rows, single txn: **141.8 ms**
- reopen: **124 µs**; count + 100 deterministic spot-checks identical after restart
- update(42) + delete(7) visible; count 9999; uncommitted txn rolled back (no leak)
- API note: v4 moved `len()` to `ReadableTableMetadata`; `begin_read` needs `ReadableDatabase` in scope

### tantivy 0.26.2

- 10k docs indexed + commit: **248 ms**; BM25 "quokka" top-10 in **126 µs**, all hits contain the term
- delete-by-term + update (delete+re-add): **18 ms**, verified absent
- reopen from dir: **451 µs**; deletes survived restart
- API notes: `TopDocs::with_limit(n)` needs `.order_by_score()` to become a `Collector`;
  doc values via `tantivy::schema::Value` trait (`as_str`/`as_u64`);
  **the id field must be `INDEXED`** — a `STORED`-only field makes `delete_term` silently no-op

### markit (shift-labs-ai, vendored)

- `markit = { path = "vendor/markit-src/rust", default-features = false }` compiles as rlib
  (napi bindings are feature-gated; `cli` default feature deselected)
- `Markit::new()` constructs offline; 19 converters registered
- HTML → markdown (h1/bold/link/list preserved), CSV → markdown table, plain text passthrough — all asserted OK
- Verdict: **usable Rust library API** (not CLI-only). Integration caveat: subdir layout forces vendoring.

## Key findings for the audit

1. hnsw_rs: healthy (recall 0.97 @ ef=200, restart-stable, 0 unsafe) but has no delete API.
2. zvec-rs: TESTED incorrect ANN for Cosine/IP + panic on reopen-insert → reference only.
3. redb: solid transactional catalog store; sub-ms reopen.
4. tantivy: solid BM25 sidecar; term-deletes need indexed id field.
5. markit: real Rust API; vendor the `rust/` subdir behind an adapter trait.

## Reproducing `convert_markit` (vendored markit)

The `vendor/markit-src` directory is intentionally NOT committed (keeps this
evidence small). To re-run the markit compile-test:

```bash
cd docs/audit/spike-phase1
git clone --depth 1 https://github.com/shift-labs-ai/markit vendor/markit-src
# pin the audited commit:
git -C vendor/markit-src fetch --depth 1 origin 4197c76a && git -C vendor/markit-src checkout 4197c76a
cargo run --release -p convert_markit
```

The audit conclusion (Rust library API is real and usable, vendoring required
because `Cargo.toml` lives in `rust/`) was TESTED against commit `4197c76a`.
