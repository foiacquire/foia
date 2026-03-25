//! Rate limiting infrastructure for API and scraper requests.
//!
//! Provides adaptive per-domain rate limiting with pluggable backends:
//! - In-memory (default, ephemeral)
//! - SQLite/PostgreSQL via Diesel (persistent, single-instance) — in `foia-db`
//! - Redis (distributed, multi-instance) — in `foia`
//!
//! Used by both scrapers and cloud API backends (Groq, Gemini, etc.).

#![allow(dead_code)]
#![allow(unused_imports)]

mod backend;
mod config;
mod limiter;
mod memory;

// Re-export main types
pub use backend::{DomainRateState, ForbiddenRecord, RateLimitBackend, RateLimitError, RateLimitResult};
pub use config::{DomainStats, RateLimitConfig, RATE_LIMIT_403_THRESHOLD, RATE_LIMIT_WINDOW};
pub use limiter::{BoxedRateLimitBackend, RateLimiter};
pub use memory::InMemoryRateLimitBackend;

/// Parse Retry-After header value (seconds).
/// Returns duration to wait, or None if header is missing/invalid.
pub fn parse_retry_after(header_value: Option<&str>) -> Option<std::time::Duration> {
    let value = header_value?;
    value
        .parse::<u64>()
        .ok()
        .map(|secs| std::time::Duration::from_secs(secs.min(60)))
}

/// Calculate exponential backoff delay for a given attempt.
pub fn backoff_delay(attempt: u32, base_ms: u64) -> std::time::Duration {
    let delay_ms = base_ms * 2u64.pow(attempt);
    std::time::Duration::from_millis(delay_ms.min(60_000))
}

/// Get delay from environment variable, with default fallback.
pub fn get_delay_from_env(env_var: &str, default_ms: u64) -> std::time::Duration {
    std::env::var(env_var)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_millis(default_ms))
}
