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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// Default bind address: localhost only.
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";
/// Default data directory (relative to the server's working directory).
pub const DEFAULT_DATA_DIR: &str = "./polyvec-data";
/// Default maximum JSON body: 16 MiB (a 10k-vector batch at dim 384 is ~15 MiB JSON).
pub const DEFAULT_JSON_LIMIT_BYTES: usize = 16 * 1024 * 1024;

/// Runtime configuration of the HTTP server.
#[derive(Clone, Debug)]
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

        Ok(Self {
            bind,
            data_dir,
            json_limit,
            workers,
            allow_nonlocal_bind,
        })
    }
}
