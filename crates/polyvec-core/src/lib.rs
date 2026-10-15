//! # polyvec-core
//!
//! Shared domain foundation of the Polyvec vector database.
//!
//! Everything every other crate needs, and nothing else. Per the plan
//! (§4, "crate responsibilities"), this crate will hold:
//!
//! * **Domain types** — the vocabulary of the database: collections,
//!   documents, vectors, payloads, and the identifiers that name them.
//! * **IDs** — strongly-typed, validated identifiers (`CollectionId`,
//!   `DocumentId`, …) so raw `u64`/`String` never leak across crate
//!   boundaries.
//! * **Config contracts** — the validated configuration shapes each
//!   subsystem consumes (dimensions, distance metrics, index parameters).
//!
//! What deliberately does **not** live here:
//!
//! * No Actix Web, no Tokio, no HTTP types — the web/API crates depend on
//!   `polyvec-core`, never the reverse (see `ARCHITECTURE.md`).
//! * No embedding providers, no index implementations, no storage engines.
//!   Those crates are created when their phase needs them — no abstraction
//!   without a concrete use case.
//!
//! Today (Phase 0) the crate seeds the foundation with the one thing every
//!   future error path will share: [`Error`], the workspace-wide error type.

use thiserror::Error;

/// The error type shared by every Polyvec crate.
///
/// Built on [`thiserror`](https://docs.rs/thiserror) so each variant keeps a
/// human-readable message and composes with `?` across crate boundaries.
/// Crate-specific errors (index, storage, providers) will live in their own
/// crates and convert into [`Error`] where crossing into shared code.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid or inconsistent configuration.
    ///
    /// Returned when a config value fails validation (e.g. a non-positive
    /// vector dimension, an unknown distance metric) or when two config
    /// values contradict each other.
    #[error("invalid configuration: {detail}")]
    Config {
        /// What was wrong, in plain language.
        detail: String,
    },

    /// An operation that is not (yet) supported.
    ///
    /// Returned for requests the engine cannot honor in its current shape —
    /// for example a distance metric the index build did not compile in.
    /// Distinct from "not implemented in this phase": this is about the
    /// request, not the codebase's maturity.
    #[error("not supported: {detail}")]
    Unsupported {
        /// What was requested and why it cannot be honored.
        detail: String,
    },

    /// An internal invariant violation — always a bug, never user error.
    ///
    /// If this variant ever surfaces, the engine's assumptions broke. File a
    /// bug report with the message and the input that produced it.
    #[error("internal error: {detail}")]
    Internal {
        /// The violated invariant, with as much context as is available.
        detail: String,
    },
}

/// Convenience alias so signatures read `Result<T>` instead of
/// `Result<T, polyvec_core::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_std_error<T: std::error::Error + std::fmt::Debug>(_: &T) {}

    #[test]
    fn config_error_display_contains_detail() {
        let err = Error::Config {
            detail: "dimension must be positive, got 0".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "invalid configuration: dimension must be positive, got 0"
        );
    }

    #[test]
    fn unsupported_error_display_contains_detail() {
        let err = Error::Unsupported {
            detail: "metric 'manhattan' not compiled into this index".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "not supported: metric 'manhattan' not compiled into this index"
        );
    }

    #[test]
    fn internal_error_display_contains_detail() {
        let err = Error::Internal {
            detail: "segment 7 missing from manifest".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "internal error: segment 7 missing from manifest"
        );
    }

    #[test]
    fn error_implements_std_error() {
        let err = Error::Config {
            detail: "x".to_string(),
        };
        // Compile-time check that `?` and error-chain interop work.
        assert_std_error(&err);
    }
}
