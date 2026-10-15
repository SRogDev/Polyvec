//! Deterministic stub embedding provider for tests and development.
//!
//! **NOT for production use.** The vectors are derived from SHA-256 hashes
//! of the input text: deterministic (same text → same vector) and
//! L2-normalized (cosine-friendly), but carrying zero semantic meaning.
//! Retrieval tests use it to prove the pipeline works end to end; any claim
//! about retrieval *quality* needs a real model.

use polyvec_core::{MAX_DIMENSION, ModelTag, Result as CoreResult};

use crate::provider::{EmbedResult, EmbeddingProvider};

/// Deterministic hash-based embeddings. Test/dev only — never production.
pub struct StubEmbeddingProvider {
    dimension: u32,
}

impl StubEmbeddingProvider {
    /// Builds the stub for `dimension`-wide vectors.
    ///
    /// # Errors
    /// [`polyvec_core::Error::Config`] if the dimension is 0 or above
    /// [`MAX_DIMENSION`].
    pub fn new(dimension: u32) -> CoreResult<Self> {
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(polyvec_core::Error::Config {
                detail: format!("stub dimension must be 1..={MAX_DIMENSION}, got {dimension}"),
            });
        }
        Ok(Self { dimension })
    }

    /// Derives one deterministic, L2-normalized vector from `text`.
    fn embed_one(&self, text: &str) -> Vec<f32> {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(text.as_bytes());
        // Seed a xorshift64* from the first 8 digest bytes; expand to
        // `dimension` components in [-1, 1).
        let mut state = digest[0..8]
            .try_into()
            .map(u64::from_le_bytes)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        if state == 0 {
            state = 0x9e3779b97f4a7c15;
        }
        let dim = self.dimension as usize;
        let mut v = Vec::with_capacity(dim);
        for _ in 0..dim {
            // xorshift64*.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let x = state.wrapping_mul(0x2545F4914F6CDD1D);
            // Top 53 bits → [0, 1) → [-1, 1).
            let u = ((x >> 11) as f64) / (1u64 << 53) as f64;
            v.push((u * 2.0 - 1.0) as f32);
        }
        // L2-normalize (guard against the measure-zero all-zeros case).
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

impl EmbeddingProvider for StubEmbeddingProvider {
    fn embed(&self, texts: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.embed_one(t)).collect())
    }

    fn model_id(&self) -> ModelTag {
        // Infallible: both literals are non-empty by construction.
        ModelTag::new("polyvec-stub", "0-not-for-production")
            .expect("stub model tag literals are non-empty")
    }

    fn dimension(&self) -> u32 {
        self.dimension
    }
}
