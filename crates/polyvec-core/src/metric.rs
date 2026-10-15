//! Distance metrics.
//!
//! Phase 2 supports exactly two metrics, both provided by `anndists` inside
//! the ANN layer: [`DistanceMetric::L2`] (Euclidean) and
//! [`DistanceMetric::Cosine`]. This module owns the *contract* — parsing,
//! display, and the exact (brute-force) distance used by the baseline — so
//! every layer agrees on what "cosine" means.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The distance metric a collection is built with.
///
/// The metric is fixed at collection creation: vectors are indexed under one
/// metric and queries must use the same one. Mixing metrics is a validation
/// error, not a silent reinterpretation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistanceMetric {
    /// Euclidean distance: `sqrt(sum((a-b)^2))`.
    L2,
    /// Cosine distance: `1 - dot(a,b) / (|a|*|b|)`.
    ///
    /// Handles non-unit vectors (normalizes internally). The zero vector has
    /// distance 0 to itself and 1 to anything else.
    Cosine,
}

impl DistanceMetric {
    /// Exact distance between two slices (brute-force ground truth).
    ///
    /// # Panics
    /// Panics if the slices have different lengths — callers validate
    /// dimensions before reaching this point.
    #[must_use]
    pub fn exact(self, a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len(), "dimension mismatch in exact distance");
        match self {
            Self::L2 => a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| (x - y) * (x - y))
                .sum::<f32>()
                .sqrt(),
            Self::Cosine => {
                let (dot, na, nb) =
                    a.iter()
                        .zip(b.iter())
                        .fold((0.0f64, 0.0f64, 0.0f64), |(d, na, nb), (x, y)| {
                            (
                                d + f64::from(*x) * f64::from(*y),
                                na + f64::from(*x * *x),
                                nb + f64::from(*y * *y),
                            )
                        });
                if na == 0.0 || nb == 0.0 {
                    // Zero vector: 0 to itself, 1 to anything else.
                    if na == 0.0 && nb == 0.0 { 0.0 } else { 1.0 }
                } else {
                    let d = (1.0 - dot / (na * nb).sqrt()).max(0.0);
                    // d is a cosine distance in [0, 2]; the f64->f32 cast is exact
                    // for this range (no truncation possible).
                    #[allow(clippy::cast_possible_truncation)]
                    let d32 = d as f32;
                    d32
                }
            }
        }
    }
}

impl FromStr for DistanceMetric {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "l2" | "euclidean" => Ok(Self::L2),
            "cosine" => Ok(Self::Cosine),
            other => Err(Error::Config {
                detail: format!("unknown distance metric {other:?}; expected 'l2' or 'cosine'"),
            }),
        }
    }
}

impl fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::L2 => f.write_str("l2"),
            Self::Cosine => f.write_str("cosine"),
        }
    }
}
