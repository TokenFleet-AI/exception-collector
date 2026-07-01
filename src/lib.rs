//! Exception Collector — 全项目组异常收集系统。
//!
//! 自动捕获 panic、error 日志和 `Result::Err`，根据用户是否参与「共建计划」
//! 走两条上报路径：
//!
//! - **共建用户**（已安装 `gh` CLI）：LLM 语义去重 → 自动创建 GitHub Issue
//! - **普通用户**（无 `gh`）：本地签名去重 → 批量上传自研收集平台
//!
//! # 集成方式
//!
//! 每个组件在启动入口调用 `install()` 注册 panic hook 和 tracing layer：
//!
//! ```rust,ignore
//! use exception_collector::{CollectorConfig, install};
//!
//! let config = CollectorConfig::from_file("repo-map.toml")?;
//! install(config, "component-name".into());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations)]

mod buffer;
#[cfg(feature = "http-llm")]
mod channel;
mod collector;
mod config;
mod dedup;
mod llm;
mod normalize;
mod pipeline;
mod reporter;
mod retry;
mod signature;

use std::{fmt, path::PathBuf};

use async_trait::async_trait;
pub use buffer::{exceptions_dir, ExceptionBuffer};
#[cfg(feature = "http-llm")]
pub use channel::HttpLlmChannel;
use chrono::{DateTime, Utc};
pub use collector::{collect_result_err, collect_unreported};
pub use config::CollectorConfig;
pub use dedup::DedupEngine;
#[cfg(feature = "http-llm")]
pub use llm::default_classifier;
pub use llm::{
    ClassificationAction, ClassificationResult, ExistingIssueData, LlmChannel, LlmClassifier,
};
pub use pipeline::PipelineRunner;
pub use reporter::{CustomPlatformReporter, GitHubReporter};
pub use retry::{with_retry, RetryPolicy};
use serde::{Deserialize, Serialize};
pub use signature::compute_signature;
use thiserror::Error;
use uuid::Uuid;

// ── ExceptionKind ─────────────────────────────────────────────────────────

/// 异常来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExceptionKind {
    /// 来自 `std::panic::set_hook` 捕获的 panic。
    Panic,
    /// 来自 tracing `error!` / `warn!` 日志级别。
    ErrorLog,
    /// 来自显式 `Result::Err` 包装。
    ResultErr,
}

impl fmt::Display for ExceptionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panic => f.write_str("panic"),
            Self::ErrorLog => f.write_str("error_log"),
            Self::ResultErr => f.write_str("result_err"),
        }
    }
}

// ── ExceptionRecord ───────────────────────────────────────────────────────

/// 单条异常记录。
///
/// 每次捕获异常时创建一条记录，包含完整的上下文信息用于后续
/// 签名计算、归一化和上报。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionRecord {
    /// 全局唯一标识。
    pub id: Uuid,
    /// 产生异常的组件名，如 `"token-fleet-switch"`。
    pub component: String,
    /// 异常来源类型。
    pub kind: ExceptionKind,
    /// 异常消息，截断至 500 字符。
    pub message: String,
    /// 完整堆栈信息。
    pub stacktrace: String,
    /// tracing span 链（从外到内）。
    pub span_chain: Vec<String>,
    /// 产生异常的 Rust 模块路径。
    pub module_path: Option<String>,
    /// 源码位置，格式为 `file:line`。
    pub location: Option<String>,
    /// 异常发生时间（UTC）。
    pub timestamp: DateTime<Utc>,
    /// SHA256 归一化签名，用于本地去重。
    pub dedup_signature: String,
    /// 是否已上报。
    pub reported: bool,
    /// 关联的 GitHub Issue URL（重复异常时填写）。
    pub duplicate_of: Option<String>,
}

impl ExceptionRecord {
    /// 创建一条新的异常记录。
    ///
    /// `message` 超过 500 字符时自动截断。
    #[must_use]
    pub fn new(
        component: impl Into<String>,
        kind: ExceptionKind,
        message: impl Into<String>,
        stacktrace: impl Into<String>,
    ) -> Self {
        let raw_message = message.into();
        let truncated_message = if raw_message.len() > 500 {
            raw_message[..500].to_string()
        } else {
            raw_message
        };

        Self {
            id: Uuid::new_v4(),
            component: component.into(),
            kind,
            message: truncated_message,
            stacktrace: stacktrace.into(),
            span_chain: Vec::new(),
            module_path: None,
            location: None,
            timestamp: Utc::now(),
            dedup_signature: String::new(),
            reported: false,
            duplicate_of: None,
        }
    }
}

// ── AggregatedException ───────────────────────────────────────────────────

/// 缓冲区内按签名聚合的异常。
///
/// 相同 `dedup_signature` 的异常记录被聚合为一条，
/// 只保留首次出现的样本记录，后续仅累加计数和更新时间。
#[derive(Debug, Clone)]
pub struct AggregatedException {
    /// 归一化签名。
    pub signature: String,
    /// 首次出现时间。
    pub first_seen: DateTime<Utc>,
    /// 最近出现时间。
    pub last_seen: DateTime<Utc>,
    /// 累计出现次数。
    pub count: u32,
    /// 样本异常记录（首次出现的记录）。
    pub sample: ExceptionRecord,
}

// ── ExceptionBatch ────────────────────────────────────────────────────────

/// 上报批次。
///
/// 触发条件满足时，从缓冲区取出待上报的聚合异常，
/// 组装成一个批次进行上报。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionBatch {
    /// 批次唯一标识。
    pub id: Uuid,
    /// 组件名。
    pub component: String,
    /// 本批次包含的异常记录。
    pub records: Vec<ExceptionRecord>,
    /// 上报目标。
    pub target: ReportTarget,
    /// 批次创建时间（UTC）。
    pub created_at: DateTime<Utc>,
    /// 批次状态。
    pub status: BatchStatus,
}

impl ExceptionBatch {
    /// 创建一个新的待上报批次。
    #[must_use]
    pub fn new(
        component: impl Into<String>,
        records: Vec<ExceptionRecord>,
        target: ReportTarget,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            component: component.into(),
            records,
            target,
            created_at: Utc::now(),
            status: BatchStatus::Pending,
        }
    }
}

// ── ReportTarget ──────────────────────────────────────────────────────────

/// 上报目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportTarget {
    /// GitHub Issue（共建用户路径）。
    GitHub {
        /// 目标仓库，格式 `owner/repo`。
        repo: String,
        /// 关联的 Issue URL（评论已有 Issue 时填写）。
        issue_url: String,
    },
    /// 自研收集平台（普通用户路径）。
    CustomPlatform {
        /// API endpoint URL。
        endpoint: String,
    },
}

// ── BatchStatus ───────────────────────────────────────────────────────────

/// 批次上报状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchStatus {
    /// 等待上报。
    Pending,
    /// 已成功上报。
    Reported,
    /// 上报失败，包含失败原因和下次重试时间。
    Failed {
        /// 失败原因。
        reason: String,
        /// 下次重试时间（UTC）。
        retry_at: DateTime<Utc>,
    },
}

// ── Error Types ───────────────────────────────────────────────────────────

/// 收集器错误类型。
#[derive(Debug, Error)]
pub enum CollectorError {
    /// I/O 错误（文件读写失败）。
    #[error("failed to read file: {}", .path.display())]
    Io {
        /// 底层 I/O 错误。
        #[source]
        source: std::io::Error,
        /// 失败的文件路径。
        path: PathBuf,
    },

    /// TOML 解析错误。
    #[error("failed to parse TOML: {}", .path.display())]
    TomlParse {
        /// 底层解析错误。
        #[source]
        source: toml::de::Error,
        /// 失败的文件路径。
        path: PathBuf,
    },

    /// `SQLite` 持久化错误。
    #[error("sqlite error: {0}")]
    Sqlite(
        /// 底层 `SQLite` 错误。
        #[source]
        rusqlite::Error,
    ),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    SerdeJson(
        /// 底层 JSON 错误。
        #[source]
        serde_json::Error,
    ),

    /// 外部命令执行错误（如 `gh` CLI）。
    #[error("command error: {message}")]
    Command {
        /// 人类可读的错误描述。
        message: String,
        /// 底层 I/O 错误。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// HTTP 请求错误。
    #[error("http error: {message}")]
    Http {
        /// 人类可读的错误描述。
        message: String,
        /// 底层传输错误。
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// 上报器错误（通用）。
    #[error("reporter error: {reason}")]
    Reporter {
        /// 人类可读的错误原因。
        reason: String,
    },
}

/// 收集器结果类型别名。
pub type CollectorResult<T> = std::result::Result<T, CollectorError>;

// ── Reporter Trait ────────────────────────────────────────────────────────

/// 上报结果。
#[derive(Debug, Clone)]
pub enum ReportResult {
    /// 全部成功。
    Success,
    /// 部分成功，包含成功上报的记录数。
    Partial {
        /// 成功上报的记录数量。
        reported_count: usize,
    },
    /// 全部失败，包含失败原因和下次重试时间。
    Failed {
        /// 失败原因。
        reason: String,
        /// 下次重试时间（UTC）。
        retry_at: DateTime<Utc>,
    },
}

/// 异常上报器 trait。
///
/// 定义异常批次上报的统一接口。不同的上报路径
/// （GitHub Issue、自研平台）实现此 trait。
#[async_trait]
pub trait ExceptionReporter: Send + Sync {
    /// 上报一个异常批次。
    ///
    /// # Errors
    ///
    /// 当上报过程发生不可恢复的错误时返回 `CollectorError`。
    async fn report(&self, batch: &ExceptionBatch) -> CollectorResult<ReportResult>;

    /// 检查此 reporter 是否可用。
    ///
    /// 例如 `GitHubReporter` 检查 `gh` CLI 是否在 PATH 中，
    /// `CustomPlatformReporter` 检查 endpoint 是否已配置。
    ///
    /// 默认返回 `true`。
    async fn check_available(&self) -> bool {
        true
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::indexing_slicing,
    clippy::wildcard_enum_match_arm,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use std::time::Duration;

    use super::*;

    // -- ExceptionKind --

    #[test]
    fn test_exception_kind_should_have_three_variants() {
        let panic = ExceptionKind::Panic;
        let error_log = ExceptionKind::ErrorLog;
        let result_err = ExceptionKind::ResultErr;

        assert_ne!(panic, error_log);
        assert_ne!(panic, result_err);
        assert_ne!(error_log, result_err);
    }

    #[test]
    fn test_exception_kind_should_be_copy() {
        let kind = ExceptionKind::Panic;
        let copied = kind;
        assert_eq!(kind, copied);
    }

    #[test]
    fn test_exception_kind_should_display_correctly() {
        assert_eq!(ExceptionKind::Panic.to_string(), "panic");
        assert_eq!(ExceptionKind::ErrorLog.to_string(), "error_log");
        assert_eq!(ExceptionKind::ResultErr.to_string(), "result_err");
    }

    #[test]
    fn test_exception_kind_should_serialize_deserialize() {
        let kind = ExceptionKind::Panic;
        let json = serde_json::to_string(&kind).expect("serialize ExceptionKind");
        let deserialized: ExceptionKind =
            serde_json::from_str(&json).expect("deserialize ExceptionKind");
        assert_eq!(kind, deserialized);
    }

    // -- ExceptionRecord --

    #[test]
    fn test_exception_record_should_create_with_defaults() {
        let record =
            ExceptionRecord::new("test-component", ExceptionKind::Panic, "test message", "");

        assert_eq!(record.component, "test-component");
        assert_eq!(record.kind, ExceptionKind::Panic);
        assert_eq!(record.message, "test message");
        assert!(record.stacktrace.is_empty());
        assert!(record.span_chain.is_empty());
        assert!(record.module_path.is_none());
        assert!(record.location.is_none());
        assert!(!record.reported);
        assert!(record.duplicate_of.is_none());
        assert!(record.dedup_signature.is_empty());
    }

    #[test]
    fn test_exception_record_should_truncate_long_message() {
        let long_message = "x".repeat(600);
        let record =
            ExceptionRecord::new("comp", ExceptionKind::ErrorLog, long_message.as_str(), "");

        assert_eq!(record.message.len(), 500);
    }

    #[test]
    fn test_exception_record_should_not_truncate_short_message() {
        let record = ExceptionRecord::new("comp", ExceptionKind::ErrorLog, "short", "");

        assert_eq!(record.message, "short");
    }

    #[test]
    fn test_exception_record_should_not_truncate_exact_500_message() {
        let exact_message = "a".repeat(500);
        let record =
            ExceptionRecord::new("comp", ExceptionKind::ErrorLog, exact_message.as_str(), "");

        assert_eq!(record.message.len(), 500);
    }

    #[test]
    fn test_exception_record_should_have_unique_ids() {
        let r1 = ExceptionRecord::new("comp", ExceptionKind::Panic, "msg1", "");
        let r2 = ExceptionRecord::new("comp", ExceptionKind::Panic, "msg2", "");

        assert_ne!(r1.id, r2.id);
    }

    #[test]
    fn test_exception_record_should_serialize_deserialize() {
        let mut record =
            ExceptionRecord::new("comp", ExceptionKind::ResultErr, "err msg", "stack trace");
        record.dedup_signature = "abc123".to_string();
        record.module_path = Some("my::module".to_string());
        record.location = Some("src/main.rs:42".to_string());
        record.span_chain = vec!["span1".to_string(), "span2".to_string()];

        let json = serde_json::to_string(&record).expect("serialize ExceptionRecord");
        let deserialized: ExceptionRecord =
            serde_json::from_str(&json).expect("deserialize ExceptionRecord");

        assert_eq!(deserialized.id, record.id);
        assert_eq!(deserialized.component, "comp");
        assert_eq!(deserialized.kind, ExceptionKind::ResultErr);
        assert_eq!(deserialized.message, "err msg");
        assert_eq!(deserialized.stacktrace, "stack trace");
        assert_eq!(deserialized.dedup_signature, "abc123");
        assert_eq!(deserialized.module_path.as_deref(), Some("my::module"));
        assert_eq!(deserialized.location.as_deref(), Some("src/main.rs:42"));
        assert_eq!(deserialized.span_chain, vec!["span1", "span2"]);
        assert!(!deserialized.reported);
        assert!(deserialized.duplicate_of.is_none());
    }

    // -- AggregatedException --

    #[test]
    fn test_aggregated_exception_should_track_count() {
        let sample = ExceptionRecord::new("comp", ExceptionKind::Panic, "msg", "");
        let now = Utc::now();
        let agg = AggregatedException {
            signature: "sig_abc".to_string(),
            first_seen: now,
            last_seen: now,
            count: 5,
            sample,
        };

        assert_eq!(agg.count, 5);
        assert_eq!(agg.signature, "sig_abc");
        assert_eq!(agg.first_seen, agg.last_seen);
    }

    #[test]
    fn test_aggregated_exception_should_be_cloneable() {
        let sample = ExceptionRecord::new("comp", ExceptionKind::Panic, "msg", "");
        let now = Utc::now();
        let agg = AggregatedException {
            signature: "sig".to_string(),
            first_seen: now,
            last_seen: now,
            count: 1,
            sample,
        };
        let cloned = agg.clone();

        assert_eq!(cloned.signature, agg.signature);
        assert_eq!(cloned.count, agg.count);
        assert_eq!(cloned.first_seen, agg.first_seen);
    }

    // -- ExceptionBatch --

    #[test]
    fn test_exception_batch_should_create_with_pending_status() {
        let records = vec![ExceptionRecord::new(
            "comp",
            ExceptionKind::Panic,
            "msg",
            "",
        )];
        let target = ReportTarget::GitHub {
            repo: "owner/repo".to_string(),
            issue_url: String::new(),
        };
        let batch = ExceptionBatch::new("comp", records, target);

        assert_eq!(batch.component, "comp");
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.status, BatchStatus::Pending);
    }

    #[test]
    fn test_exception_batch_should_have_unique_ids() {
        let target = ReportTarget::GitHub {
            repo: "owner/repo".to_string(),
            issue_url: String::new(),
        };
        let b1 = ExceptionBatch::new("comp", vec![], target.clone());
        let b2 = ExceptionBatch::new("comp", vec![], target);

        assert_ne!(b1.id, b2.id);
    }

    #[test]
    fn test_exception_batch_should_serialize_deserialize() {
        let record = ExceptionRecord::new("comp", ExceptionKind::ErrorLog, "msg", "");
        let target = ReportTarget::CustomPlatform {
            endpoint: "https://example.com/api".to_string(),
        };
        let batch = ExceptionBatch::new("comp", vec![record], target);

        let json = serde_json::to_string(&batch).expect("serialize ExceptionBatch");
        let deserialized: ExceptionBatch =
            serde_json::from_str(&json).expect("deserialize ExceptionBatch");

        assert_eq!(deserialized.id, batch.id);
        assert_eq!(deserialized.component, "comp");
        assert_eq!(deserialized.records.len(), 1);
        assert_eq!(deserialized.status, BatchStatus::Pending);
    }

    // -- ReportTarget --

    #[test]
    fn test_report_target_should_create_github_variant() {
        let target = ReportTarget::GitHub {
            repo: "TokenFleet-AI/token-fleet-switch".to_string(),
            issue_url: "https://github.com/TokenFleet-AI/token-fleet-switch/issues/1".to_string(),
        };
        match &target {
            ReportTarget::GitHub { repo, issue_url } => {
                assert_eq!(repo, "TokenFleet-AI/token-fleet-switch");
                assert!(!issue_url.is_empty());
            }
            ReportTarget::CustomPlatform { .. } => panic!("expected GitHub variant"),
        }
    }

    #[test]
    fn test_report_target_should_create_custom_platform_variant() {
        let target = ReportTarget::CustomPlatform {
            endpoint: "https://exceptions.example.com".to_string(),
        };
        match &target {
            ReportTarget::CustomPlatform { endpoint } => {
                assert_eq!(endpoint, "https://exceptions.example.com");
            }
            ReportTarget::GitHub { .. } => panic!("expected CustomPlatform variant"),
        }
    }

    #[test]
    fn test_report_target_should_support_equality() {
        let t1 = ReportTarget::GitHub {
            repo: "owner/repo".to_string(),
            issue_url: String::new(),
        };
        let t2 = ReportTarget::GitHub {
            repo: "owner/repo".to_string(),
            issue_url: String::new(),
        };
        let t3 = ReportTarget::CustomPlatform {
            endpoint: "https://example.com".to_string(),
        };

        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
    }

    #[test]
    fn test_report_target_should_serialize_deserialize() {
        let target = ReportTarget::GitHub {
            repo: "owner/repo".to_string(),
            issue_url: "https://github.com/owner/repo/issues/1".to_string(),
        };
        let json = serde_json::to_string(&target).expect("serialize ReportTarget");
        let deserialized: ReportTarget =
            serde_json::from_str(&json).expect("deserialize ReportTarget");
        assert_eq!(target, deserialized);
    }

    // -- BatchStatus --

    #[test]
    fn test_batch_status_should_have_three_variants() {
        let pending = BatchStatus::Pending;
        let reported = BatchStatus::Reported;
        let failed = BatchStatus::Failed {
            reason: "network error".to_string(),
            retry_at: Utc::now(),
        };

        assert_eq!(pending, BatchStatus::Pending);
        assert_eq!(reported, BatchStatus::Reported);
        assert_ne!(pending, reported);
        match &failed {
            BatchStatus::Failed { reason, .. } => {
                assert_eq!(reason, "network error");
            }
            _ => panic!("expected Failed variant"),
        }
    }

    #[test]
    fn test_batch_status_should_serialize_deserialize() {
        let status = BatchStatus::Failed {
            reason: "timeout".to_string(),
            retry_at: Utc::now(),
        };
        let json = serde_json::to_string(&status).expect("serialize BatchStatus");
        let deserialized: BatchStatus =
            serde_json::from_str(&json).expect("deserialize BatchStatus");
        assert_eq!(status, deserialized);
    }

    // -- CollectorConfig --

    #[test]
    fn test_collector_config_should_have_sane_defaults() {
        let config = CollectorConfig::default();

        assert_eq!(config.daily_trigger_hour, 2);
        assert_eq!(config.distinct_count_threshold, 20);
        assert_eq!(config.cooldown_duration, Duration::from_secs(3600));
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_base_delay, Duration::from_secs(300));
        assert!(config.repo_map.is_empty());
    }

    #[test]
    fn test_collector_config_new_should_equal_default() {
        let config = CollectorConfig::new();
        let default = CollectorConfig::default();

        assert_eq!(config.daily_trigger_hour, default.daily_trigger_hour);
        assert_eq!(
            config.distinct_count_threshold,
            default.distinct_count_threshold
        );
        assert_eq!(config.max_retries, default.max_retries);
    }

    #[test]
    fn test_collector_config_should_be_cloneable() {
        let mut config = CollectorConfig::default();
        config
            .repo_map
            .insert("comp".to_string(), "owner/repo".to_string());
        let cloned = config.clone();

        assert_eq!(cloned.daily_trigger_hour, config.daily_trigger_hour);
        assert_eq!(cloned.repo_map, config.repo_map);
    }

    #[test]
    fn test_collector_config_from_file_should_parse_repo_map() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("repo-map.toml");
        std::fs::write(
            &path,
            r#"
[components]
"token-fleet-switch" = "TokenFleet-AI/token-fleet-switch"
"agent-proxy-rust" = "TokenFleet-AI/agent-proxy-rust"
"#,
        )
        .expect("write test file");

        let config = CollectorConfig::from_file(&path).expect("parse config");

        assert_eq!(config.repo_map.len(), 2);
        assert_eq!(
            config
                .repo_map
                .get("token-fleet-switch")
                .map(String::as_str),
            Some("TokenFleet-AI/token-fleet-switch")
        );
        assert_eq!(
            config.repo_map.get("agent-proxy-rust").map(String::as_str),
            Some("TokenFleet-AI/agent-proxy-rust")
        );
        // defaults preserved
        assert_eq!(config.daily_trigger_hour, 2);
        assert_eq!(config.distinct_count_threshold, 20);
    }

    #[test]
    fn test_collector_config_from_file_should_error_on_missing_file() {
        let result = CollectorConfig::from_file("/nonexistent/path/repo-map.toml");

        assert!(result.is_err());
        let err = result.expect_err("expected error for missing file");
        match err {
            CollectorError::Io { path, .. } => {
                assert_eq!(path, PathBuf::from("/nonexistent/path/repo-map.toml"));
            }
            _ => panic!("expected Io error, got: {err:?}"),
        }
    }

    #[test]
    fn test_collector_config_from_file_should_error_on_invalid_toml() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{").expect("write bad file");

        let result = CollectorConfig::from_file(&path);

        assert!(result.is_err());
        let err = result.expect_err("expected error for invalid TOML");
        match err {
            CollectorError::TomlParse { path: p, .. } => {
                assert_eq!(p, path);
            }
            _ => panic!("expected TomlParse error, got: {err:?}"),
        }
    }

    #[test]
    fn test_collector_config_from_file_should_error_when_components_key_missing() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("no-components.toml");
        std::fs::write(&path, "[other]\nkey = \"value\"\n").expect("write file");

        let result = CollectorConfig::from_file(&path);

        assert!(result.is_err());
    }

    #[test]
    fn test_collector_config_from_file_should_handle_empty_components() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("empty.toml");
        std::fs::write(&path, "[components]\n").expect("write file");

        let config = CollectorConfig::from_file(&path).expect("parse empty components");

        assert!(config.repo_map.is_empty());
        assert_eq!(config.daily_trigger_hour, 2);
    }

    // -- CollectorError --

    #[test]
    fn test_collector_error_should_display_io_error() {
        let err = CollectorError::Io {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
            path: PathBuf::from("/foo/bar.toml"),
        };
        let display = err.to_string();

        assert!(display.contains("/foo/bar.toml"), "got: {display}");
    }

    #[test]
    fn test_collector_error_should_display_toml_parse_error() {
        let toml_err = toml::from_str::<toml::Table>("bad").expect_err("should fail to parse");
        let err = CollectorError::TomlParse {
            source: toml_err,
            path: PathBuf::from("/foo/bar.toml"),
        };
        let display = err.to_string();

        assert!(display.contains("/foo/bar.toml"), "got: {display}");
    }

    // -- ReportResult --

    #[test]
    fn test_report_result_should_have_three_variants() {
        let success = ReportResult::Success;
        let partial = ReportResult::Partial { reported_count: 3 };
        let failed = ReportResult::Failed {
            reason: "timeout".to_string(),
            retry_at: Utc::now(),
        };

        match success {
            ReportResult::Success => {}
            _ => panic!("expected Success"),
        }
        match partial {
            ReportResult::Partial { reported_count } => assert_eq!(reported_count, 3),
            _ => panic!("expected Partial"),
        }
        match failed {
            ReportResult::Failed { reason, .. } => assert_eq!(reason, "timeout"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn test_report_result_should_be_cloneable() {
        let result = ReportResult::Partial { reported_count: 5 };
        let cloned = result.clone();

        match cloned {
            ReportResult::Partial { reported_count } => assert_eq!(reported_count, 5),
            _ => panic!("expected Partial after clone"),
        }
    }

    // -- ExceptionReporter default check_available --

    /// Minimal stub reporter used to test the trait's default method.
    struct StubReporter;

    #[async_trait]
    impl ExceptionReporter for StubReporter {
        async fn report(&self, _batch: &ExceptionBatch) -> CollectorResult<ReportResult> {
            Ok(ReportResult::Success)
        }
    }

    #[tokio::test]
    async fn test_check_available_should_default_to_true() {
        let reporter = StubReporter;
        assert!(reporter.check_available().await);
    }
}
