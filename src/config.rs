//! 配置加载与 repo-map 管理。
//!
//! 从 `repo-map.toml` 加载组件到 GitHub 仓库的映射，
//! 未显式配置的字段使用 `Default` 值。

use std::{collections::HashMap, path::Path, time::Duration};

use serde::Deserialize;

use crate::CollectorError;

// ── RepoMapConfig ─────────────────────────────────────────────────────────

/// `repo-map.toml` 文件解析结构。
#[derive(Debug, Deserialize)]
struct RepoMapFile {
    /// 组件名 → GitHub 仓库映射。
    components: HashMap<String, String>,
}

// ── CollectorConfig ───────────────────────────────────────────────────────

/// 收集器配置。
///
/// 控制扫描周期、触发条件、重试策略和组件仓库映射。
/// 未显式配置的字段使用默认值。
#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// 日志扫描目录列表，支持 glob pattern，默认空（需手动配置）。
    pub log_dirs: Vec<String>,
    /// 日志扫描间隔（秒），默认 300（5 分钟）。
    pub scan_interval_secs: u64,
    /// 每日触发小时（UTC），默认 2（即 UTC 02:00）。
    pub daily_trigger_hour: u8,
    /// 不同签名数量阈值，达到后触发上报，默认 20。
    pub distinct_count_threshold: u32,
    /// 上报成功后的冷却时间，默认 1 小时。
    pub cooldown_duration: Duration,
    /// 最大重试次数，默认 3。
    pub max_retries: u32,
    /// 重试基础延迟（指数退避），默认 5 分钟。
    pub retry_base_delay: Duration,
    /// 组件名 → GitHub 仓库（`owner/repo`）映射。
    pub repo_map: HashMap<String, String>,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            log_dirs: Vec::new(),
            scan_interval_secs: 300,
            daily_trigger_hour: 2,
            distinct_count_threshold: 20,
            cooldown_duration: Duration::from_secs(3600),
            max_retries: 3,
            retry_base_delay: Duration::from_secs(300),
            repo_map: HashMap::new(),
        }
    }
}

impl CollectorConfig {
    /// 创建使用默认值的配置。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 `repo-map.toml` 文件加载配置。
    ///
    /// 文件中只包含 `[components]` 仓库映射表，其余字段使用默认值。
    ///
    /// # Errors
    ///
    /// 返回 `CollectorError::Io` 当文件不存在或读取失败。
    /// 返回 `CollectorError::TomlParse` 当 TOML 格式不合法。
    #[allow(clippy::disallowed_methods, reason = "同步初始化函数，非 async 上下文")]
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, CollectorError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|source| CollectorError::Io {
            source,
            path: path.to_path_buf(),
        })?;
        let file: RepoMapFile =
            toml::from_str(&content).map_err(|source| CollectorError::TomlParse {
                source,
                path: path.to_path_buf(),
            })?;
        Ok(Self {
            repo_map: file.components,
            ..Default::default()
        })
    }

    /// 根据组件名查找对应的 GitHub 仓库（`owner/repo` 格式）。
    ///
    /// 如果组件名不在 `repo_map` 中，返回 `None`。
    #[must_use]
    pub fn repo_for(&self, component: &str) -> Option<&str> {
        self.repo_map.get(component).map(String::as_str)
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
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_collector_config_should_have_sane_defaults() {
        let config = CollectorConfig::default();

        assert!(config.log_dirs.is_empty());
        assert_eq!(config.scan_interval_secs, 300);
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

    // -- repo_for --

    #[test]
    fn test_repo_for_should_return_repo_for_known_component() {
        let mut config = CollectorConfig::default();
        config.repo_map.insert(
            "token-fleet-switch".to_string(),
            "TokenFleet-AI/token-fleet-switch".to_string(),
        );

        assert_eq!(
            config.repo_for("token-fleet-switch"),
            Some("TokenFleet-AI/token-fleet-switch")
        );
    }

    #[test]
    fn test_repo_for_should_return_none_for_unknown_component() {
        let config = CollectorConfig::default();

        assert_eq!(config.repo_for("unknown-component"), None);
    }

    #[test]
    fn test_repo_for_should_return_none_for_empty_map() {
        let config = CollectorConfig::new();

        assert_eq!(config.repo_for("anything"), None);
    }
}
