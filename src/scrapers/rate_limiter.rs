//! Adaptive per-domain rate limiter.
//!
//! Tracks request timing per domain and adapts delays based on responses.
//! Backs off on 429/503, gradually recovers on success.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use url::Url;

/// Configuration for rate limiting behavior.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Base delay between requests to the same domain.
    pub base_delay: Duration,
    /// Minimum delay (floor).
    pub min_delay: Duration,
    /// Maximum delay (ceiling for backoff).
    pub max_delay: Duration,
    /// Multiplier for exponential backoff on rate limit.
    pub backoff_multiplier: f64,
    /// Multiplier for recovery on success (< 1.0 to decrease delay).
    pub recovery_multiplier: f64,
    /// Number of consecutive successes before reducing delay.
    pub recovery_threshold: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_millis(500),
            min_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            recovery_multiplier: 0.8,
            recovery_threshold: 5,
        }
    }
}

/// Window for detecting 403 rate limit patterns.
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(30);
/// Threshold of unique 403s in window to trigger rate limit detection.
const RATE_LIMIT_403_THRESHOLD: usize = 3;

/// State for a single domain.
#[derive(Debug, Clone)]
struct DomainState {
    /// Current delay for this domain.
    current_delay: Duration,
    /// Last request time.
    last_request: Option<Instant>,
    /// Consecutive successes since last rate limit.
    consecutive_successes: u32,
    /// Recent 403 responses: (timestamp, url) for pattern detection.
    /// Only triggers rate limit if multiple unique URLs get 403 in a short window.
    recent_403s: Vec<(Instant, String)>,
    /// Whether currently in backoff.
    in_backoff: bool,
    /// Total requests made.
    total_requests: u64,
    /// Total rate limit hits.
    rate_limit_hits: u64,
}

impl DomainState {
    fn new(base_delay: Duration) -> Self {
        Self {
            current_delay: base_delay,
            last_request: None,
            consecutive_successes: 0,
            recent_403s: Vec::new(),
            in_backoff: false,
            total_requests: 0,
            rate_limit_hits: 0,
        }
    }

    /// Add a 403 response, returns count of unique URLs that got 403 within the time window.
    fn add_403(&mut self, url: &str) -> usize {
        let now = Instant::now();
        let cutoff = now - RATE_LIMIT_WINDOW;

        // Binary search for cutoff point (list is chronologically sorted since we always append)
        // Find first entry that is within the window
        let cutoff_idx = self
            .recent_403s
            .binary_search_by(|(time, _)| {
                if *time < cutoff {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            })
            .unwrap_or_else(|i| i);

        // Remove old entries by slicing
        if cutoff_idx > 0 {
            self.recent_403s.drain(0..cutoff_idx);
        }

        // Add new entry (even if URL already exists - we want to track timing)
        self.recent_403s.push((now, url.to_string()));

        // Count unique URLs in the window
        let mut unique_urls: Vec<&str> = self.recent_403s.iter().map(|(_, u)| u.as_str()).collect();
        unique_urls.sort();
        unique_urls.dedup();
        unique_urls.len()
    }

    /// Clear 403 tracking (on success or confirmed rate limit).
    fn clear_403_tracking(&mut self) {
        self.recent_403s.clear();
    }

    /// Get stats about recent 403s for debugging.
    /// Returns (unique_url_count, time_span_of_window).
    fn get_403_stats(&self) -> (usize, Duration) {
        if self.recent_403s.is_empty() {
            return (0, Duration::ZERO);
        }

        // Since list is chronologically sorted, first entry is oldest, last is newest
        let oldest_time = self.recent_403s.first().map(|(t, _)| *t);
        let newest_time = self.recent_403s.last().map(|(t, _)| *t);

        let time_span = match (oldest_time, newest_time) {
            (Some(oldest), Some(newest)) => newest.duration_since(oldest),
            _ => Duration::ZERO,
        };

        let mut unique_urls: Vec<&str> = self.recent_403s.iter().map(|(_, u)| u.as_str()).collect();
        unique_urls.sort();
        unique_urls.dedup();

        (unique_urls.len(), time_span)
    }

    /// Time until this domain is ready for another request.
    fn time_until_ready(&self) -> Duration {
        match self.last_request {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed >= self.current_delay {
                    Duration::ZERO
                } else {
                    self.current_delay - elapsed
                }
            }
            None => Duration::ZERO,
        }
    }

    /// Check if this domain is ready for a request now.
    fn is_ready(&self) -> bool {
        self.time_until_ready() == Duration::ZERO
    }
}

/// Adaptive rate limiter that tracks per-domain request timing.
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    domains: Arc<RwLock<HashMap<String, DomainState>>>,
}

impl RateLimiter {
    /// Create a new rate limiter with default config.
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Create a new rate limiter with custom config.
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            config,
            domains: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Extract domain from URL.
    pub fn extract_domain(url: &str) -> Option<String> {
        Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
    }

    /// Wait until the domain is ready, then mark request as started.
    pub async fn acquire(&self, url: &str) -> Option<String> {
        let domain = Self::extract_domain(url)?;

        // Get or create domain state
        let wait_time = {
            let domains = self.domains.read().await;
            domains
                .get(&domain)
                .map(|s| s.time_until_ready())
                .unwrap_or(Duration::ZERO)
        };

        // Wait if needed
        if wait_time > Duration::ZERO {
            debug!("Rate limiting {}: waiting {:?}", domain, wait_time);
            tokio::time::sleep(wait_time).await;
        }

        // Mark request as started
        {
            let mut domains = self.domains.write().await;
            let state = domains
                .entry(domain.clone())
                .or_insert_with(|| DomainState::new(self.config.base_delay));
            state.last_request = Some(Instant::now());
            state.total_requests += 1;
        }

        Some(domain)
    }

    /// Report a successful request - may decrease delay.
    pub async fn report_success(&self, domain: &str) {
        let mut domains = self.domains.write().await;
        if let Some(state) = domains.get_mut(domain) {
            state.consecutive_successes += 1;
            state.clear_403_tracking(); // Reset 403 tracking on success

            // Recover from backoff after threshold successes
            if state.in_backoff && state.consecutive_successes >= self.config.recovery_threshold {
                let new_delay = Duration::from_secs_f64(
                    state.current_delay.as_secs_f64() * self.config.recovery_multiplier,
                );
                state.current_delay = new_delay.max(self.config.min_delay);

                if state.current_delay <= self.config.base_delay {
                    state.in_backoff = false;
                    state.current_delay = self.config.base_delay;
                    info!("Domain {} recovered from rate limit backoff", domain);
                } else {
                    debug!(
                        "Domain {} delay reduced to {:?}",
                        domain, state.current_delay
                    );
                }

                state.consecutive_successes = 0;
            }
        }
    }

    /// Check if a status code is definitely a rate limit (not ambiguous).
    pub fn is_definite_rate_limit(status_code: u16) -> bool {
        matches!(status_code, 429 | 503)
    }

    /// Check if a status code might be a rate limit (needs pattern detection).
    pub fn is_possible_rate_limit(status_code: u16) -> bool {
        matches!(status_code, 429 | 503 | 403)
    }

    /// Report a 403 response - only backs off if we see a pattern on different URLs.
    /// Returns true if this was detected as rate limiting.
    pub async fn report_403(&self, domain: &str, url: &str, has_retry_after: bool) -> bool {
        let mut domains = self.domains.write().await;
        if let Some(state) = domains.get_mut(domain) {
            let unique_403_count = state.add_403(url);
            state.consecutive_successes = 0;

            // Retry-After header = definitely rate limiting
            // N+ unique URLs getting 403 within time window = probably rate limiting
            let is_rate_limit = has_retry_after || unique_403_count >= RATE_LIMIT_403_THRESHOLD;

            if is_rate_limit {
                let (count, window) = state.get_403_stats();
                state.rate_limit_hits += 1;
                state.in_backoff = true;
                state.clear_403_tracking(); // Clear after confirming rate limit

                let new_delay = Duration::from_secs_f64(
                    state.current_delay.as_secs_f64() * self.config.backoff_multiplier,
                );
                state.current_delay = new_delay.min(self.config.max_delay);

                warn!(
                    "Rate limited by {} ({} unique URLs got 403 in {:?}), backing off to {:?}",
                    domain, count, window, state.current_delay
                );
                return true;
            } else {
                debug!(
                    "403 from {} for {} ({} unique URLs in window) - treating as access denied",
                    domain, url, unique_403_count
                );
            }
        }
        false
    }

    /// Report a definite rate limit hit (429 or 503) - increases delay.
    pub async fn report_rate_limit(&self, domain: &str, status_code: u16) {
        let mut domains = self.domains.write().await;
        if let Some(state) = domains.get_mut(domain) {
            state.rate_limit_hits += 1;
            state.consecutive_successes = 0;
            state.clear_403_tracking(); // Reset 403 tracking
            state.in_backoff = true;

            let new_delay = Duration::from_secs_f64(
                state.current_delay.as_secs_f64() * self.config.backoff_multiplier,
            );
            state.current_delay = new_delay.min(self.config.max_delay);

            warn!(
                "Rate limited by {} (HTTP {}), backing off to {:?}",
                domain, status_code, state.current_delay
            );
        }
    }

    /// Report a client error (4xx other than 429) - no delay change.
    pub async fn report_client_error(&self, domain: &str) {
        // Client errors don't affect rate limiting
        let domains = self.domains.read().await;
        if let Some(state) = domains.get(domain) {
            debug!(
                "Client error for {}, delay unchanged at {:?}",
                domain, state.current_delay
            );
        }
    }

    /// Report a server error (5xx other than 503) - mild backoff.
    pub async fn report_server_error(&self, domain: &str) {
        let mut domains = self.domains.write().await;
        if let Some(state) = domains.get_mut(domain) {
            // Mild backoff for server errors (might be overloaded)
            let new_delay = Duration::from_secs_f64(state.current_delay.as_secs_f64() * 1.5);
            state.current_delay = new_delay.min(self.config.max_delay);
            debug!(
                "Server error for {}, delay increased to {:?}",
                domain, state.current_delay
            );
        }
    }

    /// Check if a domain is currently ready for requests.
    pub async fn is_domain_ready(&self, url: &str) -> bool {
        let domain = match Self::extract_domain(url) {
            Some(d) => d,
            None => return true,
        };

        let domains = self.domains.read().await;
        domains.get(&domain).map(|s| s.is_ready()).unwrap_or(true)
    }

    /// Get time until domain is ready.
    pub async fn time_until_ready(&self, url: &str) -> Duration {
        let domain = match Self::extract_domain(url) {
            Some(d) => d,
            None => return Duration::ZERO,
        };

        let domains = self.domains.read().await;
        domains
            .get(&domain)
            .map(|s| s.time_until_ready())
            .unwrap_or(Duration::ZERO)
    }

    /// Get statistics for all domains.
    pub async fn get_stats(&self) -> HashMap<String, DomainStats> {
        let domains = self.domains.read().await;
        domains
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    DomainStats {
                        current_delay: v.current_delay,
                        in_backoff: v.in_backoff,
                        total_requests: v.total_requests,
                        rate_limit_hits: v.rate_limit_hits,
                    },
                )
            })
            .collect()
    }

    /// Find the domain that's ready soonest from a list of URLs.
    pub async fn find_ready_url<'a>(&self, urls: &'a [String]) -> Option<&'a String> {
        let domains = self.domains.read().await;

        let mut best_url: Option<&String> = None;
        let mut best_wait = Duration::MAX;

        for url in urls {
            let domain = match Self::extract_domain(url) {
                Some(d) => d,
                None => continue,
            };

            let wait = domains
                .get(&domain)
                .map(|s| s.time_until_ready())
                .unwrap_or(Duration::ZERO);

            if wait < best_wait {
                best_wait = wait;
                best_url = Some(url);

                // If we found one that's ready now, use it
                if wait == Duration::ZERO {
                    break;
                }
            }
        }

        best_url
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for RateLimiter {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            domains: self.domains.clone(),
        }
    }
}

/// Statistics for a domain.
#[derive(Debug, Clone)]
pub struct DomainStats {
    pub current_delay: Duration,
    pub in_backoff: bool,
    pub total_requests: u64,
    pub rate_limit_hits: u64,
}

// ============================================================================
// Database Persistence
// ============================================================================

/// Open a database connection with proper concurrency settings.
fn open_db(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 30000;
    "#,
    )?;
    Ok(conn)
}

/// Initialize the rate limit table in the database.
pub fn init_rate_limit_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS rate_limit_state (
            domain TEXT PRIMARY KEY,
            current_delay_ms INTEGER NOT NULL,
            in_backoff INTEGER NOT NULL DEFAULT 0,
            total_requests INTEGER NOT NULL DEFAULT 0,
            rate_limit_hits INTEGER NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
    "#,
    )?;
    Ok(())
}

/// Load rate limit state from database into a RateLimiter.
pub async fn load_rate_limit_state(limiter: &RateLimiter, db_path: &Path) -> anyhow::Result<usize> {
    let conn = open_db(db_path)?;
    init_rate_limit_table(&conn)?;

    let mut stmt = conn.prepare(
        "SELECT domain, current_delay_ms, in_backoff, total_requests, rate_limit_hits FROM rate_limit_state"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)? as u64,
            row.get::<_, i32>(2)? != 0,
            row.get::<_, i64>(3)? as u64,
            row.get::<_, i64>(4)? as u64,
        ))
    })?;

    let mut domains = limiter.domains.write().await;
    let mut count = 0;

    for row in rows {
        let (domain, delay_ms, in_backoff, total_requests, rate_limit_hits) = row?;

        // Only load domains that are still in backoff (have meaningful state)
        if in_backoff || delay_ms > limiter.config.base_delay.as_millis() as u64 {
            let state = DomainState {
                current_delay: Duration::from_millis(delay_ms),
                last_request: None, // Can't restore Instant from DB
                consecutive_successes: 0,
                recent_403s: Vec::new(),
                in_backoff,
                total_requests,
                rate_limit_hits,
            };
            info!(
                "Restored rate limit state for {}: delay={}ms, in_backoff={}",
                domain, delay_ms, in_backoff
            );
            domains.insert(domain, state);
            count += 1;
        }
    }

    if count > 0 {
        info!(
            "Loaded rate limit state for {} domains from database",
            count
        );
    }

    Ok(count)
}

/// Save rate limit state to database.
pub async fn save_rate_limit_state(limiter: &RateLimiter, db_path: &Path) -> anyhow::Result<usize> {
    let conn = open_db(db_path)?;
    init_rate_limit_table(&conn)?;

    let domains = limiter.domains.read().await;
    let mut count = 0;

    for (domain, state) in domains.iter() {
        // Only save domains with non-default state
        if state.in_backoff || state.current_delay > limiter.config.base_delay {
            conn.execute(
                r#"INSERT OR REPLACE INTO rate_limit_state
                   (domain, current_delay_ms, in_backoff, total_requests, rate_limit_hits, updated_at)
                   VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)"#,
                params![
                    domain,
                    state.current_delay.as_millis() as i64,
                    state.in_backoff as i32,
                    state.total_requests as i64,
                    state.rate_limit_hits as i64,
                ],
            )?;
            count += 1;
        }
    }

    // Clean up old entries that are no longer in backoff
    conn.execute(
        "DELETE FROM rate_limit_state WHERE in_backoff = 0 AND current_delay_ms <= ?",
        params![limiter.config.base_delay.as_millis() as i64],
    )?;

    if count > 0 {
        debug!("Saved rate limit state for {} domains to database", count);
    }

    Ok(count)
}

/// Save state for a single domain (call after rate limit events).
pub async fn save_domain_state(
    limiter: &RateLimiter,
    domain: &str,
    db_path: &Path,
) -> anyhow::Result<()> {
    let domains = limiter.domains.read().await;

    if let Some(state) = domains.get(domain) {
        if state.in_backoff || state.current_delay > limiter.config.base_delay {
            let conn = open_db(db_path)?;
            init_rate_limit_table(&conn)?;

            conn.execute(
                r#"INSERT OR REPLACE INTO rate_limit_state
                   (domain, current_delay_ms, in_backoff, total_requests, rate_limit_hits, updated_at)
                   VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)"#,
                params![
                    domain,
                    state.current_delay.as_millis() as i64,
                    state.in_backoff as i32,
                    state.total_requests as i64,
                    state.rate_limit_hits as i64,
                ],
            )?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_extract_domain() {
        assert_eq!(
            RateLimiter::extract_domain("https://example.com/path"),
            Some("example.com".to_string())
        );
        assert_eq!(
            RateLimiter::extract_domain("https://cdn.muckrock.com/file.pdf"),
            Some("cdn.muckrock.com".to_string())
        );
    }

    #[tokio::test]
    async fn test_backoff_on_rate_limit() {
        let limiter = RateLimiter::with_config(RateLimitConfig {
            base_delay: Duration::from_millis(100),
            backoff_multiplier: 2.0,
            ..Default::default()
        });

        // First request
        limiter.acquire("https://example.com/1").await;

        // Report rate limit
        limiter.report_rate_limit("example.com", 429).await;

        // Check delay increased
        let stats = limiter.get_stats().await;
        let domain_stats = stats.get("example.com").unwrap();
        assert!(domain_stats.current_delay >= Duration::from_millis(200));
        assert!(domain_stats.in_backoff);
    }
}
