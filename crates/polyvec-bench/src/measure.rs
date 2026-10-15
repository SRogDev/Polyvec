//! Measurement helpers: sizes, memory, recall arithmetic.
//!
//! All helpers are best-effort and documented as such — e.g. RSS is read
//! from `/proc/self/status` (Linux-only; `None` elsewhere).

use std::path::Path;

/// Recursively sums file sizes under `dir` (bytes).
#[must_use]
pub fn dir_size_bytes(dir: &Path) -> u64 {
    fn visit(dir: &Path, acc: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, acc);
            } else if let Ok(meta) = entry.metadata() {
                *acc += meta.len();
            }
        }
    }
    let mut acc = 0;
    visit(dir, &mut acc);
    acc
}

/// Peak resident set size (`VmHWM`) of this process in KiB, Linux-only.
#[must_use]
pub fn peak_rss_kib() -> Option<u64> {
    rss_field("VmHWM:")
}

/// Current resident set size (`VmRSS`) of this process in KiB, Linux-only.
/// Unlike the peak, this can go down — use it for before/after deltas.
#[must_use]
pub fn rss_kib() -> Option<u64> {
    rss_field("VmRSS:")
}

fn rss_field(prefix: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

/// recall@k: fraction of the exact top-k ids present in the approximate top-k.
/// Both slices hold ids in rank order; only membership matters.
#[must_use]
#[allow(clippy::cast_precision_loss)] // ratio of small ints
pub fn recall_at_k(exact_top_k: &[usize], approx_top_k: &[usize], k: usize) -> f64 {
    if k == 0 || exact_top_k.is_empty() {
        return 1.0;
    }
    let approx: std::collections::HashSet<usize> = approx_top_k.iter().copied().collect();
    let hits = exact_top_k
        .iter()
        .take(k)
        .filter(|id| approx.contains(id))
        .count();
    hits as f64 / k.min(exact_top_k.len()) as f64
}

/// Mean reciprocal rank over `(query, expected_rank1_id)` pairs: for each
/// query, the reciprocal rank of the expected id in the ranked result list
/// (0 when absent), averaged.
#[must_use]
#[allow(clippy::cast_precision_loss)] // ratio of small ints
pub fn mean_reciprocal_rank(results: &[(Vec<usize>, usize)]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    let sum: f64 = results
        .iter()
        .map(|(ranked, expected)| {
            ranked
                .iter()
                .position(|id| id == expected)
                .map_or(0.0, |pos| 1.0 / (pos + 1) as f64)
        })
        .sum();
    sum / results.len() as f64
}
