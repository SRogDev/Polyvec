//! The provider trait and its error contract.
//!
//! [`EmbedError`] carries a retry classification ([`ErrorKind`]) alongside
//! the message: the ingestion runner retries transient failures with bounded
//! exponential backoff and never retries permanent ones. This is the
//! cost-guard seam — a 429 must not become a retry storm, and a 401 must not
//! be retried at all.

use std::time::Duration;

use polyvec_core::{Error as CoreError, ModelTag};

/// Whether a provider failure is worth retrying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Retryable: rate limits (429), 5xx, timeouts, connection errors.
    /// Retried with bounded exponential backoff — never unbounded.
    Transient,
    /// Not retryable: 4xx validation/auth errors, dimension mismatches,
    /// malformed responses. Retrying would burn budget for nothing.
    Permanent,
}

/// A provider failure with its retry classification.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedError {
    /// Retry classification (see [`ErrorKind`]).
    pub kind: ErrorKind,
    /// Human-readable cause. Never carries secrets.
    pub message: String,
    /// Server-suggested wait parsed from a `Retry-After` header, if any.
    pub retry_after: Option<Duration>,
}

impl EmbedError {
    /// Builds a transient (retryable) error.
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Transient,
            message: message.into(),
            retry_after: None,
        }
    }

    /// Builds a permanent (non-retryable) error.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Permanent,
            message: message.into(),
            retry_after: None,
        }
    }

    /// Builds a rate-limit error with the server's suggested wait.
    pub fn rate_limited(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            kind: ErrorKind::Transient,
            message: message.into(),
            retry_after,
        }
    }
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scope = match self.kind {
            ErrorKind::Transient => "transient",
            ErrorKind::Permanent => "permanent",
        };
        write!(f, "[{scope}] {}", self.message)
    }
}

impl std::error::Error for EmbedError {}

/// Converts into the workspace error; the retry kind travels in the message
/// prefix so logs keep it, while API responses get the safe shape.
impl From<EmbedError> for CoreError {
    fn from(e: EmbedError) -> Self {
        CoreError::Provider {
            detail: e.to_string(),
        }
    }
}

/// Convenience alias for provider results.
pub type EmbedResult<T> = std::result::Result<T, EmbedError>;

/// Turns text into vectors. Synchronous and dyn-safe.
///
/// Implementations doing I/O (the remote adapter) use blocking HTTP and must
/// be called from a blocking thread — the ingestion runner enforces this, so
/// async request workers are never stalled by embedding calls.
pub trait EmbeddingProvider: Send + Sync {
    /// Embeds each text; the output order matches the input order.
    ///
    /// An empty input returns an empty vector without doing any work.
    ///
    /// # Errors
    /// [`EmbedError`] with [`ErrorKind::Transient`] for retryable failures
    /// (rate limits, 5xx, timeouts) and [`ErrorKind::Permanent`] for
    /// non-retryable ones (auth, validation, dimension mismatch).
    fn embed(&self, texts: &[&str]) -> EmbedResult<Vec<Vec<f32>>>;

    /// Identifies the model (name + version) these vectors came from.
    ///
    /// The ingestion pipeline compares this against the collection's model
    /// tag and refuses to mix models — never silently.
    fn model_id(&self) -> ModelTag;

    /// Vector dimension this provider produces.
    ///
    /// Must equal the collection dimension; mismatches fail fast at ingest
    /// time rather than corrupting the index.
    fn dimension(&self) -> u32;
}
