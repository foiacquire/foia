//! Database-backed configuration loader for prefer.
#![allow(dead_code)]
//!
//! This module provides a way to load `prefer::Config` from a database,
//! enabling config sync across devices while maintaining the prefer API.
//!
//! # Example
//!
//! ```no_run
//! use prefer_db::DbConfigLoader;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Load from file first, then overlay DB config
//!     let file_config = prefer::load("foiacquire").await?;
//!
//!     let loader = DbConfigLoader::new("/path/to/db.sqlite");
//!     let merged = loader.load_and_merge(file_config).await?;
//!
//!     // Use merged config
//!     let value: String = merged.get("scrapers.example.url").await?;
//!     Ok(())
//! }
//! ```

use std::path::{Path, PathBuf};

use prefer::Config as PreferConfig;
use serde_json::Value as JsonValue;

use crate::repository::diesel_context::DieselDbContext;

/// Database-backed configuration loader.
///
/// Loads configuration from the config_history table and returns
/// a `prefer::Config` instance that can be merged with file-based config.
pub struct DbConfigLoader {
    db_path: PathBuf,
}

impl DbConfigLoader {
    /// Create a new DB config loader for the given database path.
    pub fn new<P: AsRef<Path>>(db_path: P) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
        }
    }

    /// Load configuration from the database.
    ///
    /// Returns a `prefer::Config` containing the latest config from
    /// the config_history table, or None if no config is stored.
    pub async fn load(&self) -> Option<PreferConfig> {
        let ctx = DieselDbContext::from_sqlite_path(&self.db_path).ok()?;
        let entry = ctx.config_history().get_latest().await.ok()??;

        // Parse the stored config data
        let data: JsonValue = match entry.format.to_lowercase().as_str() {
            "json" => serde_json::from_str(&entry.data).ok()?,
            "toml" => {
                let toml_value: toml::Value = toml::from_str(&entry.data).ok()?;
                serde_json::to_value(toml_value).ok()?
            }
            _ => serde_json::from_str(&entry.data).ok()?,
        };

        Some(PreferConfig::new(data))
    }

    /// Load from DB and merge with an existing file-based config.
    ///
    /// DB values override file values (DB is considered more recent/authoritative
    /// for app-level settings that should sync across devices).
    pub async fn load_and_merge(&self, file_config: PreferConfig) -> PreferConfig {
        match self.load().await {
            Some(db_config) => merge_configs(file_config, db_config),
            None => file_config,
        }
    }

    /// Load from DB and merge with file config, with file taking precedence.
    ///
    /// File values override DB values (useful when file config is considered
    /// the source of truth and DB is just a fallback).
    pub async fn load_and_merge_file_priority(&self, file_config: PreferConfig) -> PreferConfig {
        match self.load().await {
            Some(db_config) => merge_configs(db_config, file_config),
            None => file_config,
        }
    }
}

/// Merge two configs, with `overlay` values taking precedence over `base`.
///
/// Performs a deep merge for objects, with overlay values replacing base values
/// at leaf nodes.
pub fn merge_configs(base: PreferConfig, overlay: PreferConfig) -> PreferConfig {
    let base_data = base.data().clone();
    let overlay_data = overlay.data().clone();

    let merged = deep_merge(base_data, overlay_data);
    PreferConfig::new(merged)
}

/// Deep merge two JSON values.
///
/// - Objects are merged recursively (overlay values take precedence)
/// - Arrays and other values from overlay completely replace base
/// - Null in overlay does NOT replace base (allows sparse overlays)
fn deep_merge(base: JsonValue, overlay: JsonValue) -> JsonValue {
    match (base, overlay) {
        // Both are objects: merge recursively
        (JsonValue::Object(mut base_map), JsonValue::Object(overlay_map)) => {
            for (key, overlay_value) in overlay_map {
                let merged_value = if let Some(base_value) = base_map.remove(&key) {
                    deep_merge(base_value, overlay_value)
                } else {
                    overlay_value
                };
                base_map.insert(key, merged_value);
            }
            JsonValue::Object(base_map)
        }
        // Overlay is null: keep base value (allows sparse overlays)
        (base, JsonValue::Null) => base,
        // Otherwise: overlay replaces base
        (_, overlay) => overlay,
    }
}

/// Save a `prefer::Config` to the database.
///
/// Converts the config to JSON and stores it in the config_history table
/// if it differs from the current stored config.
pub async fn save_to_db(
    config: &PreferConfig,
    db_path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};

    let ctx = DieselDbContext::from_sqlite_path(db_path)?;
    let repo = ctx.config_history();

    let data = serde_json::to_string_pretty(config.data())?;
    let format = "json";

    // Compute hash
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let hash = hex::encode(hasher.finalize());

    let saved = repo.insert_if_new(&data, format, &hash).await?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deep_merge_objects() {
        let base = json!({
            "a": 1,
            "b": {
                "c": 2,
                "d": 3
            }
        });

        let overlay = json!({
            "b": {
                "c": 20,
                "e": 5
            },
            "f": 6
        });

        let merged = deep_merge(base, overlay);

        assert_eq!(merged["a"], 1); // Kept from base
        assert_eq!(merged["b"]["c"], 20); // Overridden by overlay
        assert_eq!(merged["b"]["d"], 3); // Kept from base
        assert_eq!(merged["b"]["e"], 5); // Added from overlay
        assert_eq!(merged["f"], 6); // Added from overlay
    }

    #[test]
    fn test_deep_merge_null_preserves_base() {
        let base = json!({
            "a": 1,
            "b": 2
        });

        let overlay = json!({
            "a": null,
            "c": 3
        });

        let merged = deep_merge(base, overlay);

        assert_eq!(merged["a"], 1); // Null doesn't override
        assert_eq!(merged["b"], 2); // Kept from base
        assert_eq!(merged["c"], 3); // Added from overlay
    }

    #[test]
    fn test_merge_configs() {
        let base = PreferConfig::new(json!({
            "scrapers": {
                "example": {
                    "url": "https://example.com"
                }
            }
        }));

        let overlay = PreferConfig::new(json!({
            "scrapers": {
                "example": {
                    "enabled": true
                }
            },
            "llm": {
                "enabled": true
            }
        }));

        let merged = merge_configs(base, overlay);

        // Check merged values
        let scrapers = merged.data().get("scrapers").unwrap();
        let example = scrapers.get("example").unwrap();
        assert_eq!(example.get("url").unwrap(), "https://example.com");
        assert_eq!(example.get("enabled").unwrap(), true);

        let llm = merged.data().get("llm").unwrap();
        assert_eq!(llm.get("enabled").unwrap(), true);
    }
}
