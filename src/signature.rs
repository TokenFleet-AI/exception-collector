//! Signature computation for exception deduplication.
//!
//! Computes a SHA256-based deduplication signature from an
//! [`ExceptionRecord`](crate::ExceptionRecord) by hashing:
//!
//! - component name
//! - exception kind (lowercase display name)
//! - normalized message
//! - top 3 stack trace frames (first 3 non-empty lines)

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::{ExceptionKind, ExceptionRecord, normalize::normalize_message};

/// Compute a SHA256 deduplication signature for an exception record.
///
/// The signature is derived from:
/// 1. The `component` field
/// 2. The `kind` displayed as lowercase string
/// 3. The normalized message (via [`normalize_message`])
/// 4. The first 3 non-empty lines of the `stacktrace`
///
/// Returns a 64-character lowercase hex string.
#[must_use]
pub fn compute_signature(record: &ExceptionRecord) -> String {
    let mut hasher = Sha256::new();

    hasher.update(record.component.as_bytes());
    hasher.update(kind_to_bytes(record.kind));
    hasher.update(normalize_message(&record.message).as_bytes());

    for frame in top_stack_frames(&record.stacktrace, 3) {
        hasher.update(frame.as_bytes());
    }

    let result = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in result {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// Convert an [`ExceptionKind`] to its lowercase byte representation
/// followed by a delimiter byte to prevent ambiguity.
fn kind_to_bytes(kind: ExceptionKind) -> &'static [u8] {
    match kind {
        ExceptionKind::Panic => b"panic",
        ExceptionKind::ErrorLog => b"error_log",
        ExceptionKind::ResultErr => b"result_err",
    }
}

/// Extract the first `count` non-empty lines from a stack trace.
fn top_stack_frames(stacktrace: &str, count: usize) -> Vec<&str> {
    stacktrace
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(count)
        .collect()
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
    use crate::ExceptionRecord;

    fn make_record(
        component: &str,
        kind: ExceptionKind,
        message: &str,
        stacktrace: &str,
    ) -> ExceptionRecord {
        ExceptionRecord::new(component, kind, message, stacktrace)
    }

    #[test]
    fn test_should_produce_same_signature_for_same_exception() {
        let stacktrace = "frame1\nframe2\nframe3\nframe4\n";
        let r1 = make_record(
            "comp-a",
            ExceptionKind::Panic,
            "something broke",
            stacktrace,
        );
        let r2 = make_record(
            "comp-a",
            ExceptionKind::Panic,
            "something broke",
            stacktrace,
        );

        assert_eq!(compute_signature(&r1), compute_signature(&r2));
    }

    #[test]
    fn test_should_produce_different_signature_for_different_exceptions() {
        let r1 = make_record("comp-a", ExceptionKind::Panic, "error one", "");
        let r2 = make_record("comp-a", ExceptionKind::Panic, "error two", "");

        assert_ne!(compute_signature(&r1), compute_signature(&r2));
    }

    #[test]
    fn test_should_use_top_3_stack_frames_only() {
        let many_frames = "frame1\nframe2\nframe3\nframe4\nframe5\nframe6\n";
        let few_frames = "frame1\nframe2\nframe3\n";

        let r1 = make_record("comp", ExceptionKind::ErrorLog, "msg", many_frames);
        let r2 = make_record("comp", ExceptionKind::ErrorLog, "msg", few_frames);

        // Both should produce the same signature because only top 3 frames are used
        assert_eq!(compute_signature(&r1), compute_signature(&r2));
    }

    #[test]
    fn test_should_include_component_in_signature() {
        let r1 = make_record("comp-a", ExceptionKind::Panic, "msg", "");
        let r2 = make_record("comp-b", ExceptionKind::Panic, "msg", "");

        assert_ne!(compute_signature(&r1), compute_signature(&r2));
    }

    #[test]
    fn test_should_include_exception_kind_in_signature() {
        let r1 = make_record("comp", ExceptionKind::Panic, "msg", "");
        let r2 = make_record("comp", ExceptionKind::ErrorLog, "msg", "");
        let r3 = make_record("comp", ExceptionKind::ResultErr, "msg", "");

        assert_ne!(compute_signature(&r1), compute_signature(&r2));
        assert_ne!(compute_signature(&r1), compute_signature(&r3));
        assert_ne!(compute_signature(&r2), compute_signature(&r3));
    }

    #[test]
    fn test_should_return_64_char_hex_string() {
        let record = make_record("comp", ExceptionKind::Panic, "msg", "");
        let sig = compute_signature(&record);

        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_should_handle_empty_stacktrace() {
        let record = make_record("comp", ExceptionKind::ResultErr, "some error", "");
        let sig = compute_signature(&record);

        assert_eq!(sig.len(), 64);
    }

    #[test]
    fn test_should_handle_stacktrace_with_blank_lines() {
        let with_blanks = "\n\nframe1\n\nframe2\nframe3\n\n";
        let compact = "frame1\nframe2\nframe3";

        let r1 = make_record("comp", ExceptionKind::ErrorLog, "msg", with_blanks);
        let r2 = make_record("comp", ExceptionKind::ErrorLog, "msg", compact);

        // Both should produce the same signature (blank lines are skipped)
        assert_eq!(compute_signature(&r1), compute_signature(&r2));
    }
}
