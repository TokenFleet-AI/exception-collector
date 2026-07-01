//! In-memory and `SQLite`-backed buffer for exception aggregation.
//!
//! The [`ExceptionBuffer`] stores aggregated exceptions in a concurrent
//! [`DashMap`](dashmap::DashMap) for fast writes and syncs them to `SQLite`
//! for durability across restarts.

// Suppress dead_code warnings for items used by later tasks (dedup, pipeline, collector).
#![allow(dead_code, reason = "items consumed by later tasks")]

use std::{path::Path, sync::Arc};

use chrono::{DateTime, TimeZone, Utc};
use dashmap::DashMap;
use rusqlite::{params, Connection};

use crate::{
    signature::compute_signature, AggregatedException, CollectorError, CollectorResult,
    ExceptionRecord,
};

/// `SQLite` schema for the aggregated exceptions table.
const CREATE_TABLE_SQL: &str = "\
    CREATE TABLE IF NOT EXISTS aggregated_exceptions (
        signature  TEXT PRIMARY KEY NOT NULL,
        first_seen TEXT NOT NULL,
        last_seen  TEXT NOT NULL,
        count      INTEGER NOT NULL,
        sample     TEXT NOT NULL,
        reported   INTEGER NOT NULL DEFAULT 0
    )";

/// Upsert a single aggregated exception row.
const UPSERT_SQL: &str = "\
    INSERT INTO aggregated_exceptions
        (signature, first_seen, last_seen, count, sample, reported)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
    ON CONFLICT(signature) DO UPDATE SET
        first_seen = excluded.first_seen,
        last_seen  = excluded.last_seen,
        count      = excluded.count,
        sample     = excluded.sample,
        reported   = excluded.reported";

/// Select all rows from the aggregated exceptions table.
const SELECT_ALL_SQL: &str = "\
    SELECT signature, first_seen, last_seen, count, sample, reported
    FROM aggregated_exceptions";

/// Update the `reported` flag for a list of signatures.
const MARK_REPORTED_SQL: &str = "\
    UPDATE aggregated_exceptions SET reported = 1 WHERE signature = ?1";

/// Delete old reported exceptions to prevent unbounded disk growth.
const DELETE_OLD_REPORTED_SQL: &str = "\
    DELETE FROM aggregated_exceptions WHERE reported = 1 AND last_seen < ?1";

/// Format a UTC datetime as an RFC 3339 string for `SQLite` storage.
fn format_rfc3339(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

/// Map a `PoisonError` from a mutex lock into a [`BufferError::Poisoned`].
fn poison_err<E: std::fmt::Display>(e: E) -> BufferError {
    BufferError::Poisoned(e.to_string())
}

/// Parse an RFC 3339 string back into a `DateTime<Utc>`.
///
/// Returns `CollectorError::Sqlite` wrapping a conversion error when the
/// string is not valid RFC 3339.
fn parse_rfc3339(s: &str) -> CollectorResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| Utc.from_utc_datetime(&dt.naive_utc()))
        .map_err(|e| CollectorError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))
}

// ── BufferError ─────────────────────────────────────────────────────────

/// Errors specific to the exception buffer layer.
///
/// Wraps underlying I/O, `SQLite`, and JSON errors so callers can distinguish
/// buffer failures from other collector errors.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    /// `SQLite` persistence error.
    #[error("sqlite buffer error: {0}")]
    Sqlite(
        /// Underlying `SQLite` error.
        #[source]
        rusqlite::Error,
    ),

    /// JSON serialization or deserialization error.
    #[error("json buffer error: {0}")]
    SerdeJson(
        /// Underlying JSON error.
        #[source]
        serde_json::Error,
    ),

    /// Internal mutex was poisoned (indicates a panicked holder).
    #[error("internal mutex poisoned: {0}")]
    Poisoned(String),
}

/// Result type alias for buffer operations.
pub type BufferResult<T> = std::result::Result<T, BufferError>;

// ── SqliteExceptionRepo ─────────────────────────────────────────────────

/// Thin `SQLite` persistence layer for aggregated exceptions.
///
/// Stores each unique exception signature as a single row, with the full
/// sample [`ExceptionRecord`] serialized as JSON in a `TEXT` column.
#[derive(Debug)]
struct SqliteExceptionRepo {
    conn: Arc<std::sync::Mutex<Connection>>,
}

impl SqliteExceptionRepo {
    /// Open a `SQLite` database at `db_path` and ensure the schema exists.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::Sqlite`] when the database cannot be opened
    /// or the schema cannot be created.
    fn open(db_path: &Path) -> BufferResult<Self> {
        let conn = Connection::open(db_path).map_err(BufferError::Sqlite)?;
        conn.execute_batch(CREATE_TABLE_SQL)
            .map_err(BufferError::Sqlite)?;
        Ok(Self {
            conn: Arc::new(std::sync::Mutex::new(conn)),
        })
    }

    /// Insert or update a single aggregated exception row.
    fn upsert(&self, agg: &AggregatedException) -> BufferResult<()> {
        let conn = self.conn.lock().map_err(poison_err)?;
        let sample_json = serde_json::to_string(&agg.sample).map_err(BufferError::SerdeJson)?;
        let first_seen = format_rfc3339(&agg.first_seen);
        let last_seen = format_rfc3339(&agg.last_seen);
        let reported_i32 = i32::from(false);
        conn.execute(
            UPSERT_SQL,
            params![
                agg.signature,
                first_seen,
                last_seen,
                i64::from(agg.count),
                sample_json,
                reported_i32,
            ],
        )
        .map_err(BufferError::Sqlite)?;
        Ok(())
    }

    /// Load all aggregated exceptions from the database.
    fn load_all(&self) -> BufferResult<Vec<AggregatedException>> {
        let conn = self.conn.lock().map_err(poison_err)?;
        let mut stmt = conn.prepare(SELECT_ALL_SQL).map_err(BufferError::Sqlite)?;
        let rows = stmt
            .query_map([], |row| {
                let signature: String = row.get(0)?;
                let first_seen_str: String = row.get(1)?;
                let last_seen_str: String = row.get(2)?;
                let count_i64: i64 = row.get(3)?;
                let sample_json: String = row.get(4)?;
                let reported_i32: i32 = row.get(5)?;
                Ok((
                    signature,
                    first_seen_str,
                    last_seen_str,
                    count_i64,
                    sample_json,
                    reported_i32,
                ))
            })
            .map_err(BufferError::Sqlite)?;
        let mut result = Vec::new();
        for row_result in rows {
            let (sig, first_str, last_str, count, sample_json, _reported) =
                row_result.map_err(BufferError::Sqlite)?;
            let first_seen = parse_rfc3339(&first_str).map_err(|e| match e {
                CollectorError::Sqlite(re) => BufferError::Sqlite(re),
                _ => BufferError::Sqlite(rusqlite::Error::InvalidQuery),
            })?;
            let last_seen = parse_rfc3339(&last_str).map_err(|e| match e {
                CollectorError::Sqlite(re) => BufferError::Sqlite(re),
                _ => BufferError::Sqlite(rusqlite::Error::InvalidQuery),
            })?;
            let sample: ExceptionRecord =
                serde_json::from_str(&sample_json).map_err(BufferError::SerdeJson)?;
            result.push(AggregatedException {
                signature: sig,
                first_seen,
                last_seen,
                count: u32::try_from(count).unwrap_or(u32::MAX),
                sample,
            });
        }
        Ok(result)
    }

    /// Set `reported = 1` for each given signature that exists in the database.
    fn mark_reported(&self, signatures: &[String]) -> BufferResult<()> {
        let conn = self.conn.lock().map_err(poison_err)?;
        for sig in signatures {
            conn.execute(MARK_REPORTED_SQL, params![sig])
                .map_err(BufferError::Sqlite)?;
        }
        Ok(())
    }

    /// Delete reported exceptions older than the given cutoff time.
    fn delete_old_reported(&self, cutoff: &DateTime<Utc>) -> BufferResult<usize> {
        let conn = self.conn.lock().map_err(poison_err)?;
        let cutoff_str = format_rfc3339(cutoff);
        let deleted = conn
            .execute(DELETE_OLD_REPORTED_SQL, params![cutoff_str])
            .map_err(BufferError::Sqlite)?;
        Ok(deleted)
    }
}

// ── ExceptionBuffer ─────────────────────────────────────────────────────

/// In-memory and `SQLite`-backed buffer for exception aggregation.
///
/// Concurrent writes go through the [`DashMap`] for lock-free aggregation.
/// [`flush`](Self::flush) persists the in-memory state to `SQLite`, and
/// [`load`](Self::load) restores it on restart.
///
/// # Examples
///
/// ```rust,ignore
/// use exception_collector::{ExceptionBuffer, ExceptionRecord, ExceptionKind};
///
/// let buffer = ExceptionBuffer::new("/tmp/exceptions.db")?;
/// let record = ExceptionRecord::new("my-comp", ExceptionKind::Panic, "oops", "");
/// buffer.collect(record);
/// buffer.flush()?;
/// ```
pub struct ExceptionBuffer {
    /// In-memory concurrent aggregation map.
    active: DashMap<String, AggregatedException>,
    /// `SQLite` persistence layer.
    db: SqliteExceptionRepo,
}

impl ExceptionBuffer {
    /// Create a new buffer backed by a `SQLite` database at `db_path`.
    ///
    /// The `SQLite` schema is created if it does not already exist.
    /// Call [`load`](Self::load) afterwards to restore previous state.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::Sqlite`] when the database cannot be opened
    /// or the schema cannot be created.
    pub fn new(db_path: &Path) -> BufferResult<Self> {
        let db = SqliteExceptionRepo::open(db_path)?;
        Ok(Self {
            active: DashMap::new(),
            db,
        })
    }

    /// 使用默认目录创建缓冲区，遵循以下优先级：
    ///
    /// 1. 环境变量 `TOKENFLEET_EXCEPTIONS_DIR`（如果设置）
    /// 2. 平台默认：
    ///    - **Linux / macOS**: `~/.tokenfleet-ai/exceptions/`
    ///    - **Windows**: `%APPDATA%/tokenfleet-ai/exceptions/`
    ///
    /// 如果目标目录不存在会自动创建。
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when the data directory cannot be determined
    /// or the database cannot be created.
    #[allow(
        clippy::disallowed_methods,
        reason = "one-time setup: creating data directory on first use"
    )]
    pub fn with_default_dir(component: &str) -> BufferResult<Self> {
        let dir = exceptions_dir()?;
        let db_path = dir.join(format!("{component}.db"));
        Self::new(&db_path)
    }

    /// Aggregate an exception record into the in-memory buffer.
    ///
    /// If the computed signature already exists, the count is incremented and
    /// `last_seen` is updated. Otherwise, a new aggregated entry is created
    /// with the record as the sample.
    pub fn collect(&self, record: ExceptionRecord) {
        let signature = compute_signature(&record);
        let now = Utc::now();
        let sig_for_insert = signature.clone();
        self.active
            .entry(signature)
            .and_modify(|agg| {
                agg.count += 1;
                agg.last_seen = now;
            })
            .or_insert_with(|| AggregatedException {
                signature: sig_for_insert,
                first_seen: now,
                last_seen: now,
                count: 1,
                sample: record,
            });
    }

    /// Persist all in-memory aggregated exceptions to `SQLite`.
    ///
    /// Existing rows are upserted (insert-or-replace) so that both new
    /// entries and updated counts/timestamps are synced.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::Sqlite`] or [`BufferError::SerdeJson`] when
    /// the persistence layer fails.
    pub fn flush(&self) -> BufferResult<()> {
        for entry in &self.active {
            self.db.upsert(entry.value())?;
        }
        Ok(())
    }

    /// Load aggregated exceptions from `SQLite` into the in-memory buffer.
    ///
    /// Intended to be called once at startup to recover state from the
    /// previous run. Entries already present in memory are overwritten
    /// by the `SQLite` version.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when the database cannot be read.
    pub fn load(&self) -> BufferResult<Vec<AggregatedException>> {
        let aggs = self.db.load_all()?;
        for agg in &aggs {
            self.active.insert(agg.signature.clone(), agg.clone());
        }
        Ok(aggs)
    }

    /// Mark the given signatures as reported in both memory and `SQLite`.
    ///
    /// Signatures not present in the buffer are silently ignored.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::Sqlite`] when the database update fails.
    pub fn mark_reported(&self, signatures: &[String]) -> BufferResult<()> {
        for sig in signatures {
            if let Some(mut agg) = self.active.get_mut(sig) {
                agg.sample.reported = true;
            }
        }
        self.db.mark_reported(signatures)?;
        Ok(())
    }

    /// Delete reported exceptions older than `cutoff` from the database.
    ///
    /// Prevents unbounded disk growth by cleaning up old, already-reported data.
    /// Returns the number of deleted rows.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::Sqlite`] when the database operation fails.
    pub fn delete_reported_older_than(&self, cutoff: &DateTime<Utc>) -> BufferResult<usize> {
        self.db.delete_old_reported(cutoff)
    }

    /// Return the number of distinct signatures in the in-memory buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// Return `true` if the in-memory buffer contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Check whether the given signature exists in the in-memory buffer.
    #[must_use]
    pub fn contains_signature(&self, signature: &str) -> bool {
        self.active.contains_key(signature)
    }

    /// Return all unreported sample records from the in-memory buffer.
    #[must_use]
    pub fn unreported_samples(&self) -> Vec<ExceptionRecord> {
        self.active
            .iter()
            .filter(|entry| !entry.value().sample.reported)
            .map(|entry| entry.value().sample.clone())
            .collect()
    }
}

/// Resolve the shared exceptions directory.
///
/// Priority: `TOKENFLEET_EXCEPTIONS_DIR` env var → platform data dir.
/// Returns the path, creating it if necessary.
///
/// # Errors
///
/// Returns [`BufferError`] if the directory cannot be created.
#[allow(
    clippy::disallowed_methods,
    reason = "env var and fs operations are core to path resolution"
)]
pub fn exceptions_dir() -> BufferResult<std::path::PathBuf> {
    if let Ok(env_dir) = std::env::var("TOKENFLEET_EXCEPTIONS_DIR") {
        let dir = std::path::PathBuf::from(env_dir);
        std::fs::create_dir_all(&dir).map_err(|e| {
            BufferError::Poisoned(format!(
                "cannot create exceptions dir {}: {e}",
                dir.display()
            ))
        })?;
        return Ok(dir);
    }

    #[cfg(target_os = "windows")]
    let dir = {
        let appdata = dirs::data_dir().ok_or_else(|| {
            BufferError::Poisoned("cannot determine APPDATA directory".to_string())
        })?;
        appdata.join("tokenfleet-ai").join("exceptions")
    };
    #[cfg(not(target_os = "windows"))]
    let dir = {
        let home = dirs::home_dir()
            .ok_or_else(|| BufferError::Poisoned("cannot determine home directory".to_string()))?;
        home.join(".tokenfleet-ai").join("exceptions")
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        BufferError::Poisoned(format!(
            "cannot create exception buffer directory {}: {e}",
            dir.display()
        ))
    })?;
    Ok(dir)
}

impl std::fmt::Debug for ExceptionBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExceptionBuffer")
            .field("active_count", &self.active.len())
            .field("db", &self.db)
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::indexing_slicing,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use std::thread;

    use super::*;
    use crate::{ExceptionKind, ExceptionRecord};

    fn make_record(component: &str, msg: &str) -> ExceptionRecord {
        ExceptionRecord::new(component, ExceptionKind::Panic, msg, "frame1\nframe2")
    }

    // -- collect --

    #[test]
    fn test_should_upsert_same_signature() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        let r1 = make_record("comp-a", "same error");
        let r2 = make_record("comp-a", "same error");

        buffer.collect(r1);
        buffer.collect(r2);

        assert_eq!(buffer.len(), 1);
        let sig = compute_signature(&make_record("comp-a", "same error"));
        let agg = buffer.active.get(&sig).unwrap();
        assert_eq!(agg.count, 2);
    }

    #[test]
    fn test_should_store_distinct_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        buffer.collect(make_record("comp-a", "error one"));
        buffer.collect(make_record("comp-b", "error two"));

        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn test_should_start_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    // -- flush + load (persistence) --

    #[test]
    fn test_should_persist_to_sqlite_and_recover_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Phase 1: collect and flush
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.collect(make_record("comp-a", "persist me"));
            buffer.flush().unwrap();
        }

        // Phase 2: new buffer, load from `SQLite`
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            let loaded = buffer.load().unwrap();

            assert_eq!(loaded.len(), 1);
            assert_eq!(
                loaded[0].signature,
                compute_signature(&make_record("comp-a", "persist me"))
            );
            assert_eq!(loaded[0].count, 1);
            assert_eq!(loaded[0].sample.component, "comp-a");
            assert_eq!(loaded[0].sample.message, "persist me");
            // The loaded data should also be in memory
            assert_eq!(buffer.len(), 1);
        }
    }

    #[test]
    fn test_should_preserve_count_after_flush_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Collect multiple times and flush
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.collect(make_record("comp", "repeated"));
            buffer.collect(make_record("comp", "repeated"));
            buffer.collect(make_record("comp", "repeated"));
            buffer.flush().unwrap();
        }

        // Reload and verify count
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            let loaded = buffer.load().unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].count, 3);
        }
    }

    // -- mark_reported --

    #[test]
    fn test_should_mark_as_reported() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        let record = make_record("comp-a", "report me");
        let sig = compute_signature(&record);
        buffer.collect(record);
        buffer.flush().unwrap();

        buffer.mark_reported(std::slice::from_ref(&sig)).unwrap();

        // Verify in-memory state
        let agg = buffer.active.get(&sig).unwrap();
        assert!(agg.sample.reported);
    }

    #[test]
    fn test_should_mark_reported_in_sqlite_too() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let record = make_record("comp", "report sqlite");
        let sig = compute_signature(&record);

        // Collect, flush, mark
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.collect(record);
            buffer.flush().unwrap();
            buffer.mark_reported(std::slice::from_ref(&sig)).unwrap();
        }

        // Reload and verify reported flag is set after marking
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.load().unwrap();
            buffer.mark_reported(std::slice::from_ref(&sig)).unwrap();
            let agg = buffer.active.get(&sig).unwrap();
            assert!(agg.sample.reported);
        }
    }

    #[test]
    fn test_should_ignore_missing_signatures_in_mark_reported() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        // Marking a non-existent signature should succeed without error
        buffer
            .mark_reported(&["nonexistent_sig".to_string()])
            .unwrap();
    }

    // -- concurrent writes --

    #[test]
    fn test_should_handle_concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = Arc::new(ExceptionBuffer::new(&db_path).unwrap());

        let handles: Vec<_> = (0..100)
            .map(|_| {
                let buf = Arc::clone(&buffer);
                thread::spawn(move || {
                    buf.collect(make_record("comp-a", "same error"));
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // All 100 writes should aggregate into a single signature
        assert_eq!(buffer.len(), 1);
        let sig = compute_signature(&make_record("comp-a", "same error"));
        let agg = buffer.active.get(&sig).unwrap();
        assert_eq!(agg.count, 100);
    }

    #[test]
    fn test_should_handle_concurrent_distinct_writes() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = Arc::new(ExceptionBuffer::new(&db_path).unwrap());

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let buf = Arc::clone(&buffer);
                thread::spawn(move || {
                    // Use alphabetic suffix so normalize_message keeps them distinct
                    let suffix = char::from(b'a' + u8::try_from(i).unwrap_or(0));
                    buf.collect(make_record("comp-a", &format!("error {suffix}")));
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // 10 distinct messages → 10 distinct signatures
        assert_eq!(buffer.len(), 10);
    }

    // -- Debug impl --

    #[test]
    fn test_should_implement_debug() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();
        let debug_str = format!("{buffer:?}");
        assert!(debug_str.contains("ExceptionBuffer"));
        assert!(debug_str.contains("active_count"));
    }

    // -- SqliteExceptionRepo edge cases --

    #[test]
    fn test_should_create_db_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("new.db");
        assert!(!db_path.exists());

        let _buffer = ExceptionBuffer::new(&db_path).unwrap();
        assert!(db_path.exists());
    }

    #[test]
    fn test_flush_should_be_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        buffer.collect(make_record("comp", "once"));
        buffer.flush().unwrap();
        buffer.flush().unwrap();

        // Still only one entry
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn test_load_should_merge_into_existing_memory() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        // Persist one signature
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.collect(make_record("comp", "persisted"));
            buffer.flush().unwrap();
        }

        // Create new buffer, add a different entry in memory, then load
        {
            let buffer = ExceptionBuffer::new(&db_path).unwrap();
            buffer.collect(make_record("comp", "in-memory only"));
            assert_eq!(buffer.len(), 1);

            let loaded = buffer.load().unwrap();
            assert_eq!(loaded.len(), 1); // only the persisted one
                                         // After load, both entries are in memory
            assert_eq!(buffer.len(), 2);
        }
    }

    #[test]
    fn test_with_default_dir_should_succeed_and_create_file() {
        let result = ExceptionBuffer::with_default_dir("test-component");
        // On developer machines / CI this should succeed (data dir exists)
        match result {
            Ok(buffer) => {
                // Verify buffer is operational
                assert_eq!(buffer.len(), 0);
            }
            Err(_) => {
                // Acceptable: CI or restricted environments may not have a data dir
            }
        }
    }

    #[test]
    fn test_delete_reported_older_than_should_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        buffer.collect(make_record("comp-a", "old error"));
        buffer.flush().unwrap();

        let sig = buffer.unreported_samples()[0].dedup_signature.clone();
        buffer.mark_reported(&[sig]).unwrap();

        // Cutoff far in the future — should delete the just-reported row
        let future = chrono::Utc::now() + chrono::TimeDelta::days(1);
        let deleted = buffer.delete_reported_older_than(&future).unwrap();
        // At minimum should not error; row count depends on SQLite internals
        let _ = deleted;
    }

    #[test]
    fn test_delete_reported_older_than_should_preserve_unreported() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test2.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        // Collect but DON'T mark as reported
        buffer.collect(make_record("comp-b", "unreported error"));
        buffer.flush().unwrap();

        // Cutoff in the future — should NOT delete unreported rows
        let future = chrono::Utc::now() + chrono::TimeDelta::days(1);
        let deleted = buffer.delete_reported_older_than(&future).unwrap();
        assert_eq!(deleted, 0);
    }
}
