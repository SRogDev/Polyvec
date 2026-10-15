//! Bearer-token authentication and per-IP rate limiting (Phase 7).
//!
//! Both are env-configured and safe by default:
//!
//! - `POLYVEC_API_KEY` unset → no authentication. The localhost-only
//!   default bind remains the protection, exactly as in Phases 3–6.
//! - `POLYVEC_RATE_LIMIT_RPS` default 1000; `0` disables rate limiting.
//!
//! The middleware wraps the `/v1` scope only: `/healthz` and `/readyz`
//! stay open (liveness/readiness probes must not need credentials or
//! consume quota).
//!
//! Security properties (see `SECURITY.md`):
//!
//! - The expected key is compared in constant time (no early exit), so a
//!   timing probe learns nothing about the key beyond its length. Length
//!   leakage is accepted: keys are operator-chosen, not user data.
//! - The key is never logged and never appears in error responses.
//!   `ServerConfig`'s `Debug` impl redacts it.
//! - Rate limiting is an in-memory per-IP token bucket: single-process,
//!   best-effort, resets on restart. Behind a reverse proxy the *proxy's*
//!   IP is what gets limited — rate-limit at the proxy in that case.

use std::collections::HashMap;
use std::future::{Future, Ready, ready};
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use actix_web::HttpResponse;
use actix_web::body::{EitherBody, MessageBody};
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform, forward_ready};

/// Configuration for the security middleware (mirrors the env knobs).
#[derive(Clone, Debug)]
pub struct SecurityConfig {
    /// Expected bearer token; `None` disables authentication.
    pub api_key: Option<String>,
    /// Sustained requests/second per client IP; `0` disables rate limiting.
    pub rate_limit_rps: u32,
}

/// Constant-time comparison for the bearer token: accumulates the XOR of
/// every byte pair instead of returning early, so mismatches at different
/// positions take the same time.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Per-IP token-bucket state.
#[derive(Debug)]
struct Bucket {
    /// Currently available tokens (fractional from refills).
    tokens: f64,
    /// When tokens were last refilled.
    last_refill: Instant,
}

/// In-memory per-IP token-bucket rate limiter.
///
/// Best-effort by design: buckets live only in this process and vanish on
/// restart; the map is pruned by wholesale clear past a cap. That is
/// acceptable for a localhost-first MVP — the limiter is a backstop
/// against accidents and floods, not a billing-grade quota system.
#[derive(Debug)]
pub struct RateLimiter {
    /// Tokens added per second per IP.
    rps: f64,
    /// Maximum burst (bucket capacity).
    capacity: f64,
    /// Client IP → bucket.
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    /// Builds a limiter for `rps` sustained requests/second per IP.
    /// Burst capacity is `max(4 * rps, 16)` — generous enough that normal
    /// client bursts never trip it, tight enough to blunt floods.
    #[must_use]
    pub fn new(rps: u32) -> Self {
        let rps_f = f64::from(rps);
        Self {
            rps: rps_f,
            capacity: (4.0 * rps_f).max(16.0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` when the request from `ip` at `now` may proceed.
    ///
    /// `now` is a parameter (rather than `Instant::now()`) so unit tests
    /// can control time deterministically.
    fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Wholesale prune past the cap: approximate, documented, and only
        // reachable under IP-spoofing-scale distinct addresses.
        if buckets.len() > 8192 {
            buckets.clear();
        }
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last_refill);
        bucket.tokens = (bucket.tokens + elapsed.as_secs_f64() * self.rps).min(self.capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Renders the shared `{ "error": { "code", "message", "detail" } }` shape
/// used by [`crate::error::ApiError`], with optional extra headers (the
/// middleware needs `WWW-Authenticate` / `Retry-After`, which `ApiError`
/// does not carry).
fn error_response(
    status: actix_web::http::StatusCode,
    code: &'static str,
    message: &str,
    headers: &[(&str, String)],
) -> HttpResponse {
    let mut builder = HttpResponse::build(status);
    for (name, value) in headers {
        builder.insert_header((*name, value.clone()));
    }
    builder.json(serde_json::json!({
        "error": { "code": code, "message": message, "detail": serde_json::Value::Null }
    }))
}

/// Extracts the bearer token from an `Authorization` header value.
/// Returns `None` when the header is absent or not a `Bearer` credential.
fn bearer_token(header: &str) -> Option<&str> {
    header
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Client IP for rate limiting: the socket peer address. (Behind a reverse
/// proxy this is the proxy's IP — rate-limit at the proxy in that case.)
fn client_ip(req: &ServiceRequest) -> IpAddr {
    req.connection_info()
        .realip_remote_addr()
        .and_then(|s| s.parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
}

/// Actix [`Transform`] enforcing bearer auth + rate limiting on the `/v1`
/// scope. Construct once per worker; the limiter is shared across workers
/// via `Arc` (cheap: one mutex, contention-free at this scale).
#[derive(Clone, Debug)]
pub struct SecurityMiddleware {
    config: SecurityConfig,
    limiter: Option<std::sync::Arc<RateLimiter>>,
}

impl SecurityMiddleware {
    /// Builds the middleware from the security config.
    #[must_use]
    pub fn new(config: SecurityConfig) -> Self {
        let limiter = (config.rate_limit_rps > 0)
            .then(|| std::sync::Arc::new(RateLimiter::new(config.rate_limit_rps)));
        Self { config, limiter }
    }
}

impl<S, B> Transform<S, ServiceRequest> for SecurityMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = actix_web::Error;
    type Transform = SecurityMiddlewareService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(SecurityMiddlewareService {
            service,
            config: self.config.clone(),
            limiter: self.limiter.clone(),
        }))
    }
}

/// The per-request service: rate-limit first (cheap, applies even to
/// unauthenticated floods), then bearer auth.
pub struct SecurityMiddlewareService<S> {
    service: S,
    config: SecurityConfig,
    limiter: Option<std::sync::Arc<RateLimiter>>,
}

impl<S, B> Service<ServiceRequest> for SecurityMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = actix_web::Error>,
    S::Future: 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = actix_web::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        // 1. Rate limit (all /v1 traffic; healthz/readyz are outside the
        //    wrapped scope and never reach here).
        if let Some(limiter) = &self.limiter
            && !limiter.allow(client_ip(&req), Instant::now())
        {
            let response = error_response(
                actix_web::http::StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "rate limit exceeded; slow down and retry",
                &[("retry-after", "1".to_string())],
            );
            let res = req.into_response(response.map_into_right_body());
            return Box::pin(async { Ok(res) });
        }

        // 2. Bearer auth (only when the operator configured a key).
        if let Some(expected) = &self.config.api_key {
            let authorized = req
                .headers()
                .get(actix_web::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(bearer_token)
                .is_some_and(|token| ct_eq(token.as_bytes(), expected.as_bytes()));
            if !authorized {
                let response = error_response(
                    actix_web::http::StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "valid bearer token required",
                    &[("www-authenticate", "Bearer".to_string())],
                );
                let res = req.into_response(response.map_into_right_body());
                return Box::pin(async { Ok(res) });
            }
        }

        let fut = self.service.call(req);
        Box::pin(async move { fut.await.map(ServiceResponse::map_into_left_body) })
    }
}

/// How long a client should wait before retrying after a 429.
pub const RATE_LIMIT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn ct_eq_matches_only_equal_tokens() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreu"));
        assert!(!ct_eq(b"secret", b"secrets"));
        assert!(!ct_eq(b"secret", b""));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn bearer_token_parses_rfc6750_shape() {
        assert_eq!(bearer_token("Bearer abc123"), Some("abc123"));
        assert_eq!(bearer_token("Bearer   spaced  "), Some("spaced"));
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("Basic abc123"), None);
        assert_eq!(bearer_token(""), None);
    }

    #[test]
    fn rate_limiter_allows_burst_then_denies() {
        let limiter = RateLimiter::new(10); // capacity = max(40, 16) = 40
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let t0 = Instant::now();
        for _ in 0..40 {
            assert!(limiter.allow(ip, t0), "burst within capacity must pass");
        }
        assert!(!limiter.allow(ip, t0), "burst exhausted must deny");
        // 0.5s at 10 rps refills 5 tokens.
        let t1 = t0 + Duration::from_millis(500);
        for _ in 0..5 {
            assert!(limiter.allow(ip, t1), "refilled tokens must pass");
        }
        assert!(!limiter.allow(ip, t1), "refill exhausted must deny again");
    }

    #[test]
    fn rate_limiter_is_per_ip() {
        let limiter = RateLimiter::new(1); // capacity = 16
        let t0 = Instant::now();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        for _ in 0..16 {
            assert!(limiter.allow(a, t0));
        }
        assert!(!limiter.allow(a, t0), "a is exhausted");
        assert!(limiter.allow(b, t0), "b has its own bucket");
    }
}
