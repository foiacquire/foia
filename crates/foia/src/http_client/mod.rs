//! HTTP client subsystem — re-exported from [`foia_http`].
//!
//! CrawlStore is implemented by DieselCrawlRepository in foia-db.
//! BrowserBackend is implemented for BrowserPool here.

pub use foia_http::http_client::*;

#[cfg(feature = "browser")]
mod browser_impl {
    use super::*;
    use async_trait::async_trait;
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
