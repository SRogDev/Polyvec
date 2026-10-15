//! Converter adapter: the seam between Polyvec and document-conversion backends.
//!
//! [`Converter`] is a small trait — `bytes + hint → markdown` — so the
//! vendored markit backend stays replaceable. [`MarkitConverter`] is the
//! Phase 4 implementation; a test double in `tests/ingestion.rs` proves the
//! seam works.

use polyvec_core::{Error, Result};

/// Format hint for the source bytes. Everything is optional: with no hint
/// markit falls back to plain-text decoding.
#[derive(Clone, Debug, Default)]
pub struct SourceHint {
    /// Original filename, e.g. `"guide.html"`.
    pub filename: Option<String>,
    /// MIME type, e.g. `"text/html"`.
    pub mime: Option<String>,
    /// File extension with or without the leading dot, e.g. `"html"`.
    /// Normalized to markit's dotted form internally.
    pub extension: Option<String>,
}

/// Metadata about a conversion run. Preserved on the canonical document —
/// source attribution is never discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversionInfo {
    /// Which converter ran, e.g. `"markit"`.
    pub converter: String,
    /// Converter version + source pin, e.g. `"0.6.1+4197c76a"`.
    pub converter_version: String,
    /// Best-effort source format label derived from the hint, e.g. `"html"`.
    pub source_format: String,
}

/// A converted document: markdown plus conversion metadata.
#[derive(Clone, Debug)]
pub struct ConvertedDocument {
    /// Converted markdown (not yet normalized — see [`crate::canonical`]).
    pub markdown: String,
    /// Title extracted by the converter, if any.
    pub title: Option<String>,
    /// How the conversion happened.
    pub conversion: ConversionInfo,
}

/// Converts source bytes into markdown.
///
/// The trait — not markit — is the contract the pipeline programs against.
pub trait Converter: Send + Sync {
    /// Short name for logs and conversion metadata.
    fn name(&self) -> &'static str;

    /// Converts `bytes` to markdown, guided by `hint`.
    ///
    /// # Errors
    /// [`Error::Validation`] for empty input; [`Error::Provider`] when the
    /// backend fails to convert.
    fn convert(&self, bytes: &[u8], hint: &SourceHint) -> Result<ConvertedDocument>;
}

/// markit-backed [`Converter`] (vendored `vendor/markit` @ `4197c76a`).
///
/// markit's HTTP fetching is stubbed out in the vendored build (see
/// `vendor/markit/VENDOR.md`): Polyvec always feeds bytes directly, so URL
/// conversion fails loudly instead of reaching the network.
///
/// The converter is stateless: a fresh `Markit` is built per call. (markit's
/// own `Converter` trait objects are not `Send`/`Sync`, so holding one would
/// poison this type's thread-safety; construction is cheap next to the
/// conversion work.)
pub struct MarkitConverter;

impl MarkitConverter {
    /// Builds the converter. Construction is offline — no network involved.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Best-effort format label from the hint (extension wins over MIME).
    fn source_format(hint: &SourceHint) -> String {
        if let Some(ext) = hint.extension.as_deref() {
            let e = ext.trim().trim_start_matches('.').to_lowercase();
            if !e.is_empty() {
                return e;
            }
        }
        if let Some(mime) = hint.mime.as_deref() {
            let m = mime.trim().to_lowercase();
            if !m.is_empty() {
                return m;
            }
        }
        "unknown".to_string()
    }
}

impl Default for MarkitConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalizes an extension to markit's dotted lowercase form (`"html"` →
/// `".html"`); markit's `accepts` checks compare against dotted constants.
fn normalize_extension(ext: &str) -> String {
    let e = ext.trim().to_lowercase();
    if e.starts_with('.') {
        e
    } else {
        format!(".{e}")
    }
}

impl Converter for MarkitConverter {
    fn name(&self) -> &'static str {
        "markit"
    }

    fn convert(&self, bytes: &[u8], hint: &SourceHint) -> Result<ConvertedDocument> {
        if bytes.is_empty() {
            return Err(Error::Validation {
                detail: "cannot convert empty input".to_string(),
            });
        }
        let info = markit::types::StreamInfo {
            mimetype: hint.mime.clone(),
            extension: hint.extension.as_deref().map(normalize_extension),
            filename: hint.filename.clone(),
            ..Default::default()
        };
        let result = markit::markit::Markit::new()
            .convert(bytes, &info)
            .map_err(|e| Error::Provider {
                detail: format!("markit conversion failed: {e:#}"),
            })?;
        Ok(ConvertedDocument {
            markdown: result.markdown,
            title: result.title,
            conversion: ConversionInfo {
                converter: self.name().to_string(),
                converter_version: "0.6.1+4197c76a".to_string(),
                source_format: Self::source_format(hint),
            },
        })
    }
}
