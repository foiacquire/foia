//! HTTP client and rate limiting for FOIA document acquisition.
//!
//! This crate provides:
//! - An HTTP client with privacy routing, rate limiting, and via proxy support
//! - Pluggable rate limiting with in-memory, database, and Redis backends
//! - Trait abstractions for crawl storage and browser automation

pub mod http_client;
pub mod rate_limit;

pub use http_client::{
    parse_content_disposition_filename, BrowserBackend, BrowserFetchResult, CrawlStore,
    HeadResponse, HttpClient, HttpClientBuilder, HttpResponse,
};
pub use http_client::user_agent::{resolve_user_agent, IMPERSONATE_USER_AGENTS, USER_AGENT};
