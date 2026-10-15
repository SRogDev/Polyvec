//! Server configuration: bind address, data directory, request limits.
//!
//! Configuration comes from the environment (documented below) —
//! deliberately simple: no config file in Phase 3. Every knob has a safe
//! default; binding beyond localhost requires an explicit opt-in.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `POLYVEC_BIND` | `127.0.0.1:8080` | `IP:port` to listen on. Non-loopback addresses are refused unless `POLYVEC_ALLOW_NONLOCAL_BIND=1`. `localhost` is accepted as an alias for `127.0.0.1`. |
//! | `POLYVEC_DATA_DIR` | `./polyvec-data` | Directory holding one sub-directory per collection. Created on startup. |
//! | `POLYVEC_JSON_LIMIT_BYTES` | `16777216` (16 MiB) | Max JSON request body; larger bodies get `413`. |
//! | `POLYVEC_WORKERS` | (actix default: CPU count) | Number of HTTP worker threads. |
//! | `POLYVEC_ALLOW_NONLOCAL_BIND` | unset | Set to `1` to allow binding a non-loopback address. Phase 3 has **no authentication** — only set this behind a reverse proxy or firewall you trust. |
//! | `POLYVEC_EMBEDDING_PROVIDER` | `none` | `none` (no provider — text ingest/query refused), `stub` (deterministic stub, dev/test only), `remote` (OpenAI-compatible HTTP endpoint). |
//! | `POLYVEC_STUB_DIMENSION` | `64` | Stub provider dimension (1–4096). The collection dimension must match. |
//! | `POLYVEC_JOB_QUEUE_CAPACITY` | `64` | Ingest job queue capacity. Submits past capacity get `429` with `Retry-After`. |
//! | `POLYVEC_JOB_CONCURRENCY` | `4` | Ingest jobs executed concurrently. |
//! | `POLYVEC_API_KEY` | unset | Bearer token required on every `/v1` route (`Authorization: Bearer <key>`). Unset = no authentication (localhost-only default bind remains the protection). Empty string counts as unset. |
//! | `POLYVEC_RATE_LIMIT_RPS` | `1000` | Sustained requests/second per client IP on `/v1` routes (burst = 4×). `0` disables rate limiting. |
//!
//! The remote embedding provider reads its own variables — see
//! `polyvec_embeddings::RemoteEmbeddingProvider` (`POLYVEC_EMBEDDING_BASE_URL`,
//! `POLYVEC_EMBEDDING_API_KEY`, `POLYVEC_EMBEDDING_MODEL`, …).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// Default bind address: localhost only.
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";
/// Default data directory (relative to the server's working directory).
pub const DEFAULT_DATA_DIR: &str = "./polyvec-data";
/// Default maximum JSON body: 16 MiB (a 10k-vector batch at dim 384 is ~15 MiB JSON).
pub const DEFAULT_JSON_LIMIT_BYTES: usize = 16 * 1024 * 1024;
/// Default stub provider dimension.
pub const DEFAULT_STUB_DIMENSION: u32 = 64;
/// Default ingest job queue capacity.
pub const DEFAULT_JOB_QUEUE_CAPACITY: usize = 64;
/// Default ingest job concurrency.
pub const DEFAULT_JOB_CONCURRENCY: usize = 4;
/// Default per-IP rate limit (requests/second); `0` disables.
pub const DEFAULT_RATE_LIMIT_RPS: u32 = 1000;

/// How text is embedded for ingestion and text queries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbeddingKind {
    /// No provider: text ingest/query without supplied vectors is refused
    /// (`501`). The safe default — no hidden network calls.
    None,
    /// Deterministic stub provider for development and tests.
    /// **Never use in production**: the vectors are a hash function, not
    /// a semantic model.
    Stub {
        /// The stub's dimension; the collection dimension must match.
        dimension: u32,
    },
    /// OpenAI-compatible remote endpoint (env-configured).
    Remote,
}

/// Runtime configuration of the HTTP server.
///
/// `Debug` is implemented manually: the API key is redacted so it can
/// never leak through a log line (see `SECURITY.md`).
#[derive(Clone)]
pub struct ServerConfig {
    /// Address to bind.
    pub bind: SocketAddr,
    /// Directory holding per-collection sub-directories.
    pub data_dir: PathBuf,
    /// Maximum JSON request body in bytes.
    pub json_limit: usize,
    /// HTTP worker threads (`None` = actix default).
    pub workers: Option<usize>,
    /// Whether a non-loopback bind was explicitly allowed.
    pub allow_nonlocal_bind: bool,
    /// How text is embedded for ingestion and text queries.
    pub embedding: EmbeddingKind,
    /// Ingest job queue capacity (backpressure: submits past capacity get 429).
    pub job_queue_capacity: usize,
    /// Ingest jobs executed concurrently.
    pub job_concurrency: usize,
    /// Bearer token required on every `/v1` route (`None` = auth disabled;
    /// the localhost-only default bind remains the protection then).
    pub api_key: Option<String>,
    /// Sustained requests/second per client IP on `/v1` routes
    /// (`0` = rate limiting disabled).
    pub rate_limit_rps: u32,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind", &self.bind)
            .field("data_dir", &self.data_dir)
            .field("json_limit", &self.json_limit)
            .field("workers", &self.workers)
            .field("allow_nonlocal_bind", &self.allow_nonlocal_bind)
            .field("embedding", &self.embedding)
            .field("job_queue_capacity", &self.job_queue_capacity)
            .field("job_concurrency", &self.job_concurrency)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("rate_limit_rps", &self.rate_limit_rps)
            .finish()
    }
}

impl ServerConfig {
    /// Reads configuration from the process environment.
    ///
    /// # Errors
    /// Returns a human-readable message if any variable is malformed, or if
    /// the bind address is not loopback and `POLYVEC_ALLOW_NONLOCAL_BIND`
    /// is not set to `1`.
    pub fn from_env() -> Result<Self, String> {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let refs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Self::from_env_in(&refs)
    }

    /// Reads configuration from an explicit set of `(key, value)` pairs.
    ///
    /// The real entry point is [`ServerConfig::from_env`]; this variant
    /// exists so tests can exercise parsing without touching the process
    /// environment.
    ///
    /// # Errors
    /// Same as [`ServerConfig::from_env`].
    pub fn from_env_in(vars: &[(&str, &str)]) -> Result<Self, String> {
        let get = |key: &str| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);

        let bind_raw = get("POLYVEC_BIND").unwrap_or(DEFAULT_BIND);
        // Accept `localhost` as an alias; `SocketAddr::from_str` needs an IP.
        let bind_raw = bind_raw.replacen("localhost", "127.0.0.1", 1);
        let bind = SocketAddr::from_str(&bind_raw)
            .map_err(|_| format!("POLYVEC_BIND={bind_raw:?} is not a valid IP:port"))?;

        let allow_nonlocal_bind = matches!(get("POLYVEC_ALLOW_NONLOCAL_BIND"), Some("1"));
        if !bind.ip().is_loopback() && !allow_nonlocal_bind {
            return Err(format!(
                "refusing to bind {bind}: non-loopback addresses need explicit opt-in \
                 (set POLYVEC_ALLOW_NONLOCAL_BIND=1). Phase 3 has no authentication; \
                 do not expose the server to untrusted networks."
            ));
        }

        let data_dir = PathBuf::from(get("POLYVEC_DATA_DIR").unwrap_or(DEFAULT_DATA_DIR));

        let json_limit = get("POLYVEC_JSON_LIMIT_BYTES")
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| "POLYVEC_JSON_LIMIT_BYTES must be a positive integer".to_string())?
            .unwrap_or(DEFAULT_JSON_LIMIT_BYTES);
        if json_limit == 0 {
            return Err("POLYVEC_JSON_LIMIT_BYTES must be > 0".to_string());
        }

        let workers = get("POLYVEC_WORKERS")
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| "POLYVEC_WORKERS must be a positive integer".to_string())?;
        if workers == Some(0) {
            return Err("POLYVEC_WORKERS must be > 0".to_string());
        }

        let embedding = match get("POLYVEC_EMBEDDING_PROVIDER").unwrap_or("none") {
            "none" => EmbeddingKind::None,
            "stub" => {
                let dimension = get("POLYVEC_STUB_DIMENSION")
                    .map(str::parse::<u32>)
                    .transpose()
                    .map_err(|_| "POLYVEC_STUB_DIMENSION must be a positive integer".to_string())?
                    .unwrap_or(DEFAULT_STUB_DIMENSION);
                if !(1..=4096).contains(&dimension) {
                    return Err("POLYVEC_STUB_DIMENSION must be in 1..=4096".to_string());
                }
                EmbeddingKind::Stub { dimension }
            }
            "remote" => EmbeddingKind::Remote,
            other => {
                return Err(format!(
                    "POLYVEC_EMBEDDING_PROVIDER={other:?} is not one of: none, stub, remote"
                ));
            }
        };

        let job_queue_capacity = get("POLYVEC_JOB_QUEUE_CAPACITY")
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| "POLYVEC_JOB_QUEUE_CAPACITY must be a positive integer".to_string())?
            .unwrap_or(DEFAULT_JOB_QUEUE_CAPACITY);
        if job_queue_capacity == 0 {
            return Err("POLYVEC_JOB_QUEUE_CAPACITY must be > 0".to_string());
        }

        let job_concurrency = get("POLYVEC_JOB_CONCURRENCY")
            .map(str::parse::<usize>)
            .transpose()
            .map_err(|_| "POLYVEC_JOB_CONCURRENCY must be a positive integer".to_string())?
            .unwrap_or(DEFAULT_JOB_CONCURRENCY);
        if job_concurrency == 0 {
            return Err("POLYVEC_JOB_CONCURRENCY must be > 0".to_string());
        }

        // Empty string counts as unset: an "empty key" would be worse than
        // no key (it looks configured while matching nothing useful).
        let api_key = get("POLYVEC_API_KEY")
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string);

        let rate_limit_rps = get("POLYVEC_RATE_LIMIT_RPS")
            .map(str::parse::<u32>)
            .transpose()
            .map_err(|_| "POLYVEC_RATE_LIMIT_RPS must be a non-negative integer".to_string())?
            .unwrap_or(DEFAULT_RATE_LIMIT_RPS);

        Ok(Self {
            bind,
            data_dir,
            json_limit,
            workers,
            allow_nonlocal_bind,
            embedding,
            job_queue_capacity,
            job_concurrency,
            api_key,
            rate_limit_rps,
        })
    }
}
