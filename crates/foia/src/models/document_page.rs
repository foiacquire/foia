//! Document page models for per-page text extraction.

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// OCR processing status for a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOcrStatus {
    /// Page has not been processed yet.
    Pending,
    /// PDF text extraction complete, OCR not yet attempted.
    TextExtracted,
    /// OCR has been completed for this page.
    OcrComplete,
    /// Page was skipped (e.g., has sufficient text).
    Skipped,
    /// Processing failed for this page.
    Failed,
}

impl PageOcrStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::TextExtracted => "text_extracted",
            Self::OcrComplete => "ocr_complete",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "text_extracted" => Some(Self::TextExtracted),
            "ocr_complete" => Some(Self::OcrComplete),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A single page of a document with its extracted text.
///
/// Individual extraction results (pdftotext, groq, tesseract, etc.) are stored
/// in `page_ocr_results`. The `search_text` field is a materialized "best text"
/// column used for full-text search indexing — it holds the extraction result
/// with the highest character count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentPage {
    /// Database row ID.
    pub id: i64,
    /// Parent document ID.
    pub document_id: String,
    /// Document version ID this page belongs to.
    pub version_id: i64,
    /// Page number (1-indexed).
    pub page_number: u32,
    /// Materialized best text for search indexing.
    pub search_text: Option<String>,
    /// OCR processing status.
    pub ocr_status: PageOcrStatus,
    /// When this page record was created.
    pub created_at: DateTime<Utc>,
    /// When this page was last updated.
    pub updated_at: DateTime<Utc>,
}

impl DocumentPage {
    /// Create a new document page.
    pub fn new(document_id: String, version_id: i64, page_number: u32) -> Self {
        let now = Utc::now();
        Self {
            id: 0, // Set by database
            document_id,
            version_id,
            page_number,
            search_text: None,
            ocr_status: PageOcrStatus::Pending,
            created_at: now,
            updated_at: now,
        }
    }
}
