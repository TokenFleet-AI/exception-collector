//! Exception collector — shared `SQLite` buffer mode.
//!
//! Each `TokenFleet` component writes errors into its own `SQLite` database
//! via [`ExceptionBuffer`](crate::ExceptionBuffer). The desktop hub periodically
//! scans all databases under the shared exceptions directory, pulls unreported
//! records, and feeds them into the reporting pipeline.

#![allow(
    clippy::disallowed_methods,
    reason = "fs::read_dir is core to scanning exception db files"
)]

use std::{fs, path::PathBuf};

use crate::{ExceptionBuffer, ExceptionKind, ExceptionRecord};

/// Scan all `*.db` files under the shared exceptions directory and return
/// any unreported exception records across all components.
///
/// Records already marked `reported` are skipped via `WHERE reported=0`.
///
/// # Errors
///
/// Returns [`CollectorError`] if a database cannot be opened.
pub fn collect_unreported(
    exceptions_dir: &str,
) -> Result<Vec<(String, Vec<ExceptionRecord>)>, crate::CollectorError> {
    let dir = PathBuf::from(exceptions_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut all_records: Vec<(String, Vec<ExceptionRecord>)> = Vec::new();

    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "db") {
            let component = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");

            let buffer =
                ExceptionBuffer::new(&path).map_err(|e| crate::CollectorError::Reporter {
                    reason: format!("failed to open {:?}: {e}", path.file_name()),
                })?;
            let _ = buffer.load();
            let unreported = buffer.unreported_samples();
            if !unreported.is_empty() {
                all_records.push((component.to_string(), unreported));
            }
        }
    }

    Ok(all_records)
}

/// Manually capture a `Result::Err` into the buffer.
///
/// Used by any component to record an error directly into its `SQLite` buffer.
pub fn collect_result_err(buffer: &ExceptionBuffer, component: &str, error_message: &str) {
    let mut record = ExceptionRecord::new(
        component,
        ExceptionKind::ResultErr,
        error_message,
        String::new(),
    );
    record.dedup_signature = crate::compute_signature(&record);
    buffer.collect(record);
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::indexing_slicing,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_unreported_should_handle_nonexistent_dir() {
        let result = collect_unreported("/nonexistent/path").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_collect_unreported_should_find_records() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test-component.db");

        let buffer = ExceptionBuffer::new(&db_path).unwrap();
        collect_result_err(&buffer, "test-component", "test error");
        buffer.flush().unwrap();
        drop(buffer);

        let results = collect_unreported(&temp.path().to_string_lossy()).unwrap();
        assert_eq!(results.len(), 1);
        let (component, records) = &results[0];
        assert_eq!(component, "test-component");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "test error");
    }

    #[test]
    fn test_collect_result_err_should_compute_signature() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test.db");
        let buffer = ExceptionBuffer::new(&db_path).unwrap();

        collect_result_err(&buffer, "comp", "something broke");
        let samples = buffer.unreported_samples();
        assert_eq!(samples.len(), 1);
        assert!(!samples[0].dedup_signature.is_empty());
        let sig1 = samples[0].dedup_signature.clone();

        collect_result_err(&buffer, "comp", "something broke");
        let samples2 = buffer.unreported_samples();
        assert_eq!(samples2.len(), 1);
        assert_eq!(samples2[0].dedup_signature, sig1);
    }
}
