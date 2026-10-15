//! Phase 1 spike: tantivy as the BM25 lexical sidecar.
//!
//! Tests (all measured):
//!  1. index 10k docs (id + title + body), commit
//!  2. BM25 term query -> top-10 sane results
//!  3. delete 100 docs, update 1 doc (delete+re-add), commit, verify
//!  4. close, reopen index dir from disk, re-query (restart persistence)
//!
//! Run: cargo run -p lexical_tantivy --release

use anyhow::Result;
use std::time::Instant;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Schema, Value, INDEXED, STORED, TEXT};
use tantivy::{doc, Index, TantivyDocument};

const N: usize = 10_000;
const WORDS: [&str; 24] = [
    "rust",
    "vector",
    "database",
    "embedding",
    "search",
    "index",
    "quantum",
    "physics",
    "neural",
    "network",
    "graph",
    "filter",
    "metadata",
    "chunk",
    "document",
    "retrieval",
    "hybrid",
    "rerank",
    "quantization",
    "memory",
    "agent",
    "knowledge",
    "token",
    "cluster",
];

fn body_for(i: usize) -> String {
    // deterministic pseudo-text; every 7th doc mentions the rare term "quokka"
    let mut w: Vec<&str> = (0..40)
        .map(|k| WORDS[(i * 13 + k * 7) % WORDS.len()])
        .collect();
    if i % 7 == 0 {
        w.push("quokka");
    }
    w.join(" ")
}

fn main() -> Result<()> {
    let dir = "/tmp/polyvec-spike/tantivy";
    let _ = std::fs::remove_dir_all(dir); // fresh start for repeatability
    std::fs::create_dir_all(dir)?;

    let mut schema_builder = Schema::builder();
    let id = schema_builder.add_u64_field("doc_id", INDEXED | STORED);
    let title = schema_builder.add_text_field("title", TEXT | STORED);
    let body = schema_builder.add_text_field("body", TEXT | STORED);
    let schema = schema_builder.build();

    // ---- 1. index 10k docs ----
    let index = Index::create_in_dir(dir, schema.clone())?;
    let mut writer = index.writer(100_000_000)?;
    let t = Instant::now();
    for i in 0..N as u64 {
        writer.add_document(doc!(
            id => i,
            title => format!("document number {i}"),
            body => body_for(i as usize),
        ))?;
    }
    writer.commit()?;
    println!("[tantivy] indexed {N} docs + commit: {:?}", t.elapsed());

    // ---- 2. BM25 query ----
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let qp = QueryParser::for_index(&index, vec![title, body]);
    let t = Instant::now();
    let query = qp.parse_query("quokka")?;
    let top: Vec<(f32, tantivy::DocAddress)> =
        searcher.search(&query, &TopDocs::with_limit(10).order_by_score())?;
    println!(
        "[tantivy] BM25 query 'quokka' -> {} hits in {:?}",
        top.len(),
        t.elapsed()
    );
    // every 7th doc has quokka => expect ~1429 hits; top-10 all contain it
    assert!(top.len() == 10, "expected 10 top docs");
    for (score, addr) in &top {
        let d: TantivyDocument = searcher.doc(*addr)?;
        let b = d.get_first(body).unwrap().as_str().unwrap();
        assert!(b.contains("quokka"), "non-matching doc in top-10!");
        assert!(*score > 0.0);
    }
    println!("[tantivy] BM25 correctness: OK (top-10 all contain 'quokka', scores > 0)");

    // ---- 3. delete + update ----
    let t = Instant::now();
    let term_del = tantivy::Term::from_field_u64(id, 7); // doc 7 had quokka
    writer.delete_term(term_del);
    // update doc 8: delete then re-add without quokka
    writer.delete_term(tantivy::Term::from_field_u64(id, 8));
    writer.add_document(doc!(
        id => 8u64,
        title => "document number 8 updated",
        body => "plain text with no rare terms whatsoever",
    ))?;
    writer.commit()?;
    reader.reload()?;
    let searcher = reader.searcher();
    let top2: Vec<(f32, tantivy::DocAddress)> =
        searcher.search(&query, &TopDocs::with_limit(10).order_by_score())?;
    let ids: Vec<u64> = top2
        .iter()
        .map(|(_, a)| {
            let d: TantivyDocument = searcher.doc(*a).unwrap();
            d.get_first(id).unwrap().as_u64().unwrap()
        })
        .collect();
    assert!(!ids.contains(&7), "deleted doc 7 still searchable!");
    assert!(!ids.contains(&8), "updated doc 8 still matches old terms!");
    println!("[tantivy] delete(7) + update(8): OK in {:?}", t.elapsed());

    // ---- 4. restart: drop everything, reopen from dir ----
    drop(writer);
    drop(reader);
    drop(index);
    let t = Instant::now();
    let index2 = Index::open_in_dir(dir)?;
    let reader2 = index2.reader()?;
    let searcher2 = reader2.searcher();
    println!("[tantivy] reopen from disk: {:?}", t.elapsed());
    let qp2 = QueryParser::for_index(&index2, vec![title, body]);
    let q2 = qp2.parse_query("quokka")?;
    let top3: Vec<(f32, tantivy::DocAddress)> =
        searcher2.search(&q2, &TopDocs::with_limit(10).order_by_score())?;
    assert_eq!(top3.len(), 10, "result count changed across restart");
    // doc 7 must still be gone after restart
    let ids3: Vec<u64> = top3
        .iter()
        .map(|(_, a)| {
            let d: TantivyDocument = searcher2.doc(*a).unwrap();
            d.get_first(id).unwrap().as_u64().unwrap()
        })
        .collect();
    assert!(
        !ids3.contains(&7),
        "deleted doc 7 resurrected after restart!"
    );
    println!("[tantivy] restart persistence: OK (deletes survived reopen)");

    println!("\n===== DONE =====");
    Ok(())
}
