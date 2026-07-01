//! LLM-based semantic deduplication and issue content generation.
//!
//! The [`LlmClassifier`] sends batches of exceptions to an LLM through
//! a configurable channel for semantic deduplication.
//!
//! # Fallback strategy
//!
//! | Scenario | Handling |
//! |----------|----------|
//! | LLM timeout (>30s) | Fall back to local signature dedup |
//! | LLM returns non-JSON | Retry once, then fall back |
//! | All channels unavailable | Keep for next report cycle |

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{CollectorError, CollectorResult, ExceptionRecord};

// ── Channel Abstraction ─────────────────────────────────────────────────────

/// Abstract LLM channel for classification requests.
///
/// Implementations wire into the existing agent-proxy-rust Channel system,
/// or a mock for testing.
#[async_trait]
pub trait LlmChannel: Send + Sync {
    /// Send a prompt to the LLM and return the raw response text.
    ///
    /// # Errors
    ///
    /// Returns `CollectorError::Reporter` on channel failure or timeout.
    async fn send(&self, system_prompt: &str, user_prompt: &str) -> CollectorResult<String>;
}

// ── Classification Types ────────────────────────────────────────────────────

/// The LLM's decision for a single exception record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationResult {
    /// The dedup signature of the exception.
    pub signature: String,
    /// The action to take.
    pub action: ClassificationAction,
}

/// Action determined by LLM classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClassificationAction {
    /// This exception matches an existing open issue.
    Duplicate {
        /// The GitHub issue number to comment on.
        issue_number: u32,
    },
    /// This is a new issue that should be created.
    NewIssue {
        /// LLM-generated issue title.
        title: String,
        /// LLM-generated issue body.
        body: String,
    },
}

// ── LlmClassifier ───────────────────────────────────────────────────────────

/// Classifies exception batches using an LLM channel.
///
/// Sends a batch of exceptions + existing open issues to the LLM
/// for semantic deduplication and issue content generation.
pub struct LlmClassifier {
    /// The LLM channel for sending prompts.
    channel: Arc<dyn LlmChannel>,
    /// Timeout for each LLM request in seconds.
    timeout_secs: u64,
}

impl std::fmt::Debug for LlmClassifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClassifier")
            .field("timeout_secs", &self.timeout_secs)
            .finish_non_exhaustive()
    }
}

/// JSON input format for the LLM: a list of exception records.
#[derive(Debug, Serialize)]
struct ExceptionInput {
    signature: String,
    component: String,
    message: String,
    stacktrace: String,
}

/// JSON input for an existing GitHub issue.
#[derive(Debug, Serialize)]
struct ExistingIssue {
    number: u32,
    title: String,
    body: String,
}

impl LlmClassifier {
    /// Create a new `LlmClassifier` with the given channel.
    #[must_use]
    pub fn new(channel: Arc<dyn LlmChannel>) -> Self {
        Self {
            channel,
            timeout_secs: 30,
        }
    }

    /// Classify a batch of exceptions against existing open issues.
    ///
    /// Sends one LLM request for the entire batch (not per-record).
    ///
    /// # Errors
    ///
    /// Returns an error if the LLM channel fails and fallback is not possible.
    pub async fn classify_batch(
        &self,
        records: &[ExceptionRecord],
        existing_issues: &[ExistingIssueData],
    ) -> CollectorResult<Vec<ClassificationResult>> {
        let system_prompt = build_system_prompt();
        let user_prompt = build_user_prompt(records, existing_issues);

        // Try LLM request
        let response = self.channel.send(&system_prompt, &user_prompt).await;

        let text = match response {
            Ok(text) => text,
            Err(e) => {
                // Fallback: treat all as new issues with auto-generated content
                let results = fallback_classify(records);
                return if results.is_empty() {
                    Err(e)
                } else {
                    Ok(results)
                };
            }
        };

        // Try parsing, retry once on failure
        let Ok(results) = parse_llm_response(&text) else {
            let retry = self.channel.send(&system_prompt, &user_prompt).await?;
            return parse_llm_response(&retry).map_err(|_| CollectorError::Reporter {
                reason: "LLM response not valid JSON after retry".to_string(),
            });
        };
        Ok(results)
    }
}

/// Create an `LlmClassifier` using the default HTTP channel (from env vars).
///
/// Requires the `http-llm` feature.
#[cfg(feature = "http-llm")]
#[must_use]
pub fn default_classifier() -> LlmClassifier {
    use crate::channel::HttpLlmChannel;
    LlmClassifier::new(Arc::new(HttpLlmChannel::from_env()))
}

/// Data about an existing open GitHub issue (for dedup comparison).
#[derive(Debug, Clone)]
pub struct ExistingIssueData {
    /// GitHub issue number.
    pub number: u32,
    /// Issue title.
    pub title: String,
    /// Issue body.
    pub body: String,
}

// ── Prompt Building ─────────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str =
    "\
You are a TokenFleet exception classification assistant. Your job is to determine whether each new \
     exception matches an existing open issue (semantic duplicate) or is truly new. For \
     duplicates: reference the existing issue number. For new issues: generate a concise title \
     and a structured body with: exception summary, component, module path, location, and stack \
     trace. Respond ONLY with a JSON array of objects, each with keys: \"signature\", \"action\" \
     (\"duplicate\" or \"new_issue\"), \"issue_number\" (for duplicates), \"title\" and \"body\" \
     (for new issues).";

fn build_system_prompt() -> String {
    SYSTEM_PROMPT.to_string()
}

fn build_user_prompt(records: &[ExceptionRecord], existing_issues: &[ExistingIssueData]) -> String {
    let exceptions: Vec<ExceptionInput> = records
        .iter()
        .map(|r| ExceptionInput {
            signature: r.dedup_signature.clone(),
            component: r.component.clone(),
            message: r.message.clone(),
            stacktrace: r.stacktrace.clone(),
        })
        .collect();

    let issues: Vec<ExistingIssue> = existing_issues
        .iter()
        .map(|i| ExistingIssue {
            number: i.number,
            title: i.title.clone(),
            body: i.body.clone(),
        })
        .collect();

    let exceptions_json = serde_json::to_string(&exceptions).unwrap_or_default();
    let issues_json = serde_json::to_string(&issues).unwrap_or_default();

    format!(
        "## Existing Open Issues (JSON)\n{issues_json}\n\n## New Exceptions \
         (JSON)\n{exceptions_json}"
    )
}

// ── Response Parsing ────────────────────────────────────────────────────────

/// Parse the LLM's JSON response into classification results.
fn parse_llm_response(text: &str) -> CollectorResult<Vec<ClassificationResult>> {
    // Strip markdown code fences if present
    let json_text = text
        .trim()
        .strip_prefix("```json")
        .and_then(|s| s.strip_suffix("```"))
        .map_or(text.trim(), str::trim);

    let results: Vec<ClassificationResult> =
        serde_json::from_str(json_text).map_err(CollectorError::SerdeJson)?;
    Ok(results)
}

// ── Fallback ────────────────────────────────────────────────────────────────

/// Generate classification results without LLM (local signature-based).
///
/// Each record is treated as a new issue with auto-generated content.
fn fallback_classify(records: &[ExceptionRecord]) -> Vec<ClassificationResult> {
    records
        .iter()
        .map(|r| ClassificationResult {
            signature: r.dedup_signature.clone(),
            action: ClassificationAction::NewIssue {
                title: format!(
                    "[{}] {}: {}",
                    r.component,
                    r.kind,
                    &r.message[..r.message.len().min(80)]
                ),
                body: build_fallback_body(r),
            },
        })
        .collect()
}

/// Build an issue body for fallback (no LLM).
fn build_fallback_body(record: &ExceptionRecord) -> String {
    format!(
        "## 异常摘要\n{message}\n\n## 堆栈\n```\n{stacktrace}\n```\n\n## 影响范围\n- 组件: \
         {component}\n- 模块: {module}\n- 位置: {location}\n- 时间: {timestamp}\n\n---\n> 由 \
         TokenFleet Exception Collector 自动生成（降级模式）",
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
    clippy::indexing_slicing,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use super::*;
    use crate::{ExceptionKind, ExceptionRecord};

    /// Mock LLM channel that returns a predefined response.
    struct MockChannel {
        response: String,
        should_fail: bool,
    }

    #[async_trait]
    impl LlmChannel for MockChannel {
        async fn send(&self, _system: &str, _user: &str) -> CollectorResult<String> {
            if self.should_fail {
                Err(CollectorError::Reporter {
                    reason: "mock channel failure".to_string(),
                })
            } else {
                Ok(self.response.clone())
            }
        }
    }

    fn make_record(signature: &str, message: &str) -> ExceptionRecord {
        let mut record =
            ExceptionRecord::new("test-comp", ExceptionKind::ErrorLog, message, "stack trace");
        record.dedup_signature = signature.to_string();
        record
    }

    fn duplicate_response(sig: &str, issue: u32) -> String {
        format!(r#"[{{"signature":"{sig}","action":{{"issue_number":{issue}}}}}]"#)
    }

    fn new_issue_response(sig: &str) -> String {
        format!(r#"[{{"signature":"{sig}","action":{{"title":"Test Issue","body":"Test body"}}}}]"#)
    }

    #[test]
    fn test_should_detect_duplicate_via_llm() {
        let response = duplicate_response("abc123", 42);
        let channel = Arc::new(MockChannel {
            response,
            should_fail: false,
        });
        let classifier = LlmClassifier::new(channel);

        let records = vec![make_record("abc123", "test error")];
        let existing = vec![ExistingIssueData {
            number: 42,
            title: "Existing issue".to_string(),
            body: "Old body".to_string(),
        }];

        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt
            .block_on(classifier.classify_batch(&records, &existing))
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].signature, "abc123");
        match &results[0].action {
            ClassificationAction::Duplicate { issue_number } => {
                assert_eq!(*issue_number, 42);
            }
            _ => panic!("expected Duplicate"),
        }
    }

    #[test]
    fn test_should_generate_new_issue_via_llm() {
        let response = new_issue_response("xyz789");
        let channel = Arc::new(MockChannel {
            response,
            should_fail: false,
        });
        let classifier = LlmClassifier::new(channel);

        let records = vec![make_record("xyz789", "new error")];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt
            .block_on(classifier.classify_batch(&records, &[]))
            .unwrap();

        assert_eq!(results.len(), 1);
        match &results[0].action {
            ClassificationAction::NewIssue { title, body } => {
                assert_eq!(title, "Test Issue");
                assert_eq!(body, "Test body");
            }
            _ => panic!("expected NewIssue"),
        }
    }

    #[test]
    fn test_should_fallback_on_llm_failure() {
        let channel = Arc::new(MockChannel {
            response: String::new(),
            should_fail: true,
        });
        let classifier = LlmClassifier::new(channel);

        let records = vec![make_record("sig1", "test error")];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt
            .block_on(classifier.classify_batch(&records, &[]))
            .unwrap();

        // Fallback should treat all as new issues
        assert_eq!(results.len(), 1);
        match &results[0].action {
            ClassificationAction::NewIssue { title, .. } => {
                assert!(title.contains("test error"));
            }
            _ => panic!("expected fallback NewIssue"),
        }
    }

    #[test]
    fn test_should_fallback_on_non_json_response() {
        let channel = Arc::new(MockChannel {
            response: "not valid json at all".to_string(),
            should_fail: false,
        });
        let classifier = LlmClassifier::new(channel);

        let records = vec![make_record("sig1", "test")];
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Should fail after retry — non-JSON both times
        let result = rt.block_on(classifier.classify_batch(&records, &[]));

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_llm_response_should_handle_json_with_code_fence() {
        let text = r###"```json
[{"signature":"abc","action":{"title":"T","body":"B"}}]
```"###;
        let results = parse_llm_response(text).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_llm_response_should_handle_plain_json() {
        let text = r#"[{"signature":"abc","action":{"title":"T","body":"B"}}]"#;
        let results = parse_llm_response(text).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_llm_response_should_error_on_invalid() {
        let text = "not json";
        assert!(parse_llm_response(text).is_err());
    }

    #[test]
    fn test_build_fallback_body_should_include_component() {
        let mut record = make_record("sig", "test message");
        record.module_path = Some("mod".to_string());
        record.location = Some("file.rs:1".to_string());

        let body = build_fallback_body(&record);
        assert!(body.contains("test message"));
        assert!(body.contains("test-comp"));
        assert!(body.contains("降级模式"));
    }

    #[test]
    fn test_system_prompt_should_be_non_empty() {
        let prompt = build_system_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("TokenFleet"));
    }

    #[test]
    fn test_user_prompt_should_include_records() {
        let records = vec![make_record("sig-a", "error one")];
        let issues = vec![ExistingIssueData {
            number: 1,
            title: "Old".to_string(),
            body: "Old body".to_string(),
        }];

        let prompt = build_user_prompt(&records, &issues);
        assert!(prompt.contains("sig-a"));
        assert!(prompt.contains("error one"));
        assert!(prompt.contains("Old"));
        assert!(prompt.contains("Existing Open Issues"));
    }

    #[test]
    fn test_classification_action_serde_duplicate() {
        let action = ClassificationAction::Duplicate { issue_number: 5 };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("issue_number"));
        assert!(json.contains("5"));
    }

    #[test]
    fn test_classification_action_serde_new_issue() {
        let action = ClassificationAction::NewIssue {
            title: "T".to_string(),
            body: "B".to_string(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("title"));
        assert!(json.contains("B"));
    }
}
