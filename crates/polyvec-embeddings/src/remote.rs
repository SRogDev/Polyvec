//! Remote embedding adapter: generic OpenAI-compatible `/embeddings` endpoint.
//!
//! Configured from the environment (`POLYVEC_EMBEDDING_*`; see
//! [`RemoteEmbeddingProvider::from_env_in`]). Uses blocking HTTP and must
//! run on a blocking thread — the ingestion runner guarantees this.
//!
//! cost-guard mechanisms (see `docs/ingestion.md`):
//! * **Batching**: inputs are split into batches (default 64); one HTTP call
//!   per batch — never one call per text.
//! * **Retry classification**: 429/5xx/timeout → [`ErrorKind::Transient`];
//!   4xx (auth/validation) → [`ErrorKind::Permanent`]. Permanent failures
//!   fail the job fast instead of burning a retry budget.
//! * **`Retry-After` honored**: a 429's `Retry-After` (delta-seconds) is
//!   parsed and handed to the runner's backoff.

use std::time::Duration;

use polyvec_core::{MAX_DIMENSION, ModelTag};
use serde::{Deserialize, Serialize};

use crate::provider::{EmbedError, EmbedResult, EmbeddingProvider};

/// Default batch size: inputs per `/embeddings` call.
pub const DEFAULT_BATCH_SIZE: usize = 64;
/// Default HTTP timeout for one batch call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound for a server-suggested wait we will honor.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Generic OpenAI-compatible embeddings client (blocking HTTP).
///
/// The API key is held as a plain `String` and never appears in logs or in
/// the manual [`std::fmt::Debug`] impl below.
pub struct RemoteEmbeddingProvider {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: ModelTag,
    dimension: u32,
    batch_size: usize,
}

impl std::fmt::Debug for RemoteEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEmbeddingProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("dimension", &self.dimension)
            .field("batch_size", &self.batch_size)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    #[serde(default)]
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    #[serde(default)]
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

impl RemoteEmbeddingProvider {
    /// Builds the adapter.
    ///
    /// # Errors
    /// Returns a message if the base URL or key is empty, or the dimension
    /// is 0 or above [`MAX_DIMENSION`].
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: ModelTag,
        dimension: u32,
    ) -> Result<Self, String> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let api_key = api_key.into();
        if base_url.is_empty() {
            return Err("embedding base URL must not be empty".to_string());
        }
        if api_key.is_empty() {
            return Err("embedding API key must not be empty".to_string());
        }
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(format!(
                "embedding dimension must be 1..={MAX_DIMENSION}, got {dimension}"
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            client,
            base_url,
            api_key,
            model,
            dimension,
            batch_size: DEFAULT_BATCH_SIZE,
        })
    }

    /// Sets the max inputs per `/embeddings` call (≥ 1).
    #[must_use]
    pub fn with_batch_size(mut self, n: usize) -> Self {
        self.batch_size = n.max(1);
        self
    }

    /// Sets the per-batch HTTP timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        if let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
        {
            self.client = client;
        }
        self
    }

    /// Reads configuration from the process environment.
    ///
    /// # Errors
    /// Returns a message naming the first missing/malformed variable.
    pub fn from_env() -> Result<Self, String> {
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let refs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        Self::from_env_in(&refs)
    }

    /// Reads configuration from explicit `(key, value)` pairs (test seam;
    /// [`RemoteEmbeddingProvider::from_env`] is the real entry point).
    ///
    /// | Variable | Required | Meaning |
    /// |---|---|---|
    /// | `POLYVEC_EMBEDDING_BASE_URL` | yes | e.g. `https://api.openai.com/v1` |
    /// | `POLYVEC_EMBEDDING_API_KEY` | yes | bearer token (never logged) |
    /// | `POLYVEC_EMBEDDING_MODEL` | yes | model name sent as `model` |
    /// | `POLYVEC_EMBEDDING_MODEL_VERSION` | no (default `1.0`) | model version for the collection tag |
    /// | `POLYVEC_EMBEDDING_DIMENSION` | yes | must match the collection |
    /// | `POLYVEC_EMBEDDING_BATCH_SIZE` | no (default 64) | inputs per HTTP call |
    /// | `POLYVEC_EMBEDDING_TIMEOUT_SECS` | no (default 30) | per-batch timeout |
    ///
    /// # Errors
    /// Same as [`RemoteEmbeddingProvider::from_env`].
    pub fn from_env_in(vars: &[(&str, &str)]) -> Result<Self, String> {
        let get = |key: &str| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let req = |key: &str| {
            get(key)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{key} is required for the remote embedding provider"))
        };
        let base_url = req("POLYVEC_EMBEDDING_BASE_URL")?;
        let api_key = req("POLYVEC_EMBEDDING_API_KEY")?;
        let model_name = req("POLYVEC_EMBEDDING_MODEL")?;
        let model_version = get("POLYVEC_EMBEDDING_MODEL_VERSION").unwrap_or("1.0");
        let model =
            ModelTag::new(model_name, model_version).map_err(|e| format!("bad model tag: {e}"))?;
        let dimension: u32 = req("POLYVEC_EMBEDDING_DIMENSION")?
            .parse()
            .map_err(|_| "POLYVEC_EMBEDDING_DIMENSION must be an integer".to_string())?;
        let batch_size: usize = get("POLYVEC_EMBEDDING_BATCH_SIZE")
            .map(str::parse)
            .transpose()
            .map_err(|_| "POLYVEC_EMBEDDING_BATCH_SIZE must be an integer".to_string())?
            .unwrap_or(DEFAULT_BATCH_SIZE);
        let timeout_secs: u64 = get("POLYVEC_EMBEDDING_TIMEOUT_SECS")
            .map(str::parse)
            .transpose()
            .map_err(|_| "POLYVEC_EMBEDDING_TIMEOUT_SECS must be an integer".to_string())?
            .unwrap_or(DEFAULT_TIMEOUT.as_secs());
        Ok(Self::new(base_url, api_key, model, dimension)?
            .with_batch_size(batch_size)
            .with_timeout(Duration::from_secs(timeout_secs.max(1))))
    }

    /// Embeds one batch with a single HTTP call.
    fn embed_batch(&self, batch: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url);
        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&EmbeddingsRequest {
                model: &self.model.name,
                input: batch,
            })
            .send();
        let response = match response {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(EmbedError::transient(format!(
                    "embedding request timed out: {e}"
                )));
            }
            Err(e) => {
                return Err(EmbedError::transient(format!(
                    "embedding request failed: {e}"
                )));
            }
        };
        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(EmbedError::rate_limited(
                format!("embedding provider rate-limited (429) at {url}"),
                parse_retry_after(response.headers()),
            ));
        }
        if status.is_server_error() {
            return Err(EmbedError::transient(format!(
                "embedding provider returned {status} at {url}"
            )));
        }
        if status.is_client_error() {
            return Err(EmbedError::permanent(format!(
                "embedding provider rejected the request ({status}) at {url}"
            )));
        }
        let parsed: EmbeddingsResponse = response.json().map_err(|e| {
            EmbedError::permanent(format!("embedding provider returned bad JSON: {e}"))
        })?;
        if parsed.data.len() != batch.len() {
            return Err(EmbedError::permanent(format!(
                "embedding provider returned {} vectors for {} inputs",
                parsed.data.len(),
                batch.len()
            )));
        }
        // The API may return data out of order; restore input order.
        let mut by_index: Vec<(usize, Vec<f32>)> = parsed
            .data
            .into_iter()
            .map(|d| (d.index, d.embedding))
            .collect();
        by_index.sort_by_key(|(i, _)| *i);
        let mut out = Vec::with_capacity(batch.len());
        for (i, embedding) in by_index {
            if embedding.len() != self.dimension as usize {
                return Err(EmbedError::permanent(format!(
                    "embedding provider returned dimension {} for input {i}, expected {}",
                    embedding.len(),
                    self.dimension
                )));
            }
            if embedding.iter().any(|x| !x.is_finite()) {
                return Err(EmbedError::permanent(format!(
                    "embedding provider returned non-finite values for input {i}"
                )));
            }
            out.push(embedding);
        }
        Ok(out)
    }
}

/// Parses a `Retry-After` header (delta-seconds only; HTTP dates are rare
/// here and treated as absent), capped at [`MAX_RETRY_AFTER`].
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: u64 = value.trim().parse().ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

impl EmbeddingProvider for RemoteEmbeddingProvider {
    fn embed(&self, texts: &[&str]) -> EmbedResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch_size) {
            out.extend(self.embed_batch(batch)?);
        }
        Ok(out)
    }

    fn model_id(&self) -> ModelTag {
        self.model.clone()
    }

    fn dimension(&self) -> u32 {
        self.dimension
    }
}
