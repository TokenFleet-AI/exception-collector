//! 指数退避重试策略。
//!
//! 用于上报失败批次的自动重试：5min → 25min → 125min，
//! 3 次尝试后放弃并标记 Failed。
//!
//! # 示例
//!
//! ```rust,ignore
//! use exception_collector::retry::RetryPolicy;
//!
//! let policy = RetryPolicy::default();
//! let result = with_retry(&policy, || async {
//!     // 可能失败的操作
//!     Ok::<_, String>("success")
//! }).await;
//! ```

use std::{cell::RefCell, future::Future, time::Duration};

/// 指数退避重试策略。
///
/// 控制重试次数和退避延迟，内部跟踪当前尝试次数。
///
/// # 默认值
///
/// - `max_retries`: 3
/// - `base_delay`: 5 分钟
///
/// # 退避计算
///
/// 第 `n` 次重试的延迟为 `base_delay * 5^n`：
/// - 第 1 次重试：5min
/// - 第 2 次重试：25min
/// - 第 3 次重试：125min
#[derive(Debug)]
pub struct RetryPolicy {
    /// 最大重试次数。
    max_retries: u32,
    /// 基础延迟，用于指数退避计算。
    base_delay: Duration,
    /// 当前已尝试次数，用于跟踪重试状态。
    attempts: RefCell<u32>,
}

impl RetryPolicy {
    /// 创建一个新的重试策略。
    ///
    /// `max_retries` 为 0 时不重试，直接返回首次结果。
    #[must_use]
    pub fn new(max_retries: u32, base_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            attempts: RefCell::new(0),
        }
    }

    /// 返回当前已尝试次数。
    #[must_use]
    pub fn attempts(&self) -> u32 {
        *self.attempts.borrow()
    }

    /// 计算第 `attempt` 次重试的退避延迟。
    ///
    /// 使用指数退避公式：`base_delay * 5^attempt`。
    /// 对溢出进行保护，超过 `Duration::MAX` 时返回 `Duration::MAX`。
    #[must_use]
    pub fn backoff_delay(&self, attempt: u32) -> Duration {
        let factor = 5_u64.saturating_pow(attempt);
        let secs = self.base_delay.as_secs().saturating_mul(factor);
        Duration::from_secs(secs)
    }

    /// 重置尝试计数器。
    ///
    /// 在成功上报后调用，为下一次可能的故障重置状态。
    pub fn reset(&self) {
        *self.attempts.borrow_mut() = 0;
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(3, Duration::from_mins(5))
    }
}

/// 对异步操作执行指数退避重试。
///
/// 根据 `RetryPolicy` 的配置，在操作失败时自动重试，
/// 每次重试之间等待指数增长的退避延迟。
///
/// # 返回
///
/// - 操作成功时立即返回 `Ok`
/// - 所有重试耗尽后返回最后一次错误
///
/// # 示例
///
/// ```rust,ignore
/// use exception_collector::retry::{RetryPolicy, with_retry};
///
/// let policy = RetryPolicy::default();
/// let result = with_retry(&policy, || async {
///     upload_batch().await
/// }).await;
/// ```
///
/// # Errors
///
/// 返回闭包产生的错误类型，当所有重试耗尽时。
pub async fn with_retry<F, Fut, T, E>(policy: &RetryPolicy, f: F) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    policy.reset();

    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        *policy.attempts.borrow_mut() = attempt;

        match f().await {
            Ok(value) => {
                policy.reset();
                return Ok(value);
            }
            Err(err) => {
                // 已用尽所有重试（首次 + max_retries 次重试）
                if attempt > policy.max_retries {
                    return Err(err);
                }
                let delay = policy.backoff_delay(attempt - 1);
                tokio::time::sleep(delay).await;
            }
        }
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
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn test_retry_policy_default_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 3);
        assert_eq!(policy.base_delay, Duration::from_mins(5));
    }

    #[test]
    fn test_retry_policy_custom_values() {
        let policy = RetryPolicy::new(5, Duration::from_secs(10));
        assert_eq!(policy.max_retries, 5);
        assert_eq!(policy.base_delay, Duration::from_secs(10));
    }

    #[test]
    fn test_retry_policy_starts_at_zero_attempts() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.attempts(), 0);
    }

    #[test]
    fn test_retry_policy_backoff_delay_exponential() {
        let policy = RetryPolicy::new(3, Duration::from_mins(5));

        // 5min → 25min → 125min
        assert_eq!(policy.backoff_delay(0), Duration::from_mins(5));
        assert_eq!(policy.backoff_delay(1), Duration::from_mins(25));
        assert_eq!(policy.backoff_delay(2), Duration::from_mins(125));
    }

    #[test]
    fn test_retry_policy_backoff_delay_protects_overflow() {
        let policy = RetryPolicy::new(100, Duration::from_secs(u64::MAX));
        let delay = policy.backoff_delay(10);
        // saturating_mul returns u64::MAX when overflow
        assert_eq!(delay, Duration::from_secs(u64::MAX));
    }

    #[test]
    fn test_retry_policy_reset_clears_attempts() {
        let policy = RetryPolicy::default();
        *policy.attempts.borrow_mut() = 5;
        assert_eq!(policy.attempts(), 5);

        policy.reset();
        assert_eq!(policy.attempts(), 0);
    }

    #[tokio::test]
    async fn test_should_succeed_on_first_attempt() {
        let policy = RetryPolicy::default();
        let counter = AtomicU32::new(0);

        let result = with_retry(&policy, || async {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>("ok")
        })
        .await;

        assert_eq!(result, Ok("ok"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_should_retry_up_to_max() {
        let policy = RetryPolicy::new(2, Duration::from_millis(1));
        let counter = AtomicU32::new(0);

        let result = with_retry(&policy, || async {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                Err::<(), _>("fail".to_string())
            } else {
                Ok::<_, String>(())
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_should_mark_failed_after_exhaustion() {
        let policy = RetryPolicy::new(2, Duration::from_millis(1));

        let result = with_retry(&policy, || async {
            Err::<String, _>("permanent_fail".to_string())
        })
        .await;

        assert_eq!(result, Err("permanent_fail".to_string()));
        assert_eq!(policy.attempts(), 3);
    }

    #[tokio::test]
    async fn test_should_reset_after_success() {
        let policy = RetryPolicy::new(3, Duration::from_millis(1));
        let call_count = AtomicU32::new(0);

        // 第一次：失败后成功
        let result = with_retry(&policy, || async {
            let n = call_count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err::<(), _>("first_fail".to_string())
            } else {
                Ok::<_, String>(())
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(policy.attempts(), 0);

        // 第二次：直接成功，尝试计数器应从 0 开始
        let result = with_retry(&policy, || async { Ok::<_, String>(()) }).await;

        assert!(result.is_ok());
        assert_eq!(policy.attempts(), 0);
    }

    #[tokio::test]
    async fn test_should_not_retry_when_max_retries_zero() {
        let policy = RetryPolicy::new(0, Duration::from_millis(1));
        let counter = AtomicU32::new(0);

        let result = with_retry(&policy, || async {
            counter.fetch_add(1, Ordering::SeqCst);
            Err::<String, _>("fail".to_string())
        })
        .await;

        assert_eq!(result, Err("fail".to_string()));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
