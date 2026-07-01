//! Real HTTP-backed LLM channel implementation.
//!
//! POSTs to an OpenAI-compatible endpoint (e.g. agent-proxy-rust).

use async_trait::async_trait;
use serde::Serialize;

use crate::{CollectorError, CollectorResult, llm::LlmChannel};

/// HTTP-backed LLM channel that calls an OpenAI-compatible API.
#[derive(Debug, Clone)]
pub struct HttpLlmChannel {
    /// The full chat completions URL endpoint.
    pub endpoint: String,
    /// The model name to request.
    pub model: String,
    /// Timeout in seconds for each request.
    pub timeout_secs: u64,
}

impl HttpLlmChannel {
    /// Create a new channel.
    ///
    /// `endpoint` is the full chat completions URL, e.g.
    /// `http://127.0.0.1:11837/v1/chat/completions`.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            timeout_secs: 30,
        }
    }

    /// Create a channel from environment variables:
    /// - `AGENT_PROXY_URL` (default `http://127.0.0.1:11837`)
    /// - `EXCEPTION_LLM_MODEL` (default `deepseek-v4-flash`)
    #[must_use]
    pub fn from_env() -> Self {
        let base = std::env::var("AGENT_PROXY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11837".to_string());
        let model = std::env::var("EXCEPTION_LLM_MODEL")
            .unwrap_or_else(|_| "deepseek-v4-flash".to_string());
        Self::new(format!("{base}/v1/chat/completions"), model)
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[async_trait]
impl LlmChannel for HttpLlmChannel {
    async fn send(&self, system_prompt: &str, user_prompt: &str) -> CollectorResult<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build()
            .map_err(|e| CollectorError::Reporter {
                reason: format!("failed to build HTTP client: {e}"),
            })?;

        let req = ChatRequest {
            model: self.model.clone(),
            temperature: 0.1,
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_prompt.to_string(),
                },
            ],
        };

        let resp = client
            .post(&self.endpoint)
            .json(&req)
            .send()
            .await
            .map_err(|e| CollectorError::Reporter {
                reason: format!("LLM request failed: {e}"),
            })?;

        let body: serde_json::Value = resp.json().await.map_err(|e| CollectorError::Reporter {
            reason: format!("failed to parse LLM response: {e}"),
        })?;

        body.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(String::from)
            .ok_or_else(|| CollectorError::Reporter {
                reason: "LLM response missing content".to_string(),
            })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    reason = "test"
)]
mod tests {
    use super::*;

    #[test]
    fn test_http_channel_new_should_set_fields() {
        let ch = HttpLlmChannel::new("http://example.com/v1/chat/completions", "test-model");
        assert_eq!(ch.endpoint, "http://example.com/v1/chat/completions");
        assert_eq!(ch.model, "test-model");
        assert_eq!(ch.timeout_secs, 30);
    }

    #[test]
    fn test_http_channel_new_should_append_chat_path() {
        let ch = HttpLlmChannel::new(
            "http://127.0.0.1:11837/v1/chat/completions",
            "deepseek-v4-flash",
        );
        assert_eq!(ch.endpoint, "http://127.0.0.1:11837/v1/chat/completions");
        assert_eq!(ch.model, "deepseek-v4-flash");
    }

    #[test]
    fn test_http_channel_should_be_cloneable() {
        let ch = HttpLlmChannel::new("http://example.com/v1/chat/completions", "test-model");
        let cloned = ch.clone();
        assert_eq!(ch.endpoint, cloned.endpoint);
        assert_eq!(ch.model, cloned.model);
        assert_eq!(ch.timeout_secs, cloned.timeout_secs);
    }

    #[test]
    fn test_http_channel_should_be_debug() {
        let ch = HttpLlmChannel::new("http://example.com/v1/chat/completions", "test-model");
        let debug = format!("{ch:?}");
        assert!(debug.contains("HttpLlmChannel"));
        assert!(debug.contains("test-model"));
    }

    #[test]
    fn test_http_channel_default_timeout_should_be_30() {
        let ch = HttpLlmChannel::new("http://example.com/api", "model");
        assert_eq!(ch.timeout_secs, 30);
    }
}
