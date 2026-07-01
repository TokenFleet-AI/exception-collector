//! Local signature deduplication engine.
//!
//! [`DedupEngine`] wraps an [`Arc<ExceptionBuffer>`](crate::ExceptionBuffer)
//! and provides fast lookups for duplicate detection, distinct signature
//! counting, and pending batch extraction.

use std::sync::Arc;

use crate::{ExceptionBuffer, ExceptionRecord};

/// Local signature deduplication engine.
///
/// Wraps an [`ExceptionBuffer`] to provide:
/// - Duplicate detection by signature
/// - Distinct signature counting
/// - Pending (unreported) batch extraction
#[derive(Debug)]
pub struct DedupEngine {
    buffer: Arc<ExceptionBuffer>,
}

impl DedupEngine {
    /// Create a new dedup engine wrapping the given buffer.
    #[must_use]
    pub fn new(buffer: Arc<ExceptionBuffer>) -> Self {
        Self { buffer }
    }

    /// Check whether the given signature already exists in the buffer.
    #[must_use]
    pub fn is_duplicate(&self, signature: &str) -> bool {
        self.buffer.contains_signature(signature)
    }

    /// Return the number of distinct signatures in the buffer.
    #[must_use]
    pub fn distinct_count(&self) -> usize {
        self.buffer.len()
    }

    /// Return all unreported sample records from the buffer.
    #[must_use]
    pub fn pending_batch(&self) -> Vec<ExceptionRecord> {
        self.buffer.unreported_samples()
    }
}

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
    use super::*;
    use crate::{ExceptionKind, ExceptionRecord};

    struct TestCtx {
        buffer: Arc<ExceptionBuffer>,
        #[allow(dead_code, reason = "field keeps TempDir alive for test scope")]
        _dir: tempfile::TempDir,
    }

    fn make_ctx() -> TestCtx {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        TestCtx {
            buffer: Arc::new(ExceptionBuffer::new(&db_path).unwrap()),
            _dir: dir,
        }
    }

    fn make_record(component: &str, msg: &str) -> ExceptionRecord {
        ExceptionRecord::new(component, ExceptionKind::Panic, msg, "frame1\nframe2")
    }

    #[test]
    fn test_should_detect_duplicate_by_signature() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer.clone());

        let record = make_record("comp-a", "same error");
        ctx.buffer.collect(record);

        let sig = crate::compute_signature(&make_record("comp-a", "same error"));
        assert!(engine.is_duplicate(&sig));
    }

    #[test]
    fn test_should_not_detect_nonexistent_signature() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer);

        assert!(!engine.is_duplicate("nonexistent_signature_here"));
    }

    #[test]
    fn test_should_count_distinct_signatures() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer.clone());

        ctx.buffer.collect(make_record("comp-a", "error one"));
        ctx.buffer.collect(make_record("comp-b", "error two"));
        ctx.buffer.collect(make_record("comp-a", "error one")); // duplicate

        assert_eq!(engine.distinct_count(), 2);
    }

    #[test]
    fn test_should_count_zero_when_empty() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer);

        assert_eq!(engine.distinct_count(), 0);
    }

    #[test]
    fn test_should_return_correct_pending_batch() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer.clone());

        ctx.buffer.collect(make_record("comp-a", "error one"));
        ctx.buffer.collect(make_record("comp-b", "error two"));

        let pending = engine.pending_batch();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn test_should_not_include_reported_in_pending_batch() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer.clone());

        let r1 = make_record("comp-a", "error one");
        let r2 = make_record("comp-b", "error two");
        ctx.buffer.collect(r1.clone());
        ctx.buffer.collect(r2.clone());
        ctx.buffer.flush().unwrap();

        let sig1 = crate::compute_signature(&r1);
        ctx.buffer
            .mark_reported(std::slice::from_ref(&sig1))
            .unwrap();

        let pending = engine.pending_batch();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].component, "comp-b");
    }

    #[test]
    fn test_should_return_empty_pending_when_all_reported() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer.clone());

        let r = make_record("comp", "reported");
        ctx.buffer.collect(r.clone());
        ctx.buffer.flush().unwrap();

        let sig = crate::compute_signature(&r);
        ctx.buffer
            .mark_reported(std::slice::from_ref(&sig))
            .unwrap();

        let pending = engine.pending_batch();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_should_return_empty_pending_when_buffer_empty() {
        let ctx = make_ctx();
        let engine = DedupEngine::new(ctx.buffer);

        let pending = engine.pending_batch();
        assert!(pending.is_empty());
    }
}
