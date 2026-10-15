//! Workspace-wide error type.
//!
//! Every Polyvec crate reports failures through [`Error`]; crate-specific
//! context travels in the `detail`/`what` payloads. The type is deliberately
//! flat: callers match on the variant, humans read the message.

use thiserror::Error;

/// The error type shared by every Polyvec crate.
///
/// Built on [`thiserror`](https://docs.rs/thiserror) so each variant keeps a
/// human-readable message and composes with `?` across crate boundaries.
/// Crate-specific errors (index, storage, providers) live in their own
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

    /// An invalid value failed domain validation.
    ///
    /// Returned for bad inputs: dimension mismatch, NaN/Inf components,
    /// empty vectors, malformed ids. Distinct from [`Error::Config`], which
    /// is about configuration rather than per-call values.
    #[error("invalid value: {detail}")]
    Validation {
        /// What was wrong, in plain language.
        detail: String,
    },

    /// The requested entity does not exist.
    #[error("not found: {what}")]
    NotFound {
        /// What was looked up, in plain language.
        what: String,
    },

    /// The entity already exists and cannot be created twice.
    #[error("already exists: {what}")]
    AlreadyExists {
        /// What collided, in plain language.
        what: String,
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

    /// The storage engine reported a failure (I/O, redb, serialization).
    #[error("storage error: {detail}")]
    Storage {
        /// What failed, in plain language (underlying message included).
        detail: String,
    },

    /// Persisted data failed an integrity check.
    ///
    /// Checksum mismatch, format-version mismatch, or structurally invalid
    /// files. Never a panic: the caller gets this variant and can decide
    /// (rebuild, restore from backup, abort).
    #[error("corrupt data: {detail}")]
    Corrupt {
        /// Which check failed and on what path, in plain language.
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

    /// An embedding or conversion provider failed.
    ///
    /// Returned for failures in external provider calls: network errors,
    /// authentication failures, rate limits, malformed provider responses.
    /// The `detail` never carries secrets (API keys are never logged).
    #[error("provider error: {detail}")]
    Provider {
        /// What failed, in plain language (underlying message included).
        detail: String,
    },
}

/// Convenience alias so signatures read `Result<T>` instead of
/// `Result<T, polyvec_core::Error>`.
pub type Result<T> = std::result::Result<T, Error>;
