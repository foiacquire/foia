//! Web server for browsing FOIA documents.
//!
//! Provides a directory-style listing of scraped documents with:
//! - Source-level grouping (each scraper is a "folder")
//! - Timeline visualization with date range filtering
//! - Cross-source deduplication display
//! - Document version history

mod assets;
mod cache;
mod handlers;
mod routes;
mod template_structs;

pub use routes::create_router;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use foia::config::Settings;
use foia::repository::{DieselCrawlRepository, DieselDocumentRepository, DieselSourceRepository};
use ipnet::IpNet;

use cache::StatsCache;

/// Status of a DeepSeek OCR job.
#[derive(Clone, Debug, Default)]
pub struct DeepSeekJobStatus {
    /// Document being processed (None if no job running).
    pub document_id: Option<String>,
    /// Number of pages processed so far.
    pub pages_processed: u32,
    /// Total pages to process.
    pub total_pages: u32,
    /// Error message if job failed.
    pub error: Option<String>,
    /// Whether the job is complete.
    pub completed: bool,
}

/// Shared state for the web server.
#[derive(Clone)]
pub struct AppState {
    pub doc_repo: Arc<DieselDocumentRepository>,
    pub source_repo: Arc<DieselSourceRepository>,
    pub crawl_repo: Arc<DieselCrawlRepository>,
    pub documents_dir: PathBuf,
    pub stats_cache: Arc<StatsCache>,
    /// DeepSeek OCR job status (only one can run at a time).
    pub deepseek_job: Arc<RwLock<DeepSeekJobStatus>>,
    /// IP allowlist for DeepSeek re-OCR endpoints. None = allow all.
    pub deepseek_allowed_ips: Option<Vec<IpNet>>,
}

impl AppState {
    pub async fn new(settings: &Settings) -> anyhow::Result<Self> {
        let ctx = settings.create_db_context()?;

        let deepseek_allowed_ips = parse_allowed_ips_from_env();

        Ok(Self {
            doc_repo: Arc::new(ctx.documents()),
            source_repo: Arc::new(ctx.sources()),
            crawl_repo: Arc::new(ctx.crawl()),
            documents_dir: settings.documents_dir.clone(),
            stats_cache: Arc::new(StatsCache::new()),
            deepseek_job: Arc::new(RwLock::new(DeepSeekJobStatus::default())),
            deepseek_allowed_ips,
        })
    }

    /// Check if the given IP is allowed to access DeepSeek endpoints.
    pub fn is_ip_allowed_for_deepseek(&self, ip: IpAddr) -> bool {
        match &self.deepseek_allowed_ips {
            None => true,
            Some(nets) => nets.iter().any(|net| net.contains(&ip)),
        }
    }
}

/// Parse DEEPSEEK_ALLOWED_IPS env var into a list of IP networks.
///
/// Format: comma-separated IPs or CIDRs, e.g. "192.168.1.0/24,10.0.0.5,::1"
/// Individual IPs are treated as /32 (IPv4) or /128 (IPv6).
/// Returns None if the env var is not set or empty.
fn parse_allowed_ips_from_env() -> Option<Vec<IpNet>> {
    let val = std::env::var("DEEPSEEK_ALLOWED_IPS").ok()?;
    let val = val.trim();
    if val.is_empty() {
        return None;
    }

    let nets: Vec<IpNet> = val
        .split(',')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            // Try parsing as CIDR first, then as bare IP
            s.parse::<IpNet>()
                .or_else(|_| {
                    s.parse::<IpAddr>()
                        .map(IpNet::from)
                })
                .map_err(|e| {
                    tracing::warn!("Invalid IP/CIDR in DEEPSEEK_ALLOWED_IPS: '{}': {}", s, e);
                    e
                })
                .ok()
        })
        .collect();

    if nets.is_empty() {
        tracing::warn!("DEEPSEEK_ALLOWED_IPS is set but contains no valid entries");
        return None;
    }

    tracing::info!(
        "DeepSeek IP allowlist: {} entries",
        nets.len()
    );
    Some(nets)
}

/// Start the web server.
pub async fn serve(settings: &Settings, host: &str, port: u16) -> anyhow::Result<()> {
    let state = AppState::new(settings).await?;
    let app = create_router(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    tracing::info!("Starting server at http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
