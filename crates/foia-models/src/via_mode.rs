//! Via proxy mode for URL rewriting through caching proxies.

use serde::{Deserialize, Serialize};

/// Via proxy mode - controls how URL rewriting through caching proxies works.
///
/// Via mappings rewrite URLs to fetch through CDN/caching proxies (e.g., Cloudflare).
/// This setting controls when those proxies are used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViaMode {
    /// Never send requests over via proxy. Via mappings are only used for
    /// URL normalization/detection (e.g., recognizing Google Drive URLs).
    #[default]
    Strict,
    /// Use via proxy as fallback when rate limited (429/503).
    /// Primary requests go to the original URL.
    Fallback,
    /// Use via proxy as primary, fall back to original URL on failure.
    Priority,
}

impl prefer::FromValue for ViaMode {
    fn from_value(value: &prefer::ConfigValue) -> prefer::Result<Self> {
        match value.as_str() {
            Some("strict") => Ok(ViaMode::Strict),
            Some("fallback") => Ok(ViaMode::Fallback),
            Some("priority") => Ok(ViaMode::Priority),
            Some(other) => Err(prefer::Error::ConversionError {
                key: String::new(),
                type_name: "ViaMode".to_string(),
                source: format!("unknown via mode: {}", other).into(),
            }),
            None => Err(prefer::Error::ConversionError {
                key: String::new(),
                type_name: "ViaMode".to_string(),
                source: "expected string".into(),
            }),
        }
    }
}

#[allow(dead_code)]
impl ViaMode {
    /// Check if this mode allows using via for requests (not just detection).
    pub fn allows_via_requests(&self) -> bool {
        !matches!(self, ViaMode::Strict)
    }

    /// Check if via should be tried first (priority mode).
    pub fn via_first(&self) -> bool {
        matches!(self, ViaMode::Priority)
    }
}
