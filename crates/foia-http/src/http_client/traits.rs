//! Trait abstractions for pluggable HTTP client backends.
//!
//! These traits decouple the HTTP client from concrete database and browser
//! implementations, allowing `foia-http` to remain independent of Diesel
//! and browser automation crates.

use async_trait::async_trait;
use foia_models::{CrawlRequest, CrawlUrl};

/// Storage backend for crawl request logging and URL tracking.
///
/// Implemented by `DieselCrawlRepository` in `foia`.
#[async_trait]
pub trait CrawlStore: Send + Sync {
    /// Log an HTTP request/response pair.
    async fn log_request(&self, request: &mut CrawlRequest) -> anyhow::Result<()>;

    /// Look up a crawl URL by source and URL.
    async fn get_url(&self, source_id: &str, url: &str) -> anyhow::Result<Option<CrawlUrl>>;

    /// Update an existing crawl URL's status.
    async fn update_url(&self, crawl_url: &CrawlUrl) -> anyhow::Result<()>;

    /// Add a new URL to the crawl queue. Returns true if newly added.
    async fn add_url(&self, crawl_url: &CrawlUrl) -> anyhow::Result<bool>;
}

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
