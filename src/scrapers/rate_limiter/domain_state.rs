//! Per-domain rate limiting state.

use std::time::{Duration, Instant};

use super::config::{RATE_LIMIT_403_THRESHOLD, RATE_LIMIT_WINDOW};

/// State for a single domain.
#[derive(Debug, Clone)]
pub struct DomainState {
    /// Current delay for this domain.
    pub current_delay: Duration,
    /// Last request time.
    pub last_request: Option<Instant>,
    /// Consecutive successes since last rate limit.
    pub consecutive_successes: u32,
    /// Recent 403 responses: (timestamp, url) for pattern detection.
    /// Only triggers rate limit if multiple unique URLs get 403 in a short window.
    pub recent_403s: Vec<(Instant, String)>,
    /// Whether currently in backoff.
    pub in_backoff: bool,
    /// Total requests made.
    pub total_requests: u64,
    /// Total rate limit hits.
    pub rate_limit_hits: u64,
}

impl DomainState {
    pub fn new(base_delay: Duration) -> Self {
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

    /// Add a 403 response, returns true if this triggers rate limit detection.
    pub fn add_403(&mut self, url: &str) -> bool {
        let now = Instant::now();
        let cutoff = now - RATE_LIMIT_WINDOW;

        // Binary search for cutoff point (list is chronologically sorted since we always append)
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
        self.unique_403_count() >= RATE_LIMIT_403_THRESHOLD
    }

    /// Count unique URLs that received 403 in the current window.
    pub fn unique_403_count(&self) -> usize {
        let mut unique_urls: Vec<&str> = self.recent_403s.iter().map(|(_, u)| u.as_str()).collect();
        unique_urls.sort();
        unique_urls.dedup();
        unique_urls.len()
    }

    /// Clear 403 tracking (on success or confirmed rate limit).
    pub fn clear_403_tracking(&mut self) {
        self.recent_403s.clear();
    }

    /// Get stats about recent 403s for debugging.
    /// Returns (unique_url_count, time_span_of_window).
    pub fn get_403_stats(&self) -> (usize, Duration) {
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

        (self.unique_403_count(), time_span)
    }

    /// Time until this domain is ready for another request.
    pub fn time_until_ready(&self) -> Duration {
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
    pub fn is_ready(&self) -> bool {
        self.time_until_ready() == Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_state_new() {
        let state = DomainState::new(Duration::from_millis(100));
        assert_eq!(state.current_delay, Duration::from_millis(100));
        assert!(state.last_request.is_none());
        assert_eq!(state.consecutive_successes, 0);
        assert!(!state.in_backoff);
        assert_eq!(state.total_requests, 0);
    }

    #[test]
    fn test_is_ready_no_previous_request() {
        let state = DomainState::new(Duration::from_millis(100));
        assert!(state.is_ready());
        assert_eq!(state.time_until_ready(), Duration::ZERO);
    }

    #[test]
    fn test_is_ready_after_delay() {
        let mut state = DomainState::new(Duration::from_millis(10));
        state.last_request = Some(Instant::now() - Duration::from_millis(20));
        assert!(state.is_ready());
    }

    #[test]
    fn test_not_ready_during_delay() {
        let mut state = DomainState::new(Duration::from_secs(10));
        state.last_request = Some(Instant::now());
        assert!(!state.is_ready());
        assert!(state.time_until_ready() > Duration::ZERO);
    }

    #[test]
    fn test_add_403_single() {
        let mut state = DomainState::new(Duration::from_millis(100));
        // Single 403 should not trigger rate limit (threshold is 3)
        let triggered = state.add_403("https://example.com/doc1.pdf");
        assert!(!triggered);
        assert_eq!(state.unique_403_count(), 1);
    }

    #[test]
    fn test_add_403_threshold() {
        let mut state = DomainState::new(Duration::from_millis(100));
        // Add 403s from different URLs to reach threshold
        state.add_403("https://example.com/doc1.pdf");
        state.add_403("https://example.com/doc2.pdf");
        let triggered = state.add_403("https://example.com/doc3.pdf");
        assert!(triggered); // Should trigger at threshold of 3
    }

    #[test]
    fn test_add_403_same_url() {
        let mut state = DomainState::new(Duration::from_millis(100));
        // Same URL multiple times should only count as 1 unique
        state.add_403("https://example.com/doc1.pdf");
        state.add_403("https://example.com/doc1.pdf");
        state.add_403("https://example.com/doc1.pdf");
        assert_eq!(state.unique_403_count(), 1);
    }

    #[test]
    fn test_clear_403_tracking() {
        let mut state = DomainState::new(Duration::from_millis(100));
        state.add_403("https://example.com/doc1.pdf");
        state.add_403("https://example.com/doc2.pdf");
        assert_eq!(state.unique_403_count(), 2);

        state.clear_403_tracking();
        assert_eq!(state.unique_403_count(), 0);
        assert!(state.recent_403s.is_empty());
    }

    #[test]
    fn test_get_403_stats_empty() {
        let state = DomainState::new(Duration::from_millis(100));
        let (count, span) = state.get_403_stats();
        assert_eq!(count, 0);
        assert_eq!(span, Duration::ZERO);
    }

    #[test]
    fn test_get_403_stats_with_entries() {
        let mut state = DomainState::new(Duration::from_millis(100));
        state.add_403("https://example.com/doc1.pdf");
        state.add_403("https://example.com/doc2.pdf");
        let (count, _span) = state.get_403_stats();
        assert_eq!(count, 2);
        // Span should be very small since we just added them
    }
}
