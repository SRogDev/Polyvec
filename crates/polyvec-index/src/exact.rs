//! Brute-force exact index: the ground-truth baseline.
//!
//! [`ExactIndex`] answers top-k by linear scan with the collection's metric.
//! It is deliberately too slow for production — its job is to *measure* the
//! ANN index: recall@k is computed against this baseline, never claimed.

use polyvec_core::DistanceMetric;

use crate::SearchHit;

/// Brute-force top-k over an in-memory vector list.
#[derive(Clone, Debug)]
pub struct ExactIndex {
    metric: DistanceMetric,
    entries: Vec<(usize, Vec<f32>)>,
}

impl ExactIndex {
    /// Empty baseline for `metric`.
    #[must_use]
    pub fn new(metric: DistanceMetric) -> Self {
        Self {
            metric,
            entries: Vec::new(),
        }
    }

    /// Adds a vector under `node_id`.
    pub fn insert(&mut self, node_id: usize, values: Vec<f32>) {
        self.entries.push((node_id, values));
    }

    /// Removes `node_id`. Returns `true` if it was present.
    pub fn remove(&mut self, node_id: usize) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(id, _)| *id != node_id);
        self.entries.len() != before
    }

    /// Exact top-k by the metric (ascending distance, ties by node id).
    #[must_use]
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchHit> {
        let mut scored: Vec<(usize, f32)> = self
            .entries
            .iter()
            .map(|(id, v)| (*id, self.metric.exact(query, v)))
            .collect();
        scored.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        scored
            .into_iter()
            .take(k)
            .map(|(node_id, distance)| SearchHit { node_id, distance })
            .collect()
    }

    /// Number of vectors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no vectors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Fraction of `exact` ids present in `retrieved` (recall@k).
///
/// Both slices hold node ids; `exact.len()` is k. Returns 0.0 when `exact`
/// is empty.
#[must_use]
pub fn recall_at_k(retrieved: &[usize], exact: &[usize]) -> f32 {
    if exact.is_empty() {
        return 0.0;
    }
    let wanted: std::collections::HashSet<usize> = exact.iter().copied().collect();
    let hits = retrieved.iter().filter(|id| wanted.contains(id)).count();
    // Counts here are result-set sizes (small); the usize->f32 cast is safe.
    #[allow(clippy::cast_precision_loss)]
    let recall = hits as f32 / exact.len() as f32;
    recall
}
