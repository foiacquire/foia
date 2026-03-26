#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Result of an archive check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveCheckResult {
    /// Content verified to exist at earlier date(s)
    Verified,
    /// Found versions with different content
    NewVersions,
    /// No snapshots found in archive
    NoSnapshots,
    /// Error during check
    Error,
}

impl ArchiveCheckResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::NewVersions => "new_versions",
            Self::NoSnapshots => "no_snapshots",
            Self::Error => "error",
        }
    }
}

impl std::fmt::Display for ArchiveCheckResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Archive service identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveService {
    Wayback,
    ArchiveToday,
    CommonCrawl,
    PermaCC,
}

impl ArchiveService {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wayback => "wayback",
            Self::ArchiveToday => "archive_today",
            Self::CommonCrawl => "common_crawl",
            Self::PermaCC => "perma_cc",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Wayback => "Wayback Machine",
            Self::ArchiveToday => "archive.today",
            Self::CommonCrawl => "Common Crawl",
            Self::PermaCC => "Perma.cc",
        }
    }
}

impl std::fmt::Display for ArchiveService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ArchiveService {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace(['-', '.'], "_").as_str() {
            "wayback" | "wayback_machine" | "archive_org" => Ok(Self::Wayback),
            "archive_today" | "archive_is" | "archive_ph" => Ok(Self::ArchiveToday),
            "common_crawl" | "commoncrawl" => Ok(Self::CommonCrawl),
            "perma_cc" | "permacc" | "perma" => Ok(Self::PermaCC),
            _ => Err(format!("Unknown archive service: {}", s)),
        }
    }
}
