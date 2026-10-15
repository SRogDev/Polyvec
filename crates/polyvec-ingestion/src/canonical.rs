//! Canonical document model.
//!
//! [`canonicalize`] turns a [`ConvertedDocument`](crate::converter::ConvertedDocument)
//! into a [`CanonicalDocument`]: deterministically normalized markdown plus
//! everything needed to attribute and version it — source identity, original
//! filename and format, conversion metadata, stable ids, original + canonical
//! hashes, timestamps. No LLM rewriting anywhere: normalization is pure
//! string surgery, so the canonical form is byte-stable for identical inputs.

use std::time::{SystemTime, UNIX_EPOCH};

use polyvec_core::ContentHash;

use crate::converter::{ConversionInfo, ConvertedDocument};

/// A document in Polyvec's canonical form: normalized markdown plus full
/// provenance. Built by [`canonicalize`]; the ingestion pipeline chunks and
/// embeds from this.
#[derive(Clone, Debug)]
pub struct CanonicalDocument {
    /// Durable source identity (URI, file path, external key). The document
    /// id is derived from (collection, source).
    pub source: String,
    /// Original filename, if known.
    pub filename: Option<String>,
    /// Best-effort source format label, e.g. `"html"`.
    pub source_format: String,
    /// Title extracted by the converter, if any.
    pub title: Option<String>,
    /// Deterministically normalized markdown ([`normalize_markdown`]).
    pub markdown: String,
    /// SHA-256 of the raw input bytes (change detection on the source).
    pub original_hash: ContentHash,
    /// SHA-256 of [`CanonicalDocument::markdown`] (change detection on the
    /// canonical form; whitespace-only edits do not bump the version).
    pub canonical_hash: ContentHash,
    /// Unix seconds when the conversion ran.
    pub converted_at_secs: u64,
    /// How the conversion happened (converter name/version).
    pub conversion: ConversionInfo,
}

/// Deterministically normalizes markdown: `\r\n`/`\r` → `\n`, trailing
/// whitespace stripped per line, runs of 3+ newlines collapsed to two,
/// leading blank lines dropped, exactly one trailing newline.
///
/// Pure string surgery — no LLM, no semantics — so identical inputs always
/// produce identical output (idempotent and deterministic).
#[must_use]
pub fn normalize_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut blank_run = 0usize;
    let mut started = false;
    for raw_line in input.split('\n') {
        // Split on \n leaves \r at line ends for \r\n and lone-\r inputs.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            continue;
        }
        if !started {
            started = true;
        } else {
            // At most one blank line (two \n) between content lines.
            let blanks = blank_run.min(1);
            for _ in 0..=blanks {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(trimmed);
    }
    if started {
        out.push('\n');
    }
    out
}

/// Builds the canonical document from a conversion result.
///
/// `source` is the durable source identity; `filename` the original name if
/// known; `original_bytes` the raw input (for the original hash).
#[must_use]
pub fn canonicalize(
    converted: ConvertedDocument,
    source: &str,
    filename: Option<&str>,
    original_bytes: &[u8],
) -> CanonicalDocument {
    let markdown = normalize_markdown(&converted.markdown);
    let canonical_hash = ContentHash::of(markdown.as_bytes());
    CanonicalDocument {
        source: source.to_string(),
        filename: filename.map(str::to_string),
        source_format: converted.conversion.source_format.clone(),
        title: converted.title,
        markdown,
        original_hash: ContentHash::of(original_bytes),
        canonical_hash,
        converted_at_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        conversion: converted.conversion,
    }
}
