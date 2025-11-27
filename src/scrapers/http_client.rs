//! HTTP client with ETag and conditional request support.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use reqwest::{Client, Response, StatusCode};
use tokio::sync::Mutex;

use super::rate_limiter::RateLimiter;
use crate::models::{CrawlRequest, CrawlUrl, UrlStatus};
use crate::repository::CrawlRepository;

const USER_AGENT: &str = "FOIAcquire/0.1 (academic research; github.com/monokrome/foiacquire)";

/// Real browser user agents for impersonate mode.
/// These are current user agents from popular browsers (updated Nov 2024).
const IMPERSONATE_USER_AGENTS: &[&str] = &[
    // Chrome on Windows
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
    // Chrome on Mac
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
    // Firefox on Windows
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:133.0) Gecko/20100101 Firefox/133.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:132.0) Gecko/20100101 Firefox/132.0",
    // Firefox on Mac
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:133.0) Gecko/20100101 Firefox/133.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:132.0) Gecko/20100101 Firefox/132.0",
    // Safari on Mac
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15",
    // Edge on Windows
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
];

/// Get a random user agent for impersonate mode.
fn random_user_agent() -> &'static str {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as usize)
        .unwrap_or(0);
    IMPERSONATE_USER_AGENTS[nanos % IMPERSONATE_USER_AGENTS.len()]
}

/// Resolve user agent from config value.
/// - None => default FOIAcquire user agent
/// - "impersonate" => random real browser user agent
/// - other => custom user agent string
pub fn resolve_user_agent(config: Option<&str>) -> String {
    match config {
        None => USER_AGENT.to_string(),
        Some("impersonate") => random_user_agent().to_string(),
        Some(custom) => custom.to_string(),
    }
}

/// HTTP client with request logging and conditional request support.
#[derive(Clone)]
pub struct HttpClient {
    client: Client,
    crawl_repo: Option<Arc<Mutex<CrawlRepository>>>,
    source_id: String,
    request_delay: Duration,
    referer: Option<String>,
    rate_limiter: RateLimiter,
}

impl HttpClient {
    /// Create a new HTTP client.
    pub fn new(source_id: &str, timeout: Duration, request_delay: Duration) -> Self {
        Self::with_user_agent(source_id, timeout, request_delay, None)
    }

    /// Create a new HTTP client with custom user agent configuration.
    /// - None: Use default FOIAcquire user agent
    /// - Some("impersonate"): Use random real browser user agent
    /// - Some(custom): Use custom user agent string
    pub fn with_user_agent(
        source_id: &str,
        timeout: Duration,
        request_delay: Duration,
        user_agent_config: Option<&str>,
    ) -> Self {
        let user_agent = resolve_user_agent(user_agent_config);
        let client = Client::builder()
            .user_agent(&user_agent)
            .timeout(timeout)
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            crawl_repo: None,
            source_id: source_id.to_string(),
            request_delay,
            referer: None,
            rate_limiter: RateLimiter::new(),
        }
    }

    /// Create a new HTTP client with a shared rate limiter.
    pub fn with_rate_limiter(
        source_id: &str,
        timeout: Duration,
        request_delay: Duration,
        rate_limiter: RateLimiter,
    ) -> Self {
        Self::with_rate_limiter_and_user_agent(
            source_id,
            timeout,
            request_delay,
            rate_limiter,
            None,
        )
    }

    /// Create a new HTTP client with a shared rate limiter and custom user agent.
    pub fn with_rate_limiter_and_user_agent(
        source_id: &str,
        timeout: Duration,
        request_delay: Duration,
        rate_limiter: RateLimiter,
        user_agent_config: Option<&str>,
    ) -> Self {
        let user_agent = resolve_user_agent(user_agent_config);
        let client = Client::builder()
            .user_agent(&user_agent)
            .timeout(timeout)
            .gzip(true)
            .brotli(true)
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            crawl_repo: None,
            source_id: source_id.to_string(),
            request_delay,
            referer: None,
            rate_limiter,
        }
    }

    /// Set the crawl repository for request logging.
    pub fn with_crawl_repo(mut self, repo: Arc<Mutex<CrawlRepository>>) -> Self {
        self.crawl_repo = Some(repo);
        self
    }

    /// Set the Referer header for requests.
    pub fn with_referer(mut self, referer: String) -> Self {
        self.referer = Some(referer);
        self
    }

    /// Get the rate limiter for this client.
    pub fn rate_limiter(&self) -> &RateLimiter {
        &self.rate_limiter
    }

    /// Make a GET request with optional conditional headers.
    /// Uses adaptive rate limiting per domain.
    pub async fn get(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<HttpResponse, reqwest::Error> {
        // Wait for rate limiter before making request
        let domain = self.rate_limiter.acquire(url).await;

        let mut request = self.client.get(url);

        let mut headers = HashMap::new();

        // Add conditional request headers
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
            headers.insert("If-None-Match".to_string(), etag.to_string());
        }
        if let Some(lm) = last_modified {
            request = request.header("If-Modified-Since", lm);
            headers.insert("If-Modified-Since".to_string(), lm.to_string());
        }

        let was_conditional = etag.is_some() || last_modified.is_some();

        // Create request log
        let mut request_log =
            CrawlRequest::new(self.source_id.clone(), url.to_string(), "GET".to_string());
        request_log.request_headers = headers;
        request_log.was_conditional = was_conditional;

        let start = Instant::now();
        let response = request.send().await?;
        let duration = start.elapsed();

        let status_code = response.status().as_u16();

        // Update request log
        request_log.response_at = Some(Utc::now());
        request_log.duration_ms = Some(duration.as_millis() as u64);
        request_log.response_status = Some(status_code);
        request_log.was_not_modified = response.status() == StatusCode::NOT_MODIFIED;

        // Extract response headers
        let mut response_headers = HashMap::new();
        for (name, value) in response.headers() {
            if let Ok(v) = value.to_str() {
                response_headers.insert(name.to_string(), v.to_string());
            }
        }
        request_log.response_headers = response_headers.clone();

        // Log the request
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            let _ = repo.log_request(&request_log);
        }

        // Report status to rate limiter for adaptive backoff
        if let Some(ref domain) = domain {
            let has_retry_after = response_headers.contains_key("retry-after");

            if status_code == 429 || status_code == 503 {
                // Definite rate limit
                self.rate_limiter
                    .report_rate_limit(domain, status_code)
                    .await;
            } else if status_code == 403 {
                // Possible rate limit - needs pattern detection
                self.rate_limiter
                    .report_403(domain, url, has_retry_after)
                    .await;
            } else if status_code >= 500 {
                // Server error - mild backoff
                self.rate_limiter.report_server_error(domain).await;
            } else if response.status().is_success() || status_code == 304 {
                // Success - may recover from backoff
                self.rate_limiter.report_success(domain).await;
            }
        }

        // Apply base delay (rate limiter handles additional adaptive delay)
        tokio::time::sleep(self.request_delay).await;

        Ok(HttpResponse {
            status: response.status(),
            headers: response_headers,
            response,
        })
    }

    /// Get page content as text.
    pub async fn get_text(&self, url: &str) -> Result<String, reqwest::Error> {
        let response = self.get(url, None, None).await?;
        response.response.text().await
    }

    /// Make a HEAD request to check headers without downloading content.
    /// Returns headers including ETag, Last-Modified, Content-Disposition, etc.
    pub async fn head(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<HeadResponse, reqwest::Error> {
        // Wait for rate limiter before making request
        let domain = self.rate_limiter.acquire(url).await;

        let mut request = self.client.head(url);

        let mut headers = HashMap::new();

        // Add conditional request headers
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
            headers.insert("If-None-Match".to_string(), etag.to_string());
        }
        if let Some(lm) = last_modified {
            request = request.header("If-Modified-Since", lm);
            headers.insert("If-Modified-Since".to_string(), lm.to_string());
        }

        let was_conditional = etag.is_some() || last_modified.is_some();

        // Create request log
        let mut request_log =
            CrawlRequest::new(self.source_id.clone(), url.to_string(), "HEAD".to_string());
        request_log.request_headers = headers;
        request_log.was_conditional = was_conditional;

        let start = Instant::now();
        let response = request.send().await?;
        let duration = start.elapsed();

        let status_code = response.status().as_u16();

        // Update request log
        request_log.response_at = Some(Utc::now());
        request_log.duration_ms = Some(duration.as_millis() as u64);
        request_log.response_status = Some(status_code);
        request_log.was_not_modified = response.status() == StatusCode::NOT_MODIFIED;

        // Extract response headers
        let mut response_headers = HashMap::new();
        for (name, value) in response.headers() {
            if let Ok(v) = value.to_str() {
                response_headers.insert(name.to_string(), v.to_string());
            }
        }
        request_log.response_headers = response_headers.clone();

        // Log the request
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            let _ = repo.log_request(&request_log);
        }

        // Report status to rate limiter
        if let Some(ref domain) = domain {
            let has_retry_after = response_headers.contains_key("retry-after");

            if status_code == 429 || status_code == 503 {
                self.rate_limiter
                    .report_rate_limit(domain, status_code)
                    .await;
            } else if status_code == 403 {
                self.rate_limiter
                    .report_403(domain, url, has_retry_after)
                    .await;
            } else if status_code >= 500 {
                self.rate_limiter.report_server_error(domain).await;
            } else if response.status().is_success() || status_code == 304 {
                self.rate_limiter.report_success(domain).await;
            }
        }

        // Apply base delay
        tokio::time::sleep(self.request_delay).await;

        Ok(HeadResponse {
            status: response.status(),
            headers: response_headers,
        })
    }

    /// Update crawl URL status to fetching.
    pub async fn mark_fetching(&self, url: &str) {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(mut crawl_url)) = repo.get_url(&self.source_id, url) {
                crawl_url.mark_fetching();
                let _ = repo.update_url(&crawl_url);
            }
        }
    }

    /// Update crawl URL status after successful fetch.
    pub async fn mark_fetched(
        &self,
        url: &str,
        content_hash: Option<String>,
        document_id: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
    ) {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(mut crawl_url)) = repo.get_url(&self.source_id, url) {
                crawl_url.mark_fetched(content_hash, document_id, etag, last_modified);
                let _ = repo.update_url(&crawl_url);
            }
        }
    }

    /// Update crawl URL status after skip (304 Not Modified).
    pub async fn mark_skipped(&self, url: &str, reason: &str) {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(mut crawl_url)) = repo.get_url(&self.source_id, url) {
                crawl_url.mark_skipped(reason);
                let _ = repo.update_url(&crawl_url);
            }
        }
    }

    /// Update crawl URL status after failure.
    pub async fn mark_failed(&self, url: &str, error: &str) {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(mut crawl_url)) = repo.get_url(&self.source_id, url) {
                crawl_url.mark_failed(error, 3);
                let _ = repo.update_url(&crawl_url);
            }
        }
    }

    /// Track a discovered URL.
    pub async fn track_url(&self, crawl_url: &CrawlUrl) -> bool {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            repo.add_url(crawl_url).unwrap_or(false)
        } else {
            false
        }
    }

    /// Check if URL was already fetched.
    pub async fn is_fetched(&self, url: &str) -> bool {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(crawl_url)) = repo.get_url(&self.source_id, url) {
                matches!(crawl_url.status, UrlStatus::Fetched | UrlStatus::Skipped)
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Get cached headers for a URL.
    pub async fn get_cached_headers(&self, url: &str) -> (Option<String>, Option<String>) {
        if let Some(repo) = &self.crawl_repo {
            let repo = repo.lock().await;
            if let Ok(Some(crawl_url)) = repo.get_url(&self.source_id, url) {
                return (crawl_url.etag, crawl_url.last_modified);
            }
        }
        (None, None)
    }
}

/// HTTP response wrapper.
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HashMap<String, String>,
    response: Response,
}

impl HttpResponse {
    /// Check if the response is 304 Not Modified.
    pub fn is_not_modified(&self) -> bool {
        self.status == StatusCode::NOT_MODIFIED
    }

    /// Check if the response is successful.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// Get the ETag header.
    pub fn etag(&self) -> Option<&str> {
        self.headers.get("etag").map(|s| s.as_str())
    }

    /// Get the Last-Modified header.
    pub fn last_modified(&self) -> Option<&str> {
        self.headers.get("last-modified").map(|s| s.as_str())
    }

    /// Get the Content-Type header.
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get("content-type").map(|s| s.as_str())
    }

    /// Get the Content-Length header.
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get("content-length")
            .and_then(|s| s.parse().ok())
    }

    /// Get the filename from Content-Disposition header.
    pub fn content_disposition_filename(&self) -> Option<String> {
        self.headers
            .get("content-disposition")
            .and_then(|h| parse_content_disposition_filename(h))
    }

    /// Get response body as bytes.
    pub async fn bytes(self) -> Result<Vec<u8>, reqwest::Error> {
        self.response.bytes().await.map(|b| b.to_vec())
    }

    /// Get response body as text.
    pub async fn text(self) -> Result<String, reqwest::Error> {
        self.response.text().await
    }
}

/// HEAD response wrapper (no body, just headers).
pub struct HeadResponse {
    pub status: StatusCode,
    pub headers: HashMap<String, String>,
}

impl HeadResponse {
    /// Check if the response is 304 Not Modified.
    pub fn is_not_modified(&self) -> bool {
        self.status == StatusCode::NOT_MODIFIED
    }

    /// Check if the response is successful.
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// Get the ETag header.
    pub fn etag(&self) -> Option<&str> {
        self.headers.get("etag").map(|s| s.as_str())
    }

    /// Get the Last-Modified header.
    pub fn last_modified(&self) -> Option<&str> {
        self.headers.get("last-modified").map(|s| s.as_str())
    }

    /// Get the Content-Type header.
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get("content-type").map(|s| s.as_str())
    }

    /// Get the Content-Length header.
    pub fn content_length(&self) -> Option<u64> {
        self.headers
            .get("content-length")
            .and_then(|s| s.parse().ok())
    }

    /// Get the filename from Content-Disposition header.
    pub fn content_disposition_filename(&self) -> Option<String> {
        self.headers
            .get("content-disposition")
            .and_then(|h| parse_content_disposition_filename(h))
    }
}

/// Parse filename from Content-Disposition header value.
/// Parses both `filename="name.pdf"` and `filename*=UTF-8''name.pdf` formats.
pub fn parse_content_disposition_filename(header: &str) -> Option<String> {
    // Try filename*= first (RFC 5987 encoded)
    if let Some(start) = header.find("filename*=") {
        let rest = &header[start + 10..];
        if let Some(quote_start) = rest.find("''") {
            let encoded = rest[quote_start + 2..].split([';', ' ']).next()?;
            if let Ok(decoded) = urlencoding::decode(encoded) {
                let filename = decoded.trim().to_string();
                if !filename.is_empty() {
                    return Some(filename);
                }
            }
        }
    }

    // Try filename= (standard format)
    if let Some(start) = header.find("filename=") {
        let rest = &header[start + 9..];
        let filename = if let Some(quoted) = rest.strip_prefix('"') {
            quoted.split('"').next()
        } else {
            rest.split([';', ' ']).next()
        };

        if let Some(name) = filename {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_user_agent_default() {
        let ua = resolve_user_agent(None);
        assert!(ua.contains("FOIAcquire"));
    }

    #[test]
    fn test_resolve_user_agent_impersonate() {
        let ua = resolve_user_agent(Some("impersonate"));
        assert!(ua.contains("Mozilla"));
        assert!(!ua.contains("FOIAcquire"));
    }

    #[test]
    fn test_resolve_user_agent_custom() {
        let ua = resolve_user_agent(Some("MyBot/1.0"));
        assert_eq!(ua, "MyBot/1.0");
    }

    #[test]
    fn test_parse_content_disposition_quoted() {
        let header = r#"attachment; filename="document.pdf""#;
        assert_eq!(
            parse_content_disposition_filename(header),
            Some("document.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_unquoted() {
        let header = "attachment; filename=document.pdf";
        assert_eq!(
            parse_content_disposition_filename(header),
            Some("document.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_rfc5987() {
        let header = "attachment; filename*=UTF-8''my%20document.pdf";
        assert_eq!(
            parse_content_disposition_filename(header),
            Some("my document.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_both_formats() {
        // RFC 5987 should take precedence
        let header = r#"attachment; filename="fallback.pdf"; filename*=UTF-8''preferred.pdf"#;
        assert_eq!(
            parse_content_disposition_filename(header),
            Some("preferred.pdf".to_string())
        );
    }

    #[test]
    fn test_parse_content_disposition_none() {
        assert_eq!(parse_content_disposition_filename("attachment"), None);
        assert_eq!(parse_content_disposition_filename("inline"), None);
    }

    #[test]
    fn test_random_user_agent_varies() {
        // Check that random_user_agent returns valid user agents
        let ua = random_user_agent();
        assert!(ua.contains("Mozilla"));
    }
}
