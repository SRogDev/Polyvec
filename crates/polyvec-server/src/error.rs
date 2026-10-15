//! HTTP error mapping: [`polyvec_core::Error`] → status code + JSON body.
//!
//! Every error response has the same shape:
//!
//! ```json
//! { "error": { "code": "not_found", "message": "not found: vector 'x'", "detail": null } }
//! ```
//!
//! `code` is machine-readable, `message` is human-readable, `detail` carries
//! optional extra context. Internal-ish failures (`Corrupt`, `Storage`,
//! `Internal`) return **safe** messages — the full detail goes to the
//! server log, never to the client.

use actix_web::{HttpRequest, HttpResponse, ResponseError, http::StatusCode};
use polyvec_core::Error as CoreError;

/// A structured API error: status + machine code + human message.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    /// Builds an error with an explicit status, code, and message.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400 for a request value that failed domain validation.
    #[must_use]
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "validation_error", detail.into())
    }

    /// 413 for a request body larger than the configured JSON limit.
    #[must_use]
    pub fn payload_too_large(limit: usize) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("request body exceeds the {limit}-byte JSON limit"),
        )
    }

    /// 503 when the server cannot serve requests (data dir unusable).
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            detail.into(),
        )
    }

    /// Machine-readable code for the `"code"` field.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// Maps domain errors to HTTP. Corrupt/Storage/Internal keep their detail
/// in the server log only — the client gets a safe generic message.
impl From<CoreError> for ApiError {
    fn from(err: CoreError) -> Self {
        match err {
            CoreError::Validation { detail } => Self::validation(detail),
            CoreError::Config { detail } => {
                Self::new(StatusCode::BAD_REQUEST, "config_error", detail)
            }
            CoreError::NotFound { what } => Self::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("not found: {what}"),
            ),
            CoreError::AlreadyExists { what } => Self::new(
                StatusCode::CONFLICT,
                "already_exists",
                format!("already exists: {what}"),
            ),
            CoreError::Unsupported { detail } => Self::new(
                StatusCode::NOT_IMPLEMENTED,
                "unsupported",
                format!("not supported: {detail}"),
            ),
            CoreError::Corrupt { detail } => {
                log::error!("corrupt data: {detail}");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "corrupt",
                    "stored data failed an integrity check; see server logs",
                )
            }
            CoreError::Storage { detail } => {
                log::error!("storage failure: {detail}");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "storage_error",
                    "storage failure; see server logs",
                )
            }
            CoreError::Internal { detail } => {
                log::error!("internal invariant violated: {detail}");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal error; see server logs",
                )
            }
        }
    }
}

impl From<actix_web::error::BlockingError> for ApiError {
    fn from(_: actix_web::error::BlockingError) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "blocking_error",
            "background task failed; see server logs",
        )
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        self.status
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status).json(serde_json::json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "detail": serde_json::Value::Null,
            }
        }))
    }
}

/// Rejects malformed JSON (400) and oversized bodies (413) with the
/// structured error shape instead of actix-web's default plain-text errors.
///
/// The by-value `err` is dictated by actix-web's `JsonConfig::error_handler`
/// signature (`Fn(JsonPayloadError, &HttpRequest) -> Error`).
#[allow(clippy::needless_pass_by_value)]
pub fn json_error_handler(
    err: actix_web::error::JsonPayloadError,
    _req: &HttpRequest,
) -> actix_web::Error {
    let api = match &err {
        // actix-web distinguishes "content-length said it upfront" from
        // "the stream ran past the limit"; both are a 413 here.
        actix_web::error::JsonPayloadError::Overflow { limit, .. }
        | actix_web::error::JsonPayloadError::OverflowKnownLength { limit, .. } => {
            ApiError::payload_too_large(*limit)
        }
        actix_web::error::JsonPayloadError::ContentType => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_content_type",
            "content-type must be application/json",
        ),
        other => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            format!("invalid JSON body: {other}"),
        ),
    };
    api.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;

    #[test]
    fn core_error_mapping_statuses() {
        let cases = [
            (
                CoreError::Validation {
                    detail: "bad".to_string(),
                },
                StatusCode::BAD_REQUEST,
                "validation_error",
            ),
            (
                CoreError::Config {
                    detail: "bad".to_string(),
                },
                StatusCode::BAD_REQUEST,
                "config_error",
            ),
            (
                CoreError::NotFound {
                    what: "x".to_string(),
                },
                StatusCode::NOT_FOUND,
                "not_found",
            ),
            (
                CoreError::AlreadyExists {
                    what: "x".to_string(),
                },
                StatusCode::CONFLICT,
                "already_exists",
            ),
            (
                CoreError::Unsupported {
                    detail: "x".to_string(),
                },
                StatusCode::NOT_IMPLEMENTED,
                "unsupported",
            ),
            (
                CoreError::Corrupt {
                    detail: "checksum mismatch at /secret/path".to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                "corrupt",
            ),
            (
                CoreError::Storage {
                    detail: "io at /secret/path".to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
            ),
            (
                CoreError::Internal {
                    detail: "invariant".to_string(),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];
        for (core, status, code) in cases {
            let api = ApiError::from(core);
            assert_eq!(api.status_code(), status, "code={code}");
            assert_eq!(api.code(), code);
        }
    }

    #[tokio::test]
    async fn internalish_errors_get_safe_messages() {
        // The detail (paths, io errors) goes to the server log only — the
        // client must not see it.
        for core in [
            CoreError::Corrupt {
                detail: "checksum mismatch at /data/kb/catalog.redb".to_string(),
            },
            CoreError::Storage {
                detail: "open /data/kb/catalog.redb: permission denied".to_string(),
            },
            CoreError::Internal {
                detail: "node 42 has no catalog mapping".to_string(),
            },
        ] {
            let api = ApiError::from(core);
            let body = api.error_response().into_body();
            let bytes = actix_web::body::to_bytes(body).await.unwrap();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(
                !text.contains("/data/kb"),
                "leaked path in client error: {text}"
            );
            assert!(text.contains("\"detail\":null"), "body: {text}");
        }
    }
}
