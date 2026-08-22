//! The one error type the provider layer produces.
//!
//! `status` drives retry classification (`retry::is_retryable`); the
//! constructor default of 502 means "transport fault, always retryable". A
//! user abort is 499 (`sse::aborted`), a missing key 401 (`routing::
//! require_key`), and a provider's own status is passed through verbatim.
//!
//! **Error text is a product surface.** `Display` is the message alone — the
//! host's turn runner hands it to the model as the exception a program sees,
//! and its tests grep substrings of it.

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq)]
#[error("{message}")]
pub struct LlmError {
    /// `None` in spirit = transport fault; materialised as the 502 default.
    pub status: u16,
    /// The provider's `Retry-After`, in ms, when it sent one.
    pub retry_after_ms: Option<u64>,
    pub message: String,
}

impl LlmError {
    /// A status-less fault: the 502 default, always retryable.
    pub fn new(message: impl Into<String>) -> Self {
        LlmError {
            status: 502,
            retry_after_ms: None,
            message: message.into(),
        }
    }

    /// An explicit status and optional Retry-After hint (ms).
    pub fn with(message: impl Into<String>, status: u16, retry_after_ms: Option<u64>) -> Self {
        LlmError {
            status,
            retry_after_ms,
            message: message.into(),
        }
    }

    /// The HTTP status this error should become.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The class name that appears in logs and trace files.
    pub fn name(&self) -> &'static str {
        "LlmError"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_drives_retry_classification() {
        let transport = LlmError::new("connection reset");
        assert_eq!(transport.status(), 502);
        assert_eq!(transport.retry_after_ms, None);
        let rate_limited = LlmError::with("overloaded", 429, Some(1500));
        assert_eq!(rate_limited.status(), 429);
        assert_eq!(rate_limited.retry_after_ms, Some(1500));
    }

    #[test]
    fn display_is_the_message() {
        assert_eq!(
            LlmError::new("stream stalled").to_string(),
            "stream stalled"
        );
        assert_eq!(LlmError::new("x").name(), "LlmError");
    }
}
