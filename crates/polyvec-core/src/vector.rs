//! A validated vector: id + finite `f32` components of the expected dimension.
//!
//! Validation happens once, at construction: dimension match, non-empty,
//! every component finite. Downstream layers (index, storage) can rely on
//! the invariant instead of re-checking.

use serde::{Deserialize, Serialize};

use crate::{Error, Result, VectorId};

/// A single embedding with its stable identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vector {
    /// Stable identity (derived from the owning chunk in Phase 2).
    pub id: VectorId,
    /// The embedding components.
    pub values: Vec<f32>,
}

impl Vector {
    /// Builds a vector, validating dimension and finiteness.
    ///
    /// # Errors
    /// [`Error::Validation`] if `values` is empty, `values.len() != dim`,
    /// or any component is NaN or infinite.
    pub fn new(id: VectorId, values: Vec<f32>, dim: u32) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::Validation {
                detail: "vector must not be empty".to_string(),
            });
        }
        if values.len() != dim as usize {
            return Err(Error::Validation {
                detail: format!(
                    "dimension mismatch: vector has {} components, collection expects {dim}",
                    values.len()
                ),
            });
        }
        if values.iter().any(|x| !x.is_finite()) {
            return Err(Error::Validation {
                detail: "vector components must be finite (no NaN/Inf)".to_string(),
            });
        }
        Ok(Self { id, values })
    }

    /// The vector dimension.
    #[must_use]
    pub fn dimension(&self) -> u32 {
        // Validated at construction: values.len() == dim <= MAX_DIMENSION,
        // so the usize->u32 cast cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        let dim = self.values.len() as u32;
        dim
    }
}
