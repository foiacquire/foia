//! API rate limiting helpers for cloud OCR backends.
//!
//! Provides retry logic with exponential backoff and Retry-After header support.

#![allow(dead_code)]

use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Default delay between API requests (milliseconds).
const DEFAULT_DELAY_MS: u64 = 200;

/// Maximum retry attempts on rate limit errors.
const MAX_RETRIES: u32 = 5;

/// Maximum backoff delay (seconds).
const MAX_BACKOFF_SECS: u64 = 60;

/// Get the configured delay for an API from environment variable.
/// Falls back to default if not set.
pub fn get_api_delay(env_var: &str) -> Duration {
    std::env::var(env_var)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(DEFAULT_DELAY_MS))
}

/// Parse Retry-After header value (seconds or HTTP date).
/// Returns duration to wait, or None if header is missing/invalid.
pub fn parse_retry_after(header_value: Option<&str>) -> Option<Duration> {
    let value = header_value?;

    // Try parsing as seconds first
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(MAX_BACKOFF_SECS)));
    }

    // Could add HTTP date parsing here if needed
    None
}

/// Calculate exponential backoff delay for a given attempt.
pub fn backoff_delay(attempt: u32, base_ms: u64) -> Duration {
    let delay_ms = base_ms * 2u64.pow(attempt);
    Duration::from_millis(delay_ms.min(MAX_BACKOFF_SECS * 1000))
}

/// Rate limit state for tracking request timing.
#[derive(Debug, Clone)]
pub struct ApiRateLimiter {
    /// Name of the API (for logging).
    pub name: String,
    /// Environment variable for delay configuration.
    pub delay_env_var: String,
    /// Last request timestamp.
    last_request: Option<std::time::Instant>,
}

impl ApiRateLimiter {
    /// Create a new rate limiter for an API.
    pub fn new(name: impl Into<String>, delay_env_var: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            delay_env_var: delay_env_var.into(),
            last_request: None,
        }
    }

    /// Wait for the configured delay since the last request.
    pub async fn wait_for_slot(&mut self) {
        let delay = get_api_delay(&self.delay_env_var);

        if let Some(last) = self.last_request {
            let elapsed = last.elapsed();
            if elapsed < delay {
                let wait = delay - elapsed;
                debug!("{}: waiting {:?} before next request", self.name, wait);
                sleep(wait).await;
            }
        }

        self.last_request = Some(std::time::Instant::now());
    }

    /// Handle a rate limit response, returning how long to wait.
    /// Returns None if max retries exceeded.
    pub fn handle_rate_limit(
        &self,
        attempt: u32,
        retry_after: Option<&str>,
    ) -> Option<Duration> {
        if attempt >= MAX_RETRIES {
            warn!("{}: max retries ({}) exceeded", self.name, MAX_RETRIES);
            return None;
        }

        let wait = if let Some(duration) = parse_retry_after(retry_after) {
            debug!("{}: rate limited, Retry-After: {:?}", self.name, duration);
            duration
        } else {
            let backoff = backoff_delay(attempt, 1000);
            debug!("{}: rate limited, backing off {:?}", self.name, backoff);
            backoff
        };

        Some(wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_retry_after_seconds() {
        assert_eq!(parse_retry_after(Some("5")), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after(Some("0")), Some(Duration::from_secs(0)));
        assert_eq!(parse_retry_after(Some("100")), Some(Duration::from_secs(60))); // capped
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        assert_eq!(parse_retry_after(None), None);
        assert_eq!(parse_retry_after(Some("invalid")), None);
    }

    #[test]
    fn test_backoff_delay() {
        assert_eq!(backoff_delay(0, 1000), Duration::from_millis(1000));
        assert_eq!(backoff_delay(1, 1000), Duration::from_millis(2000));
        assert_eq!(backoff_delay(2, 1000), Duration::from_millis(4000));
        assert_eq!(backoff_delay(10, 1000), Duration::from_secs(60)); // capped
    }
}
