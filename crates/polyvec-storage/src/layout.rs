//! Persistence layout: one directory per collection.
//!
//! ```text
//! <collection-dir>/
//!   catalog.redb        # redb: collections/documents/chunks/vectors/meta
//!   index.hnsw.graph    # hnsw_rs graph dump  (absent until first checkpoint)
//!   index.hnsw.data     # hnsw_rs vector data  (absent until first checkpoint)
//!   sidecar.json        # version + checksums + generation (see [`Sidecar`])
//! ```
//!
//! The sidecar is Polyvec's integrity gate: `hnsw_rs`'s loader asserts on
//! magic/dimension (panics on corrupt input), so we verify the SHA-256
//! checksums *before* handing the files to `hnsw_rs`. Corrupt dumps become
//! [`Error::Corrupt`](polyvec_core::Error::Corrupt), never a panic.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use polyvec_core::{Error, INDEX_FORMAT_VERSION, Result};

/// Base name of the ANN dump files (`<basename>.hnsw.{graph,data}`).
pub const DUMP_BASENAME: &str = "index";
/// Temporary base name used while dumping (renamed atomically after).
pub const DUMP_TMP_BASENAME: &str = "index.tmp";
/// Sidecar file name.
pub const SIDECAR_NAME: &str = "sidecar.json";
/// Catalog file name.
pub const CATALOG_NAME: &str = "catalog.redb";

/// Integrity + version record for the ANN dump files.
///
/// Written after every successful dump (temp file + atomic rename), read on
/// every open. `dump_present == false` means "no dump yet" (fresh or empty
/// collection) — the index is then rebuilt from the catalog.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sidecar {
    /// [`INDEX_FORMAT_VERSION`] — mismatch is [`Error::Corrupt`].
    pub format_version: u32,
    /// Collection dimension the dump was built for.
    pub dimension: u32,
    /// Metric name (`"l2"` / `"cosine"`).
    pub metric: String,
    /// Effective dump basename (`hnsw_rs` may uniquify; normally `"index"`).
    pub basename: String,
    /// Whether dump files exist for this sidecar.
    pub dump_present: bool,
    /// SHA-256 of the `.hnsw.graph` file (`""` when `!dump_present`).
    pub sha256_graph: String,
    /// SHA-256 of the `.hnsw.data` file (`""` when `!dump_present`).
    pub sha256_data: String,
    /// Catalog generation the dump was built from.
    pub generation: u64,
    /// Live + tombstoned points contained in the dump.
    pub point_count: u64,
    /// Tombstoned points contained in the dump.
    pub tombstone_count: u64,
}

impl Sidecar {
    /// Fresh sidecar for a collection with no dump yet.
    #[must_use]
    pub fn empty(dimension: u32, metric: &str) -> Self {
        Self {
            format_version: INDEX_FORMAT_VERSION,
            dimension,
            metric: metric.to_string(),
            basename: DUMP_BASENAME.to_string(),
            dump_present: false,
            sha256_graph: String::new(),
            sha256_data: String::new(),
            generation: 0,
            point_count: 0,
            tombstone_count: 0,
        }
    }
}

/// Paths of the dump files for `basename` in `dir`.
pub fn dump_paths(dir: &Path, basename: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{basename}.hnsw.graph")),
        dir.join(format!("{basename}.hnsw.data")),
    )
}

/// Reads the sidecar. `Ok(None)` when the file is absent; parse failures are
/// [`Error::Corrupt`] (a half-written sidecar is indistinguishable from a
/// corrupt one — both must stop the open).
///
/// # Errors
/// [`Error::Corrupt`] on unreadable JSON; [`Error::Storage`] on I/O failure.
pub fn read_sidecar(dir: &Path) -> Result<Option<Sidecar>> {
    let path = dir.join(SIDECAR_NAME);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| Error::Storage {
        detail: format!("read sidecar {}: {e}", path.display()),
    })?;
    serde_json::from_str(&text).map_err(|e| Error::Corrupt {
        detail: format!("sidecar {} is not valid JSON: {e}", path.display()),
    })
}

/// Writes the sidecar via temp file + atomic rename.
pub fn write_sidecar(dir: &Path, sidecar: &Sidecar) -> Result<()> {
    let text = serde_json::to_string_pretty(sidecar).map_err(|e| Error::Storage {
        detail: format!("encode sidecar: {e}"),
    })?;
    let tmp = dir.join(format!("{SIDECAR_NAME}.tmp"));
    let dst = dir.join(SIDECAR_NAME);
    fs::write(&tmp, text).map_err(|e| Error::Storage {
        detail: format!("write sidecar temp file: {e}"),
    })?;
    fs::rename(&tmp, &dst).map_err(|e| Error::Storage {
        detail: format!("rename sidecar into place: {e}"),
    })?;
    Ok(())
}

/// SHA-256 hex digest of a file.
///
/// # Errors
/// [`Error::Storage`] on I/O failure.
pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|e| Error::Storage {
        detail: format!("read {} for checksum: {e}", path.display()),
    })?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Verifies the dump files against the sidecar.
///
/// # Errors
/// [`Error::Corrupt`] with a recovery hint when a checksum mismatches.
/// Callers must only invoke this when `sidecar.dump_present` is true and
/// both files exist.
pub fn verify_dump(dir: &Path, sidecar: &Sidecar) -> Result<()> {
    let (graph, data) = dump_paths(dir, &sidecar.basename);
    let graph_sum = sha256_file(&graph)?;
    if graph_sum != sidecar.sha256_graph {
        return Err(Error::Corrupt {
            detail: format!(
                "checksum mismatch on {}: expected {}, got {graph_sum}. \
                 To recover, delete {}.hnsw.graph, {}.hnsw.data and sidecar.json, \
                 then reopen to rebuild the index from the catalog.",
                graph.display(),
                sidecar.sha256_graph,
                sidecar.basename,
                sidecar.basename
            ),
        });
    }
    let data_sum = sha256_file(&data)?;
    if data_sum != sidecar.sha256_data {
        return Err(Error::Corrupt {
            detail: format!(
                "checksum mismatch on {}: expected {}, got {data_sum}. \
                 To recover, delete {}.hnsw.graph, {}.hnsw.data and sidecar.json, \
                 then reopen to rebuild the index from the catalog.",
                data.display(),
                sidecar.sha256_data,
                sidecar.basename,
                sidecar.basename
            ),
        });
    }
    Ok(())
}
