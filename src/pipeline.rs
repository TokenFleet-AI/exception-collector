//! Exception reporting pipeline orchestration.
//!
//! The [`PipelineRunner`] coordinates the full reporting flow:
//! buffer → dedup → classify → report → `mark_reported`.

use std::sync::Arc;

use crate::{
    CollectorResult, ExceptionBatch, ExceptionBuffer, ExceptionReporter, ReportResult, ReportTarget,
};

/// Orchestrates the exception reporting pipeline.
#[allow(dead_code, reason = "fields consumed by later integration")]
pub struct PipelineRunner {
    /// Exception buffer for aggregating and persisting exceptions.
    buffer: Arc<ExceptionBuffer>,
    /// Reporter implementation (GitHub or custom platform).
    reporter: Box<dyn ExceptionReporter>,
}

impl std::fmt::Debug for PipelineRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineRunner").finish_non_exhaustive()
    }
}

impl PipelineRunner {
    /// Create a new pipeline runner.
    #[must_use]
    pub fn new(buffer: Arc<ExceptionBuffer>, reporter: Box<dyn ExceptionReporter>) -> Self {
        Self { buffer, reporter }
    }

    /// Trigger a manual report cycle.
    ///
    /// Drains pending items from the buffer and sends them to the reporter.
    ///
    /// # Errors
    ///
    /// Returns a [`CollectorError`] if the reporter fails.
    pub async fn trigger_now(&self) -> CollectorResult<ReportResult> {
        let pending = self.buffer.unreported_samples();
        if pending.is_empty() {
            return Ok(ReportResult::Success);
        }

        let component = pending
            .first()
            .map(|r| r.component.clone())
            .unwrap_or_default();
        let batch = ExceptionBatch::new(
            component,
            pending,
            ReportTarget::GitHub {
                repo: String::new(),
                issue_url: String::new(),
            },
        );

        self.reporter.report(&batch).await
    }
}
