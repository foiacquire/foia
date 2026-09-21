//! Rate limiting infrastructure — core re-exported from [`foia_http`].
//!
//! This module re-exports the core rate limiting types (traits, in-memory backend,
//! limiter, config) from `foia-http`, and provides the database-backed backends
//! locally (SQLite via Diesel, Redis).

#![allow(dead_code)]
#![allow(unused_imports)]

mod sqlite;

#[cfg(feature = "redis-backend")]
mod redis;

// Re-export everything from foia-http's rate_limit
pub use foia_http::rate_limit::*;

// Local backends (depend on Diesel/Redis)
pub use sqlite::DieselRateLimitBackend;

#[cfg(feature = "redis-backend")]
pub use redis::RedisRateLimitBackend;
