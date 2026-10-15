# Polyvec benchmark fixture

This is a small Markdown fixture for the Phase 7 benchmark harness
(conversion success by format, per-document ingest timing).

## Vector databases

A vector database stores embeddings — dense numeric representations of
content — and retrieves them by similarity. Polyvec is a from-scratch
vector database engine written in Rust.

## Design notes

- Reads are the hot path; every choice is scored against read cost.
- The catalog (redb) is the source of truth; the ANN index is a
  checksummed, rebuildable cache.

```rust
fn main() {
    println!("hello, bench");
}
```

| engine | language |
|--------|----------|
| polyvec | rust |
