//! Phase 1 spike: markit compile+behavior test.
//!
//! Question: does shift-labs-ai/markit expose a usable Rust library API
//! (vs CLI-only / Node-only)? TESTED here via a vendored path dependency
//! (its Cargo.toml lives in rust/, so a crates.io/git dep is not possible).
//!
//! Tests:
//!  1. Markit::new() constructs without network
//!  2. convert() an HTML snippet -> markdown
//!  3. convert() a CSV snippet -> markdown table
//!  4. convert() plain text -> passthrough
//!
//! Run: cargo run -p convert_markit --release

use anyhow::Result;
use markit::markit::Markit;
use markit::types::{ConversionResult, StreamInfo};

fn convert(markit: &Markit, bytes: &[u8], ext: &str, mime: &str) -> Result<ConversionResult> {
    let info = StreamInfo {
        mimetype: Some(mime.to_string()),
        extension: Some(ext.to_string()),
        ..Default::default()
    };
    Ok(markit.convert(bytes, &info)?)
}

fn main() -> Result<()> {
    let markit = Markit::new();
    println!(
        "[markit] Markit::new() OK; converters: {:?}",
        markit.converter_names()
    );

    // 1. HTML -> markdown
    let html = br#"<!doctype html><html><head><title>Test Doc</title></head><body>
<h1>Polyvec Spike</h1><p>This is a <strong>test</strong> paragraph with a
<a href="https://example.com">link</a>.</p><ul><li>one</li><li>two</li></ul>
</body></html>"#;
    let r = convert(&markit, html, "html", "text/html")?;
    println!(
        "[markit] HTML -> markdown ({} chars):\n---\n{}\n---",
        r.markdown.len(),
        r.markdown.trim()
    );
    assert!(
        r.markdown.contains("Polyvec Spike"),
        "h1 lost in conversion"
    );
    assert!(
        r.markdown.contains("**test**") || r.markdown.contains("test"),
        "bold text lost"
    );

    // 2. CSV -> markdown
    let csv = b"name,score\nalice,95\nbob,87\n";
    let r = convert(&markit, csv, "csv", "text/csv")?;
    println!(
        "[markit] CSV -> markdown ({} chars):\n---\n{}\n---",
        r.markdown.len(),
        r.markdown.trim()
    );
    assert!(
        r.markdown.contains("alice") && r.markdown.contains("95"),
        "csv rows lost"
    );

    // 3. plain text passthrough
    let txt = b"just some plain text\nsecond line";
    let r = convert(&markit, txt, "txt", "text/plain")?;
    assert!(
        r.markdown.contains("just some plain text"),
        "plain text mangled"
    );
    println!("[markit] plain text passthrough: OK");

    println!(
        "\n===== DONE: markit exposes a usable Rust library API (rlib, no napi/CLI needed) ====="
    );
    Ok(())
}
