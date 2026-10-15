//! Shared helpers for the `bench_report` binary: scratch dirs, configs,
//! environment facts. Imported by `main.rs`, `layer_b.rs`, `layer_c.rs`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use polyvec_core::{CollectionConfig, CollectionId, DistanceMetric, Vector, VectorId};
use serde_json::{Value, json};

use polyvec_bench::fixtures::unit_vectors;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Fresh scratch directory (unique per call).
pub fn scratch(name: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "polyvec-benchrep-{name}-{pid}-{n}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Standard benchmark collection config (cosine, default HNSW params).
pub fn test_config(dim: u32) -> CollectionConfig {
    CollectionConfig::new(
        CollectionId::new("bench").unwrap(),
        dim,
        DistanceMetric::Cosine,
    )
    .unwrap()
}

/// `n` typed vectors of `dim` from `seed` (ids `v-{i:06}`).
pub fn typed_vectors(n: usize, dim: usize, seed: u64) -> Vec<Vector> {
    let dim_u32 = u32::try_from(dim).expect("benchmark dimensions fit in u32");
    unit_vectors(n, dim, seed)
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            Vector::new(VectorId::parse(&format!("v-{i:06}")).unwrap(), v, dim_u32).unwrap()
        })
        .collect()
}

/// Engine commit under test.
pub fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

/// Hardware facts for the report.
pub fn hardware() -> Value {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split(':').nth(1))
        .map_or("unknown", str::trim)
        .to_string();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mem_kb = meminfo
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    json!({
        "cpu_model": model,
        "cpu_threads": std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        "mem_total_kb": mem_kb,
        "os": std::env::consts::OS,
    })
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
