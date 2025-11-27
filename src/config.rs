//! Configuration management for FOIAcquire using the prefer crate.

use std::collections::HashMap;

/// Default refresh TTL in days (14 days).
pub const DEFAULT_REFRESH_TTL_DAYS: u64 = 14;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::llm::LlmConfig;
use crate::scrapers::ScraperConfig;

/// Application settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Base data directory.
    pub data_dir: PathBuf,
    /// Database filename.
    pub database_filename: String,
    /// Directory for storing documents.
    pub documents_dir: PathBuf,
    /// User agent for HTTP requests.
    pub user_agent: String,
    /// Request timeout in seconds.
    pub request_timeout: u64,
    /// Delay between requests in milliseconds.
    pub request_delay_ms: u64,
    /// Rate limit backend URL (None = in-memory, "sqlite" = local DB, "redis://..." = Redis).
    pub rate_limit_backend: Option<String>,
    /// Worker queue broker URL (None = local DB, "amqp://..." = RabbitMQ).
    pub broker_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        // Default to ~/Documents/foia/ for user data
        let data_dir = dirs::document_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("foia");

        Self {
            documents_dir: data_dir.join("documents"),
            data_dir,
            database_filename: "foiacquire.db".to_string(),
            user_agent: "FOIAcquire/0.1 (academic research)".to_string(),
            request_timeout: 30,
            request_delay_ms: 500,
            rate_limit_backend: None, // In-memory by default
            broker_url: None,         // Local DB by default
        }
    }
}

impl Settings {
    /// Create settings with a custom data directory.
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            documents_dir: data_dir.join("documents"),
            data_dir,
            ..Default::default()
        }
    }

    /// Get the full path to the database.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(&self.database_filename)
    }

    /// Ensure all directories exist.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.documents_dir)?;
        Ok(())
    }
}

/// Configuration file structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Target directory for data.
    #[serde(default)]
    pub target: Option<String>,
    /// Database filename.
    #[serde(default)]
    pub database: Option<String>,
    /// User agent string.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Request timeout in seconds.
    #[serde(default)]
    pub request_timeout: Option<u64>,
    /// Delay between requests in milliseconds.
    #[serde(default)]
    pub request_delay_ms: Option<u64>,
    /// Rate limit backend URL.
    /// - None or "memory": In-memory (single process only)
    /// - "sqlite": Use local SQLite database (multi-process safe)
    /// - "redis://host:port": Use Redis (distributed)
    #[serde(default)]
    pub rate_limit_backend: Option<String>,
    /// Worker queue broker URL.
    /// - None or "database": Use local SQLite database
    /// - "amqp://host:port": Use RabbitMQ
    #[serde(default)]
    pub broker_url: Option<String>,
    /// Default refresh TTL in days for re-checking fetched URLs.
    /// Individual scrapers can override this with their own refresh_ttl_days.
    /// Defaults to 14 days if not set.
    #[serde(default)]
    pub default_refresh_ttl_days: Option<u64>,
    /// Scraper configurations.
    #[serde(default)]
    pub scrapers: HashMap<String, ScraperConfig>,
    /// LLM configuration for document summarization.
    #[serde(default)]
    pub llm: LlmConfig,
}

impl Config {
    /// Load configuration using prefer crate.
    /// Automatically discovers foiacquire config files in standard locations.
    pub async fn load() -> Self {
        match prefer::load("foiacquire").await {
            Ok(pref_config) => {
                // Extract values from prefer config using dot notation
                let target: Option<String> = pref_config.get("target").await.ok();
                let database: Option<String> = pref_config.get("database").await.ok();
                let user_agent: Option<String> = pref_config.get("user_agent").await.ok();
                let request_timeout: Option<u64> = pref_config.get("request_timeout").await.ok();
                let request_delay_ms: Option<u64> = pref_config.get("request_delay_ms").await.ok();
                let rate_limit_backend: Option<String> =
                    pref_config.get("rate_limit_backend").await.ok();
                let broker_url: Option<String> = pref_config.get("broker_url").await.ok();
                let default_refresh_ttl_days: Option<u64> =
                    pref_config.get("default_refresh_ttl_days").await.ok();
                let scrapers: HashMap<String, ScraperConfig> =
                    pref_config.get("scrapers").await.unwrap_or_default();
                let llm: LlmConfig = pref_config.get("llm").await.unwrap_or_default();

                Config {
                    target,
                    database,
                    user_agent,
                    request_timeout,
                    request_delay_ms,
                    rate_limit_backend,
                    broker_url,
                    default_refresh_ttl_days,
                    scrapers,
                    llm,
                }
            }
            Err(_) => {
                // No config file found, use defaults
                Self::default()
            }
        }
    }

    /// Apply configuration to settings.
    pub fn apply_to_settings(&self, settings: &mut Settings) {
        if let Some(ref target) = self.target {
            let path = shellexpand::tilde(target);
            settings.data_dir = PathBuf::from(path.as_ref());
            settings.documents_dir = settings.data_dir.join("documents");
        }
        if let Some(ref database) = self.database {
            settings.database_filename = database.clone();
        }
        if let Some(ref user_agent) = self.user_agent {
            settings.user_agent = user_agent.clone();
        }
        if let Some(timeout) = self.request_timeout {
            settings.request_timeout = timeout;
        }
        if let Some(delay) = self.request_delay_ms {
            settings.request_delay_ms = delay;
        }
        if let Some(ref backend) = self.rate_limit_backend {
            settings.rate_limit_backend = Some(backend.clone());
        }
        if let Some(ref broker) = self.broker_url {
            settings.broker_url = Some(broker.clone());
        }
    }

    /// Get the effective refresh TTL in days for a scraper.
    /// Priority: scraper config > global config > default constant.
    pub fn get_refresh_ttl_days(&self, source_id: &str) -> u64 {
        // First check scraper-specific config
        if let Some(scraper_config) = self.scrapers.get(source_id) {
            if let Some(ttl) = scraper_config.refresh_ttl_days {
                return ttl;
            }
        }
        // Fall back to global config or default
        self.default_refresh_ttl_days
            .unwrap_or(DEFAULT_REFRESH_TTL_DAYS)
    }
}

/// Load settings from configuration (async version).
pub async fn load_settings() -> Settings {
    let config = Config::load().await;
    let mut settings = Settings::default();
    config.apply_to_settings(&mut settings);
    settings
}
