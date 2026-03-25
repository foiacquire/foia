//! Trait abstractions for pluggable HTTP client backends.
//!
//! These traits decouple the HTTP client from concrete database and browser
//! implementations, allowing `foia-http` to remain independent of Diesel
//! and browser automation crates.

use async_trait::async_trait;

// CrawlStore is defined in foia-models (domain-level trait).
// Re-exported here for convenience.
pub use foia_models::CrawlStore;

/// Result from a browser-based page fetch.
pub struct BrowserFetchResult {
    /// HTTP status code.
    pub status: u16,
    /// Content-Type header value.
    pub content_type: String,
    /// Page content (HTML body).
    pub content: String,
}

/// Backend for browser-based page fetching (headless Chrome, etc.).
///
/// Implemented by `BrowserPool` in `foia` (behind the `browser` feature).
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    /// Fetch a URL using a headless browser.
    async fn fetch(&self, url: &str) -> anyhow::Result<BrowserFetchResult>;
}
