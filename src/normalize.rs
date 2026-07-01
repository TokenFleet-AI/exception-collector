//! Normalize error messages by replacing dynamic values with placeholders.
//!
//! This module provides [`normalize_message`], which replaces:
//! - File paths (e.g., `/home/user/project/src/main.rs:42`) with `*/src/*:*`
//! - IPv4 addresses (e.g., `192.168.1.1`) with `*`
//! - IPv6 addresses (e.g., `::1`) with `*`
//! - UUIDs (e.g., `550e8400-e29b-41d4-a716-446655440000`) with `*`
//! - Integer and float numbers with `*`
//!
//! The normalization is applied in order from most-specific to least-specific
//! pattern, ensuring that composite patterns (UUIDs, IPs, paths) are replaced
//! before generic number replacement.

use std::sync::LazyLock;

use regex::Regex;

#[allow(
    clippy::unwrap_used,
    reason = "compile-time constant regex that cannot fail"
)]
static RE_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S+/src/\S+:\d+").unwrap());

#[allow(
    clippy::unwrap_used,
    reason = "compile-time constant regex that cannot fail"
)]
static RE_UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{?[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\}?")
        .unwrap()
});

#[allow(
    clippy::unwrap_used,
    reason = "compile-time constant regex that cannot fail"
)]
static RE_IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap());

#[allow(
    clippy::unwrap_used,
    reason = "compile-time constant regex that cannot fail"
)]
static RE_IPV6: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:|(?:[0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,5}(?::[0-9a-fA-F]{1,4}){1,2}|(?:[0-9a-fA-F]{1,4}:){1,4}(?::[0-9a-fA-F]{1,4}){1,3}|(?:[0-9a-fA-F]{1,4}:){1,3}(?::[0-9a-fA-F]{1,4}){1,4}|(?:[0-9a-fA-F]{1,4}:){1,2}(?::[0-9a-fA-F]{1,4}){1,5}|[0-9a-fA-F]{1,4}:(?::[0-9a-fA-F]{1,4}){1,6}|:(?::[0-9a-fA-F]{1,4}){1,7}|::(?:[fF]{4}:)?(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|fe80:(?::[0-9a-fA-F]{0,4}){0,4}%[0-9a-zA-Z]+|::(?:[fF]{4}:)?(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)|(?:[0-9a-fA-F]{1,4}:){1,4}:(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)",
    )
    .unwrap()
});

#[allow(
    clippy::unwrap_used,
    reason = "compile-time constant regex that cannot fail"
)]
static RE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+\.?\d*\b").unwrap());

/// Normalize an error message by replacing dynamic values with placeholders.
///
/// Applies the following replacements in order (most-specific first):
/// 1. File paths matching `/\S+/src/\S+:\d+` -> `*/src/*:*`
/// 2. UUIDs (8-4-4-4-12 hex format) -> `*`
/// 3. IPv4 addresses -> `*`
/// 4. IPv6 addresses -> `*`
/// 5. Integer and float numbers -> `*`
///
/// # Examples
///
/// ```ignore
/// // The normalize module is private; this is a documentation example.
/// use exception_collector::normalize::normalize_message;
/// // File paths are normalized:
/// assert!(normalize_message("panic at /home/proj/src/main.rs:42").contains("*/src/*:*"));
/// // UUIDs are replaced:
/// assert!(normalize_message("user 550e8400-e29b-41d4-a716-446655440000").contains("user *"));
/// // Numbers are replaced:
/// assert_eq!(normalize_message("error code 42"), "error code *");
/// ```
#[must_use]
pub fn normalize_message(msg: &str) -> String {
    let result = RE_PATH.replace_all(msg, "*/src/*:*");
    let result = RE_UUID.replace_all(&result, "*");
    let result = RE_IPV4.replace_all(&result, "*");
    let result = RE_IPV6.replace_all(&result, "*");
    let result = RE_NUMBER.replace_all(&result, "*");
    result.into_owned()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    reason = "test module relaxes production lint strictness"
)]
mod tests {
    use super::*;

    #[test]
    fn test_should_replace_numbers_with_wildcard() {
        assert_eq!(normalize_message("error code 42"), "error code *");
        assert_eq!(
            normalize_message("retry after 3 seconds"),
            "retry after * seconds"
        );
        assert_eq!(normalize_message("value is 3.14"), "value is *");
        assert_eq!(normalize_message("got 0 and 999"), "got * and *");
    }

    #[test]
    fn test_should_replace_file_paths_with_wildcard() {
        assert_eq!(
            normalize_message("panic at /home/user/project/src/main.rs:42"),
            "panic at */src/*:*"
        );
        assert_eq!(
            normalize_message("/opt/app/src/lib/mod.rs:128 failed"),
            "*/src/*:* failed"
        );
    }

    #[test]
    fn test_should_replace_ip_addresses() {
        assert_eq!(
            normalize_message("connection refused from 192.168.1.1"),
            "connection refused from *"
        );
        assert_eq!(
            normalize_message("timeout to 10.0.0.1:8080"),
            "timeout to *:*"
        );
        assert_eq!(
            normalize_message("loopback ::1 unreachable"),
            "loopback * unreachable"
        );
    }

    #[test]
    fn test_should_replace_uuids() {
        assert_eq!(
            normalize_message("user 550e8400-e29b-41d4-a716-446655440000 not found"),
            "user * not found"
        );
        assert_eq!(
            normalize_message("request {a1b2c3d4-e5f6-7890-abcd-ef1234567890} failed"),
            "request * failed"
        );
    }

    #[test]
    fn test_should_preserve_error_structure() {
        // Non-dynamic text should be preserved
        assert_eq!(normalize_message("ConnectionRefused"), "ConnectionRefused");
        assert_eq!(
            normalize_message("failed to open file"),
            "failed to open file"
        );
        // Mixed content: text preserved, numbers replaced
        let result = normalize_message("timeout after 30 seconds connecting to 10.0.0.1");
        assert!(result.contains("timeout after"));
        assert!(result.contains("seconds connecting to"));
        assert!(!result.contains("30"));
        assert!(!result.contains("10.0.0.1"));
    }

    #[test]
    fn test_should_handle_empty_message() {
        assert_eq!(normalize_message(""), "");
    }
}
