//! Scraper configuration re-exports and browser-specific extensions.
//!
//! Type definitions live in `crate::config::scraper`. This module
//! re-exports them for backwards compatibility and adds the
//! `BrowserConfig::to_engine_config()` method which depends on
//! browser engine types from the scrapers crate.

pub use crate::config::scraper::*;

use super::browser::{BrowserEngineConfig, BrowserEngineType, SelectionStrategyType};

impl BrowserConfig {
    /// Convert to BrowserEngineConfig.
    /// Per-scraper config overrides environment variables.
    pub fn to_engine_config(&self) -> BrowserEngineConfig {
        let engine = match self.engine.to_lowercase().as_str() {
            "stealth" => BrowserEngineType::Stealth,
            "cookies" => BrowserEngineType::Cookies,
            "standard" => BrowserEngineType::Standard,
            _ => BrowserEngineType::Stealth,
        };

        let selection = self
            .selection
            .as_ref()
            .and_then(|s| SelectionStrategyType::from_str(s))
            .unwrap_or_default();

        // Per-scraper URLs override environment
        let (remote_url, remote_urls) = if !self.urls.is_empty() {
            (None, self.urls.clone())
        } else if let Some(ref url) = self.remote_url {
            (Some(url.clone()), Vec::new())
        } else {
            // Fall back to environment variables
            let base = BrowserEngineConfig::default().with_env_overrides();
            (base.remote_url, base.remote_urls)
        };

        BrowserEngineConfig {
            engine,
            headless: self.headless,
            proxy: self.proxy.clone(),
            cookies_file: self.cookies_file.as_ref().map(std::path::PathBuf::from),
            timeout: self.timeout,
            wait_for_selector: self.wait_for_selector.clone(),
            chrome_args: Vec::new(),
            remote_url,
            remote_urls,
            selection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_config_to_engine_config() {
        let config = BrowserConfig {
            enabled: true,
            engine: "stealth".to_string(),
            headless: false,
            proxy: Some("socks5://127.0.0.1:1080".to_string()),
            timeout: 60,
            wait_for_selector: Some("#content".to_string()),
            ..Default::default()
        };

        let engine_config = config.to_engine_config();
        assert!(matches!(engine_config.engine, BrowserEngineType::Stealth));
        assert!(!engine_config.headless);
        assert_eq!(
            engine_config.proxy,
            Some("socks5://127.0.0.1:1080".to_string())
        );
        assert_eq!(engine_config.timeout, 60);
    }

    #[test]
    fn test_browser_engine_type_parsing() {
        let stealth = BrowserConfig {
            engine: "stealth".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            stealth.to_engine_config().engine,
            BrowserEngineType::Stealth
        ));

        let cookies = BrowserConfig {
            engine: "cookies".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            cookies.to_engine_config().engine,
            BrowserEngineType::Cookies
        ));

        let standard = BrowserConfig {
            engine: "standard".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            standard.to_engine_config().engine,
            BrowserEngineType::Standard
        ));

        // Unknown defaults to Stealth
        let unknown = BrowserConfig {
            engine: "unknown".to_string(),
            ..Default::default()
        };
        assert!(matches!(
            unknown.to_engine_config().engine,
            BrowserEngineType::Stealth
        ));
    }
}
