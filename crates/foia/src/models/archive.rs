#![allow(dead_code)]

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

use crate::schema::{archive_checks, archive_snapshots};
// Pure enums are re-exported at the models level from foia_models.
// Re-export here so `foia::models::archive::ArchiveService` also works.
pub use foia_models::archive::{ArchiveCheckResult, ArchiveService};

/// A snapshot captured by an archive service (Wayback Machine, archive.today, etc.)
#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = archive_snapshots)]
pub struct ArchiveSnapshot {
    pub id: i32,
    /// Archive service name (wayback, archive_today, common_crawl, etc.)
    pub service: String,
    /// Original URL that was archived
    pub original_url: String,
    /// URL to retrieve content from the archive
    pub archive_url: String,
    /// When the archive captured this snapshot
    pub captured_at: String,
    /// When we discovered this snapshot
    pub discovered_at: String,
    /// HTTP status from archive metadata
    pub http_status: Option<i32>,
    /// MIME type from archive metadata
    pub mimetype: Option<String>,
    /// Content length from archive metadata
    pub content_length: Option<i64>,
    /// Archive's content digest (e.g., Wayback SHA-1)
    pub digest: Option<String>,
    /// Service-specific metadata (JSON)
    pub metadata: String,
}

/// New archive snapshot for insertion.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = archive_snapshots)]
pub struct NewArchiveSnapshot {
    pub service: String,
    pub original_url: String,
    pub archive_url: String,
    pub captured_at: String,
    pub discovered_at: String,
    pub http_status: Option<i32>,
    pub mimetype: Option<String>,
    pub content_length: Option<i64>,
    pub digest: Option<String>,
    pub metadata: String,
}

impl NewArchiveSnapshot {
    pub fn new(
        service: impl Into<String>,
        original_url: impl Into<String>,
        archive_url: impl Into<String>,
        captured_at: DateTime<Utc>,
    ) -> Self {
        Self {
            service: service.into(),
            original_url: original_url.into(),
            archive_url: archive_url.into(),
            captured_at: captured_at.to_rfc3339(),
            discovered_at: Utc::now().to_rfc3339(),
            http_status: None,
            mimetype: None,
            content_length: None,
            digest: None,
            metadata: "{}".to_string(),
        }
    }

    pub fn with_http_status(mut self, status: i32) -> Self {
        self.http_status = Some(status);
        self
    }

    pub fn with_mimetype(mut self, mimetype: impl Into<String>) -> Self {
        self.mimetype = Some(mimetype.into());
        self
    }

    pub fn with_content_length(mut self, length: i64) -> Self {
        self.content_length = Some(length);
        self
    }

    pub fn with_digest(mut self, digest: impl Into<String>) -> Self {
        self.digest = Some(digest.into());
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata.to_string();
        self
    }
}

/// Record of checking an archive for historical versions of a document.
#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = archive_checks)]
pub struct ArchiveCheck {
    pub id: i32,
    /// Document version we were checking
    pub document_version_id: i32,
    /// Archive service that was queried
    pub archive_source: String,
    /// URL we searched for in the archive
    pub url_checked: String,
    /// When the check was performed
    pub checked_at: String,
    /// Number of snapshots found in archive
    pub snapshots_found: i32,
    /// Number of snapshots with matching content hash
    pub matching_snapshots: i32,
    /// Result: verified, new_versions, no_snapshots, error
    pub result: String,
    /// Error message if result is error
    pub error_message: Option<String>,
}

/// New archive check for insertion.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = archive_checks)]
pub struct NewArchiveCheck {
    pub document_version_id: i32,
    pub archive_source: String,
    pub url_checked: String,
    pub checked_at: String,
    pub snapshots_found: i32,
    pub matching_snapshots: i32,
    pub result: String,
    pub error_message: Option<String>,
}

impl NewArchiveCheck {
    pub fn new(
        document_version_id: i32,
        archive_source: impl Into<String>,
        url_checked: impl Into<String>,
        result: ArchiveCheckResult,
    ) -> Self {
        Self {
            document_version_id,
            archive_source: archive_source.into(),
            url_checked: url_checked.into(),
            checked_at: Utc::now().to_rfc3339(),
            snapshots_found: 0,
            matching_snapshots: 0,
            result: result.as_str().to_string(),
            error_message: None,
        }
    }

    pub fn with_counts(mut self, found: i32, matching: i32) -> Self {
        self.snapshots_found = found;
        self.matching_snapshots = matching;
        self
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error_message = Some(error.into());
        self
    }
}
