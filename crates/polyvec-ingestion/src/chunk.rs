//! Deterministic section-aware chunking.
//!
//! [`Chunker`] splits normalized markdown into chunks:
//!
//! * Sections are atomic: the text is split at ATX headings (`#`–`######`),
//!   and each section becomes its own chunk. Chunk boundaries never cut a
//!   section in two — except when a single section exceeds `target_chars`,
//!   in which case it is hard-split into `target_chars` windows with
//!   `overlap_chars` overlap.
//! * Offsets are character indices into the source markdown; chunk text is
//!   always exactly `markdown[char_start..char_end]`.
//! * Deterministic: the same markdown + config always yields the same
//!   chunks. Chunk *ids* are assigned by the pipeline from
//!   (document id, version, index) — see
//!   [`polyvec_core::ChunkId::for_versioned_index`].
//!
//! Empty or whitespace-only input yields no chunks; the pipeline rejects
//! such documents loudly instead of indexing empty vectors.

use polyvec_core::{ChunkingConfig, Result};

/// One chunk of markdown with character offsets into the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawChunk {
    /// The chunk text: exactly `source[char_start..char_end]` in chars.
    pub text: String,
    /// Character offset of the first char in the source markdown.
    pub char_start: usize,
    /// Character offset one past the last char in the source markdown.
    pub char_end: usize,
}

/// Section-aware deterministic chunker.
pub struct Chunker {
    config: ChunkingConfig,
}

impl Chunker {
    /// Builds the chunker.
    ///
    /// # Errors
    /// [`Error::Config`] if the chunking parameters are invalid.
    pub fn new(config: ChunkingConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    /// The config this chunker was built with.
    #[must_use]
    pub fn config(&self) -> &ChunkingConfig {
        &self.config
    }

    /// Splits `markdown` (expected normalized, but any text works) into
    /// chunks. Returns an empty vec for empty/whitespace-only input.
    #[must_use]
    pub fn chunk(&self, markdown: &str) -> Vec<RawChunk> {
        let chars: Vec<char> = markdown.chars().collect();
        if chars.iter().all(|c| c.is_whitespace()) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (start, end) in split_sections(&chars) {
            let len = end - start;
            if len <= self.config.target_chars {
                out.push(slice(&chars, start, end));
            } else {
                out.extend(hard_split(&chars, start, end, &self.config));
            }
        }
        out
    }
}

/// Builds a chunk from char range `[start, end)`.
fn slice(chars: &[char], start: usize, end: usize) -> RawChunk {
    RawChunk {
        text: chars[start..end].iter().collect(),
        char_start: start,
        char_end: end,
    }
}

/// Hard-splits `[start, end)` into `target_chars` windows with
/// `overlap_chars` overlap between consecutive windows.
fn hard_split(chars: &[char], start: usize, end: usize, config: &ChunkingConfig) -> Vec<RawChunk> {
    let target = config.target_chars;
    let overlap = config.overlap_chars;
    let step = target - overlap;
    let mut out = Vec::new();
    let mut pos = start;
    while pos < end {
        let window_end = (pos + target).min(end);
        out.push(slice(chars, pos, window_end));
        if window_end == end {
            break;
        }
        pos += step;
    }
    out
}

/// Splits char slices into sections at ATX headings.
///
/// Returns `(start, end)` char ranges. Leading content before the first
/// heading is its own section; a document with no headings is one section.
fn split_sections(chars: &[char]) -> Vec<(usize, usize)> {
    // Line starts: 0 and every index after a '\n'.
    let mut line_starts = vec![0usize];
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            line_starts.push(i + 1);
        }
    }
    let mut sections = Vec::new();
    let mut current = 0usize;
    for &ls in &line_starts[1..] {
        if is_heading(chars, ls) {
            sections.push((current, ls));
            current = ls;
        }
    }
    sections.push((current, chars.len()));
    // Drop empty sections (e.g. a trailing heading with no content is kept —
    // only truly empty ranges are dropped).
    sections
        .into_iter()
        .filter(|(s, e)| {
            let text: String = chars[*s..*e].iter().collect();
            !text.trim().is_empty()
        })
        .collect()
}

/// True if the line starting at `ls` is an ATX heading (`#`–`######` followed
/// by a space or end of line).
fn is_heading(chars: &[char], ls: usize) -> bool {
    let mut hashes = 0usize;
    let mut i = ls;
    while i < chars.len() && chars[i] == '#' && hashes < 6 {
        hashes += 1;
        i += 1;
    }
    if hashes == 0 {
        return false;
    }
    // 7+ hashes is not a heading; require space/EOL after the run.
    if i < chars.len() && chars[i] == '#' {
        return false;
    }
    i == chars.len() || chars[i] == ' ' || chars[i] == '\t' || chars[i] == '\n'
}
