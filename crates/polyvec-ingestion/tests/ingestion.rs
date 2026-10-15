//! Converter, canonicalization and chunking tests.
//!
//! RED: defines the intended API of `polyvec-ingestion`; fails to compile
//! until the modules are implemented.

use polyvec_core::{ChunkingConfig, ContentHash};
use polyvec_ingestion::{
    CanonicalDocument, Chunker, ConversionInfo, ConvertedDocument, Converter, MarkitConverter,
    SourceHint, canonicalize, normalize_markdown,
};

// ---------------------------------------------------------------------------
// converter adapter
// ---------------------------------------------------------------------------

fn html_hint() -> SourceHint {
    SourceHint {
        filename: Some("guide.html".to_string()),
        mime: Some("text/html".to_string()),
        extension: Some("html".to_string()),
    }
}

#[test]
fn markit_converts_html_to_markdown() {
    let c = MarkitConverter::new();
    let html = br#"<!doctype html><html><head><title>Guide</title></head><body>
<h1>Getting Started</h1><p>Install with <code>cargo install</code> and read the
<a href="https://example.com/docs">docs</a>.</p><ul><li>fast</li><li>cheap</li></ul>
</body></html>"#;
    let out = c.convert(html, &html_hint()).unwrap();
    assert!(out.markdown.contains("Getting Started"), "h1 lost:\n{}", out.markdown);
    assert!(out.markdown.contains("cargo install"), "code lost:\n{}", out.markdown);
    assert!(out.markdown.contains("https://example.com/docs"), "link lost:\n{}", out.markdown);
    assert_eq!(out.conversion.converter, "markit");
    assert!(out.conversion.source_format.contains("html"), "format: {}", out.conversion.source_format);
}

#[test]
fn markit_converts_csv_to_markdown_table() {
    let c = MarkitConverter::new();
    let csv = b"name,score\nalice,95\nbob,87\n";
    let hint = SourceHint {
        extension: Some("csv".to_string()),
        mime: Some("text/csv".to_string()),
        ..Default::default()
    };
    let out = c.convert(csv, &hint).unwrap();
    assert!(out.markdown.contains("alice") && out.markdown.contains("95"), "rows lost:\n{}", out.markdown);
    assert!(out.markdown.contains("name"), "header lost:\n{}", out.markdown);
}

#[test]
fn markit_passes_plain_text_through() {
    let c = MarkitConverter::new();
    let txt = b"just some plain text\nsecond line";
    let hint = SourceHint {
        extension: Some("txt".to_string()),
        ..Default::default()
    };
    let out = c.convert(txt, &hint).unwrap();
    assert!(out.markdown.contains("just some plain text"), "mangled:\n{}", out.markdown);
}

#[test]
fn markit_passes_markdown_through() {
    let c = MarkitConverter::new();
    let md = b"# Title\n\nSome *markdown* content.\n";
    let hint = SourceHint {
        extension: Some(".md".to_string()),
        ..Default::default()
    };
    let out = c.convert(md, &hint).unwrap();
    assert!(out.markdown.contains("# Title"), "heading lost:\n{}", out.markdown);
    assert!(out.markdown.contains("*markdown*"), "emphasis lost:\n{}", out.markdown);
}

#[test]
fn converter_rejects_empty_input() {
    let c = MarkitConverter::new();
    assert!(c.convert(b"", &html_hint()).is_err());
}

#[test]
fn converter_is_behind_a_replaceable_trait() {
    // The trait is the seam: a test double proves markit is swappable.
    struct Upper;
    impl Converter for Upper {
        fn name(&self) -> &'static str {
            "upper-test-double"
        }
        fn convert(
            &self,
            bytes: &[u8],
            _hint: &SourceHint,
        ) -> Result<ConvertedDocument, polyvec_core::Error> {
            Ok(ConvertedDocument {
                markdown: String::from_utf8_lossy(bytes).to_uppercase(),
                title: None,
                conversion: ConversionInfo {
                    converter: self.name().to_string(),
                    converter_version: "test".to_string(),
                    source_format: "test".to_string(),
                },
            })
        }
    }
    let out = Upper.convert(b"hi", &SourceHint::default()).unwrap();
    assert_eq!(out.markdown, "HI");
    assert_eq!(out.conversion.converter, "upper-test-double");
}

// ---------------------------------------------------------------------------
// canonicalization
// ---------------------------------------------------------------------------

#[test]
fn normalize_markdown_is_deterministic() {
    let messy = "# Title  \r\n\r\n\r\nbody with trailing space   \n";
    let clean = normalize_markdown(messy);
    assert_eq!(clean, "# Title\n\nbody with trailing space\n");
    assert_eq!(normalize_markdown(&clean), clean, "normalization is idempotent");
    assert_eq!(normalize_markdown(messy), clean, "normalization is deterministic");
}

#[test]
fn canonicalize_preserves_attribution_and_hashes() {
    let converted = ConvertedDocument {
        markdown: "# T\n\nbody\n".to_string(),
        title: Some("T".to_string()),
        conversion: ConversionInfo {
            converter: "markit".to_string(),
            converter_version: "0.6.1+4197c76a".to_string(),
            source_format: "html".to_string(),
        },
    };
    let raw = b"<h1>T</h1><p>body</p>";
    let doc: CanonicalDocument = canonicalize(
        converted,
        "https://example.com/guide",
        Some("guide.html"),
        raw,
    );
    assert_eq!(doc.source, "https://example.com/guide");
    assert_eq!(doc.filename.as_deref(), Some("guide.html"));
    assert_eq!(doc.source_format, "html");
    assert_eq!(doc.original_hash, ContentHash::of(raw));
    assert_eq!(doc.canonical_hash, ContentHash::of(doc.markdown.as_bytes()));
    assert_ne!(doc.original_hash, doc.canonical_hash);
    assert!(doc.converted_at_secs > 0);
}

// ---------------------------------------------------------------------------
// chunker
// ---------------------------------------------------------------------------

fn chunker() -> Chunker {
    Chunker::new(ChunkingConfig {
        target_chars: 120,
        overlap_chars: 20,
        version: 1,
    })
    .unwrap()
}

#[test]
fn chunker_splits_on_sections_not_mid_section() {
    let md = "# Alpha\n\naaaa bbbb cccc\n\n# Beta\n\ndddd eeee ffff\n";
    let chunks = chunker().chunk(md);
    assert!(chunks.len() >= 2, "expected section split, got {}", chunks.len());
    // No chunk should contain content from both sections.
    for ch in &chunks {
        assert!(
            !(ch.text.contains("aaaa") && ch.text.contains("dddd")),
            "chunk spans two sections:\n{}",
            ch.text
        );
    }
}

#[test]
fn chunker_hard_splits_oversized_sections_with_overlap() {
    let long = "x".repeat(500);
    let md = format!("# Big\n\n{long}\n");
    let chunks = chunker().chunk(&md);
    assert!(chunks.len() >= 2, "oversized section must hard-split");
    for ch in &chunks {
        assert!(ch.text.chars().count() <= 120, "chunk exceeds target");
    }
    // Overlap: the tail of chunk[i] is the head of chunk[i+1].
    let a: Vec<char> = chunks[0].text.chars().collect();
    let b: Vec<char> = chunks[1].text.chars().collect();
    let overlap = 20;
    assert_eq!(
        &a[a.len() - overlap..],
        &b[..overlap],
        "consecutive hard-split chunks must overlap"
    );
}

#[test]
fn chunker_offsets_slice_the_source() {
    let md = "# A\n\nhello world\n\n# B\n\nbye world\n";
    let chars: Vec<char> = md.chars().collect();
    for ch in chunker().chunk(md) {
        let sliced: String = chars[ch.char_start..ch.char_end].iter().collect();
        assert_eq!(sliced, ch.text, "offsets must slice the source exactly");
    }
}

#[test]
fn chunker_is_deterministic_and_rejects_empty() {
    let md = "# A\n\nsome content here\n";
    assert_eq!(chunker().chunk(md), chunker().chunk(md));
    assert!(chunker().chunk("").is_empty());
    assert!(chunker().chunk("   \n  ").is_empty());
}

#[test]
fn chunker_validates_config() {
    assert!(Chunker::new(ChunkingConfig::default()).is_ok());
    assert!(
        Chunker::new(ChunkingConfig {
            target_chars: 10,
            ..Default::default()
        })
        .is_err()
    );
}
