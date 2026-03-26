//! Trait for crawl request logging and URL tracking.

use async_trait::async_trait;

use crate::{CrawlRequest, CrawlUrl};

/// Storage backend for crawl request logging and URL tracking.
///
/// Implemented by `DieselCrawlRepository` in `foia-db`.
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
