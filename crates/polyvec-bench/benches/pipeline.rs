//! Layer B micro-benchmarks: chunking, canonicalization, stub embedding.
//!
//! These measure the per-unit cost of the pipeline stages; end-to-end
//! per-document numbers live in the `bench_report` binary.

use criterion::{Criterion, criterion_group, criterion_main};
use polyvec_bench::fixtures::synthetic_corpus;
use polyvec_core::ChunkingConfig;
use polyvec_embeddings::{EmbeddingProvider, StubEmbeddingProvider};
use polyvec_ingestion::{Chunker, ConversionInfo, ConvertedDocument, canonicalize};

/// A converted document of roughly `target_chars` characters.
fn converted_doc(target_chars: usize) -> ConvertedDocument {
    let corpus = synthetic_corpus(4, 0xD0C);
    let mut md = String::new();
    for (_, text) in &corpus {
        md.push_str(text);
        md.push('\n');
        if md.len() >= target_chars {
            break;
        }
    }
    md.truncate(target_chars);
    ConvertedDocument {
        markdown: md,
        title: Some("bench doc".to_string()),
        conversion: ConversionInfo {
            converter: "bench".to_string(),
            converter_version: "0".to_string(),
            source_format: "md".to_string(),
        },
    }
}

fn chunk_bench(c: &mut Criterion) {
    let chunker = Chunker::new(ChunkingConfig::default()).unwrap();
    let doc = converted_doc(8_000);
    let canon = canonicalize(doc, "bench://doc", Some("doc.md"), b"bench-bytes");
    c.bench_function("chunk_8k_chars", |b| {
        b.iter(|| chunker.chunk(&canon.markdown));
    });
}

fn canonicalize_bench(c: &mut Criterion) {
    let doc = converted_doc(8_000);
    c.bench_function("canonicalize_8k_chars", |b| {
        b.iter(|| canonicalize(doc.clone(), "bench://doc", Some("doc.md"), b"bench-bytes"));
    });
}

fn stub_embed_bench(c: &mut Criterion) {
    let provider = StubEmbeddingProvider::new(64).unwrap();
    let texts: Vec<String> = (0..32)
        .map(|i| format!("benchmark text number {i}"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    c.bench_function("stub_embed_batch_32_dim64", |b| {
        b.iter(|| provider.embed(&refs).unwrap());
    });
}

criterion_group!(benches, chunk_bench, canonicalize_bench, stub_embed_bench);
criterion_main!(benches);
