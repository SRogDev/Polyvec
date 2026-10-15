//! Ingestion job records: durable, inspectable, idempotent.
//!
//! An [`IngestionJob`] tracks one unit of ingestion work (ingest or delete)
//! from submission to a terminal state. Records live in redb
//! (`{data_dir}/jobs.redb`), so a crash or restart never loses them: jobs
//! found in `Running`/`Retrying` when the store opens are marked `Failed`
//! with an explanatory error (their input payload lives only in memory, so
//! they cannot resume — resubmission is the remedy and the idempotency key
//! makes it safe).
//!
//! State machine: `Pending → Running → Done | Failed`, with
//! `Running → Retrying → Running …` for transient failures (bounded by
//! `max_attempts`). Terminal states are never left — a failed job stays
//! failed with its error detail; partial progress is recorded in
//! [`AffectedRecords`] but never reported as complete.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use polyvec_core::{CollectionId, Error, Result};

/// `jobs`: job id → JSON [`IngestionJob`].
const T_JOBS: TableDefinition<&str, &str> = TableDefinition::new("jobs");
/// `idempotency`: idempotency key → latest job id.
const T_IDEMPOTENCY: TableDefinition<&str, &str> = TableDefinition::new("idempotency");
/// `doc_records`: document id → JSON [`DocRecord`].
const T_DOC_RECORDS: TableDefinition<&str, &str> = TableDefinition::new("doc_records");
/// `meta`: single-row metadata (`job_counter` → next id).
const T_META: TableDefinition<&str, &str> = TableDefinition::new("meta");

/// What a job does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobOp {
    /// Convert → canonicalize → chunk → embed → store.
    Ingest,
    /// Delete the document and cascade to its chunks/vectors.
    Delete,
}

impl JobOp {
    /// Wire string for the job's operation.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            JobOp::Ingest => "ingest",
            JobOp::Delete => "delete",
        }
    }
}

/// Where a job stands. Terminal states (`Done`, `Failed`) are final.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Accepted, waiting for a worker.
    Pending,
    /// A worker is executing it.
    Running,
    /// Transient failure; scheduled for another attempt.
    Retrying,
    /// Finished successfully (see `affected`; `skipped` when unchanged).
    Done,
    /// Finished unsuccessfully. Never retried implicitly; the error detail
    /// is preserved for inspection.
    Failed,
}

impl JobState {
    /// True for [`JobState::Done`] and [`JobState::Failed`].
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Done | JobState::Failed)
    }

    /// Wire string for the job's state.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Running => "running",
            JobState::Retrying => "retrying",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }
}

/// Error detail preserved on failed/retrying jobs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobErrorInfo {
    /// `"transient"` or `"permanent"`.
    pub kind: String,
    /// Human-readable cause (never carries secrets).
    pub message: String,
    /// Which attempt produced this error (1-based).
    pub attempt: u32,
}

/// What a finished job did (or partially did, on failure).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedRecords {
    /// Affected document id.
    pub document_id: String,
    /// Document version after the job (unchanged on skip).
    pub version: u64,
    /// Chunks written (0 on skip/delete-miss).
    pub chunks: u64,
    /// Vectors written (0 on skip/delete-miss).
    pub vectors: u64,
    /// True when re-ingestion found unchanged content (no version bump).
    pub skipped: bool,
}

/// Durable ingestion provenance per document: the "never discard source
/// attribution" home. Written when an ingest job completes; removed on
/// delete. (A `_polyvec.*` subset is also stamped on the document metadata
/// for visibility via the documents API.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRecord {
    /// Document id.
    pub document_id: String,
    /// Owning collection.
    pub collection: String,
    /// Durable source identity.
    pub source: String,
    /// Version this record describes.
    pub version: u64,
    /// Hex SHA-256 of the raw input bytes.
    pub original_hash: String,
    /// Hex SHA-256 of the canonical markdown.
    pub canonical_hash: String,
    /// Chunking version used.
    pub chunking_version: u32,
    /// Converter name, e.g. `"markit"`.
    pub converter: String,
    /// Source format label, e.g. `"html"`.
    pub source_format: String,
    /// Original filename, if known.
    pub filename: Option<String>,
}

/// A durable ingestion job record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestionJob {
    /// Job id (`job-00000001`, …).
    pub id: String,
    /// Target collection.
    pub collection: CollectionId,
    /// Durable source identity.
    pub source: String,
    /// What the job does.
    pub op: JobOp,
    /// Current state.
    pub state: JobState,
    /// Attempts so far (1-based once running).
    pub attempts: u32,
    /// Attempt budget; transient failures beyond this fail the job.
    pub max_attempts: u32,
    /// Unix seconds when created.
    pub created_at_secs: u64,
    /// Unix seconds of the last state change.
    pub updated_at_secs: u64,
    /// Latest error detail (on `Retrying`/`Failed`).
    pub error: Option<JobErrorInfo>,
    /// Idempotency key: duplicate submissions return the existing job.
    pub idempotency_key: String,
    /// What the job did (on `Done`; partial on `Failed`).
    pub affected: Option<AffectedRecords>,
}

/// Job id newtype.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JobId(String);

impl JobId {
    /// Wraps a raw id string.
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrows the id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn storage_error(e: impl std::fmt::Display) -> Error {
    Error::Storage {
        detail: format!("job store: {e}"),
    }
}

/// Persistent job records backed by redb.
///
/// One database per data directory (`jobs.redb`); shared behind a `Mutex`
/// by the runner and the HTTP handlers (redb allows a single writer).
pub struct JobStore {
    db: Database,
}

impl JobStore {
    /// Opens (creating) the job database at `path`.
    ///
    /// Crash recovery: jobs found in `Running` or `Retrying` are marked
    /// `Failed` — their input payload lives only in memory, so they cannot
    /// resume after a restart. The error says so and names resubmission as
    /// the remedy; the idempotency key makes resubmitting safe.
    ///
    /// # Errors
    /// [`Error::Storage`] if the database cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let db = Database::create(path).map_err(storage_error)?;
        let store = Self { db };
        // Create all tables up front: opening a missing table on a READ
        // transaction is an error in redb, and readers must not fail just
        // because no writer has run yet.
        let write = store.db.begin_write().map_err(storage_error)?;
        write.open_table(T_JOBS).map_err(storage_error)?;
        write.open_table(T_IDEMPOTENCY).map_err(storage_error)?;
        write.open_table(T_DOC_RECORDS).map_err(storage_error)?;
        write.open_table(T_META).map_err(storage_error)?;
        write.commit().map_err(storage_error)?;
        store.fail_inflight()?;
        Ok(store)
    }

    /// Creates a job in `Pending` state with a fresh id.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn create_job(
        &self,
        collection: &CollectionId,
        source: &str,
        op: JobOp,
        idempotency_key: &str,
        max_attempts: u32,
    ) -> Result<IngestionJob> {
        let write = self.db.begin_write().map_err(storage_error)?;
        let job = Self::insert_job(&write, collection, source, op, idempotency_key, max_attempts)?;
        write.commit().map_err(storage_error)?;
        Ok(job)
    }

    /// Builds and inserts a job row inside an open write transaction.
    fn insert_job(
        write: &redb::WriteTransaction,
        collection: &CollectionId,
        source: &str,
        op: JobOp,
        idempotency_key: &str,
        max_attempts: u32,
    ) -> Result<IngestionJob> {
        let now = now_secs();
        let id = {
            let mut meta = write.open_table(T_META).map_err(storage_error)?;
            let next: u64 = meta
                .get("job_counter")
                .map_err(storage_error)?
                .map(|g| g.value().parse().unwrap_or(0))
                .unwrap_or(0)
                + 1;
            meta.insert("job_counter", next.to_string().as_str())
                .map_err(storage_error)?;
            format!("job-{next:08}")
        };
        let job = IngestionJob {
            id: id.clone(),
            collection: collection.clone(),
            source: source.to_string(),
            op,
            state: JobState::Pending,
            attempts: 0,
            max_attempts: max_attempts.max(1),
            created_at_secs: now,
            updated_at_secs: now,
            error: None,
            idempotency_key: idempotency_key.to_string(),
            affected: None,
        };
        {
            let mut jobs = write.open_table(T_JOBS).map_err(storage_error)?;
            let json = serde_json::to_string(&job).map_err(storage_error)?;
            jobs.insert(id.as_str(), json.as_str())
                .map_err(storage_error)?;
        }
        {
            let mut idem = write.open_table(T_IDEMPOTENCY).map_err(storage_error)?;
            idem.insert(idempotency_key, id.as_str())
                .map_err(storage_error)?;
        }
        Ok(job)
    }

    /// Atomically finds or creates a job for an idempotency key.
    ///
    /// If a non-terminal (`Pending`/`Running`/`Retrying`) job already holds
    /// the key, it is returned with `created = false` — the submitter should
    /// poll that job instead of duplicating the work. Otherwise a fresh
    /// `Pending` job is created (overwriting the key mapping) and returned
    /// with `created = true`. Terminal jobs never block resubmission: the
    /// pipeline's content-hash check makes re-ingesting unchanged content a
    /// cheap, side-effect-free skip.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn find_or_create(
        &self,
        collection: &CollectionId,
        source: &str,
        op: JobOp,
        idempotency_key: &str,
        max_attempts: u32,
    ) -> Result<(IngestionJob, bool)> {
        let write = self.db.begin_write().map_err(storage_error)?;
        let existing: Option<IngestionJob> = {
            let idem = write.open_table(T_IDEMPOTENCY).map_err(storage_error)?;
            let jobs = write.open_table(T_JOBS).map_err(storage_error)?;
            match idem.get(idempotency_key).map_err(storage_error)? {
                None => None,
                Some(id) => match jobs.get(id.value()).map_err(storage_error)? {
                    None => None,
                    Some(guard) => {
                        Some(serde_json::from_str(guard.value()).map_err(storage_error)?)
                    }
                },
            }
        };
        if let Some(job) = existing {
            if !job.state.is_terminal() {
                write.commit().map_err(storage_error)?;
                return Ok((job, false));
            }
        }
        let job = Self::insert_job(
            &write,
            collection,
            source,
            op,
            idempotency_key,
            max_attempts,
        )?;
        write.commit().map_err(storage_error)?;
        Ok((job, true))
    }

    /// Returns the latest job for an idempotency key, if any.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn find_by_idempotency(&self, key: &str) -> Result<Option<IngestionJob>> {        let read = self.db.begin_read().map_err(storage_error)?;
        let idem = read.open_table(T_IDEMPOTENCY).map_err(storage_error)?;
        let Some(id) = idem.get(key).map_err(storage_error)? else {
            return Ok(None);
        };
        let id = id.value().to_string();
        drop(idem);
        drop(read);
        self.get(&JobId::new(id))
    }

    /// Fetches a job by id.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn get(&self, id: &JobId) -> Result<Option<IngestionJob>> {
        let read = self.db.begin_read().map_err(storage_error)?;
        let jobs = read.open_table(T_JOBS).map_err(storage_error)?;
        let Some(guard) = jobs.get(id.as_str()).map_err(storage_error)? else {
            return Ok(None);
        };
        let job: IngestionJob = serde_json::from_str(guard.value()).map_err(storage_error)?;
        Ok(Some(job))
    }

    /// Persists a job record (insert or overwrite), bumping `updated_at_secs`.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn update(&self, job: &IngestionJob) -> Result<()> {
        let mut job = job.clone();
        job.updated_at_secs = now_secs();
        let write = self.db.begin_write().map_err(storage_error)?;
        {
            let mut jobs = write.open_table(T_JOBS).map_err(storage_error)?;
            let json = serde_json::to_string(&job).map_err(storage_error)?;
            jobs.insert(job.id.as_str(), json.as_str())
                .map_err(storage_error)?;
        }
        write.commit().map_err(storage_error)?;
        Ok(())
    }

    /// Lists jobs for a collection, newest first, capped at `limit`.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn list_for_collection(
        &self,
        collection: &CollectionId,
        limit: usize,
    ) -> Result<Vec<IngestionJob>> {
        let read = self.db.begin_read().map_err(storage_error)?;
        let jobs = read.open_table(T_JOBS).map_err(storage_error)?;
        let mut out = Vec::new();
        for entry in jobs.iter().map_err(storage_error)? {
            let (_, v) = entry.map_err(storage_error)?;
            let job: IngestionJob = serde_json::from_str(v.value()).map_err(storage_error)?;
            if job.collection == *collection {
                out.push(job);
            }
        }
        // Ids are zero-padded and monotonic: reverse lexicographic = newest first.
        out.sort_by(|a, b| b.id.cmp(&a.id));
        out.truncate(limit);
        Ok(out)
    }

    /// Stores/overwrites the ingestion provenance record for a document.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn put_doc_record(&self, record: &DocRecord) -> Result<()> {
        let write = self.db.begin_write().map_err(storage_error)?;
        {
            let mut t = write.open_table(T_DOC_RECORDS).map_err(storage_error)?;
            let json = serde_json::to_string(record).map_err(storage_error)?;
            t.insert(record.document_id.as_str(), json.as_str())
                .map_err(storage_error)?;
        }
        write.commit().map_err(storage_error)?;
        Ok(())
    }

    /// Fetches the provenance record for a document, if any.
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn get_doc_record(&self, document_id: &str) -> Result<Option<DocRecord>> {
        let read = self.db.begin_read().map_err(storage_error)?;
        let t = read.open_table(T_DOC_RECORDS).map_err(storage_error)?;
        let Some(guard) = t.get(document_id).map_err(storage_error)? else {
            return Ok(None);
        };
        let record: DocRecord = serde_json::from_str(guard.value()).map_err(storage_error)?;
        Ok(Some(record))
    }

    /// Removes the provenance record for a document (on delete).
    ///
    /// # Errors
    /// [`Error::Storage`] on database failure.
    pub fn remove_doc_record(&self, document_id: &str) -> Result<()> {
        let write = self.db.begin_write().map_err(storage_error)?;
        {
            let mut t = write.open_table(T_DOC_RECORDS).map_err(storage_error)?;
            t.remove(document_id).map_err(storage_error)?;
        }
        write.commit().map_err(storage_error)?;
        Ok(())
    }

    /// Crash recovery: `Running`/`Retrying` jobs become `Failed`.
    ///
    /// The input payload (bytes to convert) lives only in memory, so an
    /// interrupted job cannot resume. Failing loudly — with an error that
    /// names resubmission as the remedy — beats silently dropping the work
    /// or pretending it completed.
    fn fail_inflight(&self) -> Result<()> {
        let write = self.db.begin_write().map_err(storage_error)?;
        let mut failed = 0u32;
        {
            let mut jobs = write.open_table(T_JOBS).map_err(storage_error)?;
            // Collect first: redb guards borrow the table.
            let mut pending_writes = Vec::new();
            for entry in jobs.iter().map_err(storage_error)? {
                let (k, v) = entry.map_err(storage_error)?;
                let mut job: IngestionJob =
                    serde_json::from_str(v.value()).map_err(storage_error)?;
                if matches!(job.state, JobState::Running | JobState::Retrying) {
                    job.state = JobState::Failed;
                    job.error = Some(JobErrorInfo {
                        kind: "permanent".to_string(),
                        message: "interrupted by restart; the input payload is not \
                                  persisted — resubmit to retry"
                            .to_string(),
                        attempt: job.attempts,
                    });
                    job.updated_at_secs = now_secs();
                    pending_writes.push((
                        k.value().to_string(),
                        serde_json::to_string(&job).map_err(storage_error)?,
                    ));
                    failed += 1;
                }
            }
            for (k, json) in pending_writes {
                jobs.insert(k.as_str(), json.as_str())
                    .map_err(storage_error)?;
            }
        }
        write.commit().map_err(storage_error)?;
        if failed > 0 {
            log::warn!("job store: marked {failed} in-flight job(s) as failed after restart");
        }
        Ok(())
    }
}
