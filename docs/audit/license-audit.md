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
3. `subtle` (BSD-3-Clause, via markit's ureq): avoidable in Phase 4 by stubbing
   markit's HTTP fetch (Polyvec feeds bytes directly) — decide then.
