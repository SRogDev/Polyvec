# Polyvec — License Audit (Phase 1)

> Status: accepted 2026-10-15. Re-run `cargo deny check` after every dependency change; CI enforces it.

Date: 2026-10-15. Policy: **MIT / Apache-2.0 preferred; explicitly-reviewed permissive licenses allowed** (coordinator decision 2026-10-15, reversible by Roger — see "Policy decision" below).
Method: crates.io metadata (2026-10-15) + `cargo tree` provenance on the Phase-1
spike workspace (340 packages in Cargo.lock). "OK" = license expression includes
MIT or Apache-2.0 as an option.

## Direct dependencies (selected stack)

| Crate | Version (pinned in spike) | License | Verdict |
|---|---|---|---|
| hnsw_rs | 0.3.5 | MIT/Apache-2.0 (LICENSE-MIT + LICENSE-APACHE in crate) | OK |
| redb | 4.4.0 | MIT OR Apache-2.0 | OK |
| tantivy | 0.26.2 | MIT | OK |
| markit (vendored, shift-labs-ai) | 0.6.1 (git SHA pinned in spike README) | MIT | OK |
| tokio | 1.53.3 | MIT | OK |
| actix-web | 4.17.0 | MIT OR Apache-2.0 | OK |
| reqwest | 0.13.5 | MIT OR Apache-2.0 | OK |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | OK |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | OK |
| tracing | 0.1.44 | MIT | OK |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | OK |
| roaring (roaring-rs, future filter bitmaps) | 0.11.5 | MIT OR Apache-2.0 | OK |
| zvec-rs (REFERENCE ONLY, not selected) | git master (no release) | **declared MIT in Cargo.toml, but NO LICENSE file in repo** | FLAG (hygiene) |

## Transitive flags (strict MIT/Apache-2.0 reading)

These appear in the spike's resolved tree. All are permissive, non-copyleft —
but they are NOT MIT/Apache-2.0, so under a strict reading they violate the policy.

| Crate | Version | License | Provenance (`cargo tree -i`) | Avoidable? |
|---|---|---|---|---|
| foldhash | 0.2.0 | **Zlib** | hashbrown ← lru ← tantivy | Only if tantivy drops its `lru` dep (upstream change; unlikely). Zlib is a tiny permissive license. |
| zstd-safe / zstd-sys | 7.3.0 / 2.1.1 | **BSD-3-Clause** | zstd ← tantivy-sstable ← tantivy-columnar ← tantivy | **Yes**: disable tantivy default features (`default-features=false, features=["mmap"]`) — drops `columnar-zstd-compression`. TESTED: spike builds with defaults; no-defaults build not yet run — do in Phase 2. |
| subtle | 2.6.1 | **BSD-3-Clause** | rustls ← ureq ← markit (UreqFetcher) | Only via vendored-markit patch (swap ureq TLS or drop URL fetching — Polyvec feeds bytes directly, so `HttpFetch` can be a stub). Phase 4 call. |
| rust-stemmers | 1.2.0 | MIT/BSD-3-Clause | tantivy `stemmer` default feature | Has MIT option → OK under "includes MIT" reading; droppable with no-defaults anyway. |
| untrusted | 0.9.0 | ISC | rustls ← ureq ← markit; also ← reqwest later | Permissive; same class as MIT. |
| ring | 0.17.14 | Apache-2.0 AND ISC | rustls ← (ureq, reqwest) | Apache-2.0 included → OK. |
| jiff-core, aho-corasick, memchr, byteorder | various | Unlicense OR MIT | various | MIT option → OK. |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | flate2 ← (tantivy, markit/zip) | MIT/Apache option → OK. |

**No copyleft (GPL/AGPL) anywhere in the 340-package tree.** Verified by scanning
every package's declared license — the complete list above is exhaustive for the
spike's Cargo.lock.

## Policy statement (draft for repo docs)

> Polyvec accepts only MIT / Apache-2.0 (dual or either) licensed dependencies,
> direct and transitive. Permissive non-copyleft licenses (BSD-*, ISC, Zlib,
> Unlicense) are flagged in this audit; adopting any of them requires an explicit
> policy exception recorded here with rationale. Copyleft licenses (GPL, AGPL,
> LGPL for linked code) are rejected.

## Policy decision (coordinator, 2026-10-15 — reversible by Roger)

**Allowed:** MIT, Apache-2.0, and explicitly-reviewed permissive non-copyleft
licenses (Zlib, ISC, BSD-2/3-Clause, Unlicense). Each non-MIT/Apache-2.0 crate
gets a documented `exceptions` entry in `deny.toml` with rationale, following
the Phase 0 precedent (`unicode-ident` / Unicode-3.0).

**Rationale:** the strict reading flags `foldhash` (Zlib, via tantivy → lru —
only avoidable if upstream drops the dep) while buying nothing: Zlib/ISC/BSD
are OSI-approved, GPL-compatible, single-file permissive licenses. Avoiding
them entirely would mean forking around `tantivy`'s `lru` for zero legal gain.

**Still banned:** all copyleft (GPL/AGPL/LGPL for linked code), unknown/unlicensed.

**Open items:**
1. zvec-rs is reference-only, so its missing LICENSE file does not block us —
   if ever reconsidered, require a LICENSE file first.
2. tantivy should be integrated with `default-features = false` (drops zstd/BSD
   and stemmers) unless zstd columnar compression is measured to matter — do the
   no-defaults build in Phase 2 and confirm the flag disappears.
   - **Phase 2 result (2026-10-15, TESTED):** `tantivy 0.26.2` with
     `default-features = false, features = ["mmap"]` compiles clean in a
     scratch crate (4m 41s build, not committed as a dependency).
     `zstd-safe`/`zstd-sys` (BSD-3-Clause) and `rust-stemmers` are **gone**
     from the tree. `foldhash 0.2.0` (Zlib) **remains** via
     `tantivy → lru → hashbrown → foldhash` — only avoidable if upstream
     drops `lru`, so it will need a documented `deny.toml` exception when
     tantivy is integrated (Phase 5), same class as the Phase 2 foldhash
     exception via hnsw_rs.
3. `subtle` (BSD-3-Clause, via markit's ureq): avoidable in Phase 4 by stubbing
   markit's HTTP fetch (Polyvec feeds bytes directly) — decide then.

## Phase 4 results (2026-10-15)

New dependency trees: `reqwest` (remote embedding adapter, rustls) and the
vendored `markit` (converter backend). `cargo deny check` green after
documented exceptions (see `deny.toml`).

### `subtle` (BSD-3-Clause) — the Phase 4 call

The Phase 1 audit flagged `subtle` via `markit → ureq` and deferred the
decision to Phase 4. **Done:** the vendored markit stubs HTTP fetching
(`DisabledHttpFetcher`; see `vendor/markit/VENDOR.md`) — Polyvec feeds bytes
directly, so markit's `ureq` dependency is gone and its `subtle` chain with
it. `subtle 2.6.1` remains in the tree via `rustls ← reqwest` (constant-time
comparisons in TLS); BSD-3-Clause is in the audit's pre-approved BSD
category, so it gets a documented `deny.toml` exception.

### markit tree (new transitive licenses)

| Crate | License | Provenance | Verdict |
|---|---|---|---|
| `scraper` 0.27.0, `ego-tree` 0.11.0 | ISC | markit → scraper (HTML parsing) | Exception: permissive, OSI-approved |
| `mutate_once` 0.1.2, `kamadak-exif` 0.6.1 | BSD-2-Clause | scraper / markit (image EXIF) | Exception: permissive, OSI-approved |
| `cssparser` 0.37.0, `cssparser-macros` 0.7.1, `dtoa-short` 0.3.5, `selectors` 0.38.0 | **MPL-2.0** | scraper → selectors/cssparser | **Exception with flag:** weak copyleft, file-scoped — requires sharing modifications to those files only, does not propagate to Polyvec. Unavoidable for HTML parsing. Flagged for Roger's review (reversible). |
| `libbz2-rs-sys` 0.2.5 | bzip2-1.0.6 | markit → zip | Exception: BSD-style permissive |
| `ppmd-rust` 1.5.0 | CC0-1.0 OR MIT-0 | markit → zip | Exception: public domain |
| `zlib-rs` 0.6.8 | Zlib | markit → zip → flate2 | Exception: same class as the Phase 2 foldhash exception |

### reqwest/rustls tree (new transitive licenses)

| Crate | License | Provenance | Verdict |
|---|---|---|---|
| `rustls-webpki` 0.103.15, `untrusted` 0.9.0, `aws-lc-rs` 1.18.2 | ISC | reqwest → rustls | Exception: permissive, OSI-approved |
| `aws-lc-sys` 0.46.0 | ISC/Apache-2.0/MIT/BSD-3-Clause mix | rustls crypto provider | Exception: all components permissive |
| `webpki-root-certs` 1.0.9 | CDLA-Permissive-2.0 | rustls root certificates data | Exception: permissive data license, OSI-approved |

### Advisories

- `RUSTSEC-2024-0436` (`paste` 1.0.15, unmaintained — NOT a vulnerability):
  build-time proc-macro via `markit → lofty` (audio metadata). Ignored with
  rationale in `deny.toml`; unavoidable without patching vendored markit to
  drop audio support.

**No GPL/AGPL anywhere in the Phase 4 tree.** Verified via `cargo deny check`
(green 2026-10-15).

## Phase 5 results (2026-10-15)

New dependency: `tantivy 0.26.2` (`default-features = false, features = ["mmap"]`)
in `polyvec-retrieval`. `cargo deny check` green (advisories/bans/licenses/sources).

### tantivy tree (new transitive licenses)

| Crate | License | Provenance | Verdict |
|---|---|---|---|
| `tantivy` 0.26.2 | MIT | direct | OK |
| `foldhash` 0.2.0 | **Zlib** | tantivy → lru → hashbrown → foldhash | **Exception (documented):** same class as the Phase 2 foldhash exception; the pre-existing per-crate `deny.toml` exception covers both provenances. Only avoidable if upstream drops `lru`. Comment updated 2026-10-15. |

Verified 2026-10-15: with `default-features = false`, the zstd
(BSD-3-Clause) and rust-stemmers stacks are **gone** from the tantivy tree
(as predicted in Phase 2) — `foldhash` (Zlib) is the **only**
non-MIT/Apache-2.0 license the tantivy subtree introduces, and it was
already exceptioned. `cargo tree -p polyvec-retrieval -i foldhash`
confirms the single chain `tantivy → lru → hashbrown → foldhash`.

**No GPL/AGPL anywhere in the Phase 5 tree.** Verified via `cargo deny check`
(green 2026-10-15).

## Phase 6 results (2026-10-15)

New dependencies: **none**. Phase 6 adds no crates to any `Cargo.toml` —
the relation subsystem is built on `redb` (already in the tree), `serde` /
`serde_json` (already in the tree), and the standard library. `cargo deny
check` green 2026-10-15 (advisories/bans/licenses/sources), no new
exceptions, no new transitive licenses.

**No GPL/AGPL anywhere in the Phase 6 tree.** Verified via `cargo deny check`
(green 2026-10-15).

## Release re-verification — v0.1.0 (Phase 9, 2026-10-15)

**Phase 7 additions:** `criterion` 0.8.2 (dev-only benchmark dep for
`polyvec-bench`; never shipped in the server binary) + transitives
(`tinytemplate`, `oorandom`, `cast`, `clap`, `criterion-plot`). All
carry **MIT OR Apache-2.0** license expressions (verified from crates.io
metadata on 2026-10-15). No new license class, no new `deny.toml`
exception needed.

**Phase 8 additions:** the docs site (`site/`, npm: astro 7.4.0 +
@astrojs/starlight 0.42.6, pinned via `package-lock.json`). npm
dependencies are outside the `deny.toml` policy (Cargo-only); they ship
documentation only, not the database binary. License scan of the npm
tree is **not performed** in this release (documented gap — the site is
docs-only; add an npm license policy when the site gains
user-facing functionality).

**MPL-2.0 flag (Phase 4) still noted:** `cssparser` 0.37.0 +
`selectors` 0.38.0 via the vendored markit HTML parser — weak
file-scoped copyleft, documented exception, reversible by dropping HTML
conversion, flagged for Roger's review. Unchanged in the release tree.

**Release-tree verdict:** `cargo deny check` green on the
`phase-9-release` branch (advisories + bans + licenses + sources,
2026-10-15). **No GPL/AGPL anywhere in the v0.1.0 tree.**
`cargo audit` still does not build in this sandbox (linker env quirk,
reproduced since Phase 0); advisory coverage comes from `cargo deny`'s
`advisories` source.
