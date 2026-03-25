//! HTTP client subsystem — re-exported from [`foia_http`].
//!
//! This module also provides the [`CrawlStore`] implementation for
//! [`DieselCrawlRepository`] and the [`BrowserBackend`] implementation
//! for [`BrowserPool`], bridging `foia-http` trait abstractions to
//! the concrete database and browser types in this crate.

pub use foia_http::http_client::*;

use async_trait::async_trait;
use foia_models::{CrawlRequest, CrawlUrl};

use crate::repository::DieselCrawlRepository;

#[async_trait]
impl CrawlStore for DieselCrawlRepository {
    async fn log_request(&self, request: &mut CrawlRequest) -> anyhow::Result<()> {
        self.log_request(request).await?;
        Ok(())
    }

    async fn get_url(&self, source_id: &str, url: &str) -> anyhow::Result<Option<CrawlUrl>> {
        Ok(self.get_url(source_id, url).await?)
    }

    async fn update_url(&self, crawl_url: &CrawlUrl) -> anyhow::Result<()> {
        Ok(self.update_url(crawl_url).await?)
    }

    async fn add_url(&self, crawl_url: &CrawlUrl) -> anyhow::Result<bool> {
        Ok(self.add_url(crawl_url).await?)
    }
}

#[cfg(feature = "browser")]
mod browser_impl {
    use super::*;
    use crate::browser::BrowserPool;

    #[async_trait]
    impl BrowserBackend for BrowserPool {
        async fn fetch(&self, url: &str) -> anyhow::Result<BrowserFetchResult> {
            let response = self.fetch(url).await?;
            Ok(BrowserFetchResult {
                status: response.status,
                content_type: response.content_type,
                content: response.content,
            })
        }
    }
}
