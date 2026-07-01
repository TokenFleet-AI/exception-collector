//! Reporter implementations: GitHub Issue reporting and custom platform reporting.
//!
//! Provides two [`ExceptionReporter`] implementations:
//! - [`GitHubReporter`] — calls `gh` CLI for GitHub Issue creation
//! - [`CustomPlatformReporter`] — POSTs to a configurable endpoint

use async_trait::async_trait;
use serde::Serialize;

use crate::{CollectorError, CollectorResult, ExceptionBatch, ExceptionReporter, ReportResult};

// ── GitHubReporter ──────────────────────────────────────────────────────────

/// Reports exceptions by calling the `gh` CLI.
///
/// Requires `gh` to be installed and authenticated.
/// Each new exception becomes a GitHub Issue; duplicates get a comment.
#[derive(Debug)]
pub struct GitHubReporter;

impl GitHubReporter {
    /// Create a new `GitHubReporter`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Execute `gh issue create` and return the issue URL.
    async fn create_issue(&self, repo: &str, title: &str, body: &str) -> CollectorResult<String> {
        let output = tokio::process::Command::new("gh")
            .args([
                "issue", "create", "--repo", repo, "--title", title, "--body", body,
            ])
            .output()
            .await
            .map_err(|e| CollectorError::Reporter {
                reason: format!("failed to run gh issue create: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CollectorError::Reporter {
                reason: format!("gh issue create failed: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(stdout)
    }

    /// Execute `gh issue comment` on an existing issue.
    async fn comment_on_issue(
        &self,
        repo: &str,
        issue_number: &str,
        body: &str,
    ) -> CollectorResult<()> {
        let issue_ref = format!("{repo}#{issue_number}");
        let output = tokio::process::Command::new("gh")
            .args(["issue", "comment", &issue_ref, "--body", body])
            .output()
            .await
            .map_err(|e| CollectorError::Reporter {
                reason: format!("failed to run gh issue comment: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CollectorError::Reporter {
                reason: format!("gh issue comment failed: {stderr}"),
            });
        }

        Ok(())
    }
}

impl Default for GitHubReporter {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ExceptionReporter for GitHubReporter {
    /// Check if `gh` CLI is installed and available in `PATH`.
    async fn check_available(&self) -> bool {
        which::which("gh").is_ok()
    }

    /// Report an exception batch by creating GitHub Issues.
    ///
    /// For each record in the batch, creates a new issue.
    /// Records with `duplicate_of` set get a comment on the existing issue instead.
    async fn report(&self, batch: &ExceptionBatch) -> CollectorResult<ReportResult> {
        let repo = match &batch.target {
            crate::ReportTarget::GitHub { repo, .. } => repo.clone(),
            crate::ReportTarget::CustomPlatform { .. } => {
                return Err(CollectorError::Reporter {
                    reason: "GitHubReporter requires ReportTarget::GitHub".to_string(),
                });
            }
        };

        let mut reported_count = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for record in &batch.records {
            if let Some(dup_url) = &record.duplicate_of {
                let issue_number = dup_url.rsplit('/').next().unwrap_or("unknown");

                let comment_body = format!(
                    "## 重复出现\n\n此异常仍在发生。\n\n- 组件: {}\n- 最近发生: {}\n",
                    record.component,
                    record.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
                );

                match self
                    .comment_on_issue(&repo, issue_number, &comment_body)
                    .await
                {
                    Ok(()) => reported_count += 1,
                    Err(e) => errors.push(format!("comment failed for {dup_url}: {e}")),
                }
            } else {
                let title = format!(
                    "[{}] {}: {}",
                    record.component,
                    record.kind,
                    truncate_for_title(&record.message, 80)
                );
                let body = build_issue_body(record);

                match self.create_issue(&repo, &title, &body).await {
                    Ok(_url) => reported_count += 1,
                    Err(e) => errors.push(format!("issue create failed: {e}")),
                }
            }
        }

        let total = batch.records.len();
        if reported_count == total {
            Ok(ReportResult::Success)
        } else if reported_count > 0 {
            Ok(ReportResult::Partial { reported_count })
        } else {
            let reason = errors.join("; ");
            let retry_at = chrono::Utc::now() + chrono::TimeDelta::minutes(5);
            Ok(ReportResult::Failed { reason, retry_at })
        }
    }
}

// ── CustomPlatformReporter ─────────────────────────────────────────────────

/// Reports exceptions by sending a `POST` request to a custom platform endpoint.
///
/// This is a placeholder implementation intended for integration
/// with a self-hosted exception collection platform.
#[derive(Debug, Clone)]
pub struct CustomPlatformReporter {
    /// The base URL of the custom platform API.
    endpoint: String,
}

impl CustomPlatformReporter {
    /// Create a new `CustomPlatformReporter` with the given endpoint URL.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

/// Lightweight JSON payload for the custom platform API.
#[derive(Debug, Serialize)]
struct ExceptionPayload<'a> {
    component: &'a str,
    batch_id: &'a str,
    records: &'a [crate::ExceptionRecord],
}

#[async_trait]
impl ExceptionReporter for CustomPlatformReporter {
    /// Check if the endpoint is configured (non-empty).
    async fn check_available(&self) -> bool {
        !self.endpoint.is_empty()
    }

    /// POST the batch to `{endpoint}/api/exceptions`.
    async fn report(&self, batch: &ExceptionBatch) -> CollectorResult<ReportResult> {
        let url = format!("{}/api/exceptions", self.endpoint);
        let payload = ExceptionPayload {
            component: &batch.component,
            batch_id: &batch.id.to_string(),
            records: &batch.records,
        };
        let body = serde_json::to_string(&payload).map_err(CollectorError::SerdeJson)?;

        let result = tokio::task::spawn_blocking(
            move || -> Result<ureq::Response, Box<dyn std::error::Error + Send + Sync>> {
                ureq::post(&url)
                    .set("Content-Type", "application/json")
                    .send_string(&body)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            },
        )
        .await
        .map_err(|e| CollectorError::Reporter {
            reason: format!("spawn_blocking join error: {e}"),
        })?;

        match result {
            Ok(response) if response.status() < 400 => {
                if batch.records.is_empty() {
                    Ok(ReportResult::Success)
                } else {
                    Ok(ReportResult::Partial {
                        reported_count: batch.records.len(),
                    })
                }
            }
            Ok(response) => {
                let status = response.status();
                Ok(ReportResult::Failed {
                    reason: format!("HTTP {status}"),
                    retry_at: chrono::Utc::now() + chrono::TimeDelta::minutes(5),
                })
            }
            Err(e) => Ok(ReportResult::Failed {
                reason: e.to_string(),
                retry_at: chrono::Utc::now() + chrono::TimeDelta::minutes(5),
            }),
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Truncate message for use in an Issue title.
fn truncate_for_title(msg: &str, max_len: usize) -> &str {
    if msg.len() <= max_len {
        msg
    } else {
        &msg[..max_len]
    }
}

/// Build an Issue body from an `ExceptionRecord`.
fn build_issue_body(record: &crate::ExceptionRecord) -> String {
    format!(
        "## 异常摘要\n{message}\n\n## 堆栈\n```\n{stacktrace}\n```\n\n## 影响范围\n- 组件: \
         {component}\n- 模块: {module}\n- 位置: {location}\n- 时间: {timestamp}\n\n---\n> 由 \
         TokenFleet Exception Collector 自动生成",
        message = record.message,
        stacktrace = record.stacktrace,
        component = record.component,
        module = record.module_path.as_deref().unwrap_or("unknown"),
        location = record.location.as_deref().unwrap_or("unknown"),
        timestamp = record.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
    )
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::wildcard_enum_match_arm,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use super::*;
    use crate::{ExceptionKind, ExceptionRecord};

    fn make_record(message: &str) -> ExceptionRecord {
        ExceptionRecord::new(
            "test-comp",
            ExceptionKind::ErrorLog,
            message,
            "stack trace line",
        )
    }

    // -- GitHubReporter --

    #[test]
    fn test_github_reporter_should_create() {
        let reporter = GitHubReporter::new();
        let _ = reporter;
    }

    #[test]
    fn test_github_reporter_should_be_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GitHubReporter>();
    }

    // -- CustomPlatformReporter --

    #[test]
    fn test_custom_platform_reporter_should_create() {
        let reporter = CustomPlatformReporter::new("https://example.com");
        assert!(reporter.endpoint.contains("example.com"));
    }

    #[test]
    fn test_custom_platform_reporter_should_be_cloneable() {
        let reporter = CustomPlatformReporter::new("https://example.com");
        let cloned = reporter.clone();
        assert_eq!(cloned.endpoint, "https://example.com");
    }

    #[tokio::test]
    async fn test_custom_platform_reporter_check_available_should_return_true() {
        let reporter = CustomPlatformReporter::new("https://example.com");
        assert!(reporter.check_available().await);
    }

    #[tokio::test]
    async fn test_custom_platform_reporter_check_available_should_return_false_for_empty() {
        let reporter = CustomPlatformReporter::new("");
        assert!(!reporter.check_available().await);
    }

    // -- Helper functions --

    #[test]
    fn test_truncate_for_title_should_keep_short_messages() {
        assert_eq!(truncate_for_title("short", 80), "short");
    }

    #[test]
    fn test_truncate_for_title_should_truncate_long_messages() {
        let long = "x".repeat(100);
        let result = truncate_for_title(&long, 80);
        assert_eq!(result.len(), 80);
    }

    #[test]
    fn test_build_issue_body_should_include_component_and_message() {
        let mut record = make_record("test error message");
        record.module_path = Some("my::module".to_string());
        record.location = Some("src/main.rs:42".to_string());

        let body = build_issue_body(&record);
        assert!(body.contains("test error message"));
        assert!(body.contains("test-comp"));
        assert!(body.contains("my::module"));
        assert!(body.contains("src/main.rs:42"));
    }
}
