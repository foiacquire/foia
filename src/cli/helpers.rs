//! Shared helper functions for CLI commands.

use std::path::Path;

use crate::models::{Document, DocumentVersion};
use crate::repository::{
    extract_filename_parts, sanitize_filename, AsyncDocumentRepository, DocumentRepository,
};
use crate::scrapers::ScraperResult;

/// Save scraped document content to disk and database.
///
/// This handles:
/// - Computing content hash
/// - Creating file path with hash subdirectory
/// - Writing file to disk
/// - Creating or updating document in database
///
/// Returns `true` if a new document was created, `false` if an existing one was updated.
pub fn save_scraped_document(
    doc_repo: &DocumentRepository,
    content: &[u8],
    result: &ScraperResult,
    source_id: &str,
    documents_dir: &Path,
) -> anyhow::Result<bool> {
    // Compute content hash and save file with readable name
    let content_hash = DocumentVersion::compute_hash(content);

    // Extract basename and extension from URL or title
    let (basename, extension) =
        extract_filename_parts(&result.url, &result.title, &result.mime_type);
    let filename = format!(
        "{}-{}.{}",
        sanitize_filename(&basename),
        &content_hash[..8],
        extension
    );

    // Store in subdirectory by first 2 chars of hash (for filesystem efficiency)
    let content_path = documents_dir.join(&content_hash[..2]).join(&filename);
    std::fs::create_dir_all(content_path.parent().unwrap())?;
    std::fs::write(&content_path, content)?;

    let version = DocumentVersion::new_with_metadata(
        content,
        content_path,
        result.mime_type.clone(),
        Some(result.url.clone()),
        result.original_filename.clone(),
        result.server_date,
    );

    // Check existing document
    let existing = doc_repo.get_by_url(&result.url)?;

    if let Some(mut doc) = existing {
        if doc.add_version(version) {
            doc_repo.save(&doc)?;
        }
        Ok(false) // Updated existing
    } else {
        let doc = Document::new(
            uuid::Uuid::new_v4().to_string(),
            source_id.to_string(),
            result.title.clone(),
            result.url.clone(),
            version,
            result.metadata.clone(),
        );
        doc_repo.save(&doc)?;
        Ok(true) // Created new
    }
}

/// Async version of save_scraped_document for use with AsyncDocumentRepository.
pub async fn save_scraped_document_async(
    doc_repo: &AsyncDocumentRepository,
    content: &[u8],
    result: &ScraperResult,
    source_id: &str,
    documents_dir: &Path,
) -> anyhow::Result<bool> {
    // Compute content hash and save file with readable name
    let content_hash = DocumentVersion::compute_hash(content);

    // Extract basename and extension from URL or title
    let (basename, extension) =
        extract_filename_parts(&result.url, &result.title, &result.mime_type);
    let filename = format!(
        "{}-{}.{}",
        sanitize_filename(&basename),
        &content_hash[..8],
        extension
    );

    // Store in subdirectory by first 2 chars of hash (for filesystem efficiency)
    let content_path = documents_dir.join(&content_hash[..2]).join(&filename);
    std::fs::create_dir_all(content_path.parent().unwrap())?;
    std::fs::write(&content_path, content)?;

    let version = DocumentVersion::new_with_metadata(
        content,
        content_path,
        result.mime_type.clone(),
        Some(result.url.clone()),
        result.original_filename.clone(),
        result.server_date,
    );

    // Check existing document
    let existing = doc_repo.get_by_url(&result.url).await?;

    if let Some(mut doc) = existing {
        if doc.add_version(version) {
            doc_repo.save(&doc).await?;
        }
        Ok(false) // Updated existing
    } else {
        let doc = Document::new(
            uuid::Uuid::new_v4().to_string(),
            source_id.to_string(),
            result.title.clone(),
            result.url.clone(),
            version,
            result.metadata.clone(),
        );
        doc_repo.save(&doc).await?;
        Ok(true) // Created new
    }
}

/// Map MIME type to file extension.
pub fn mime_to_extension(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        "text/html" => "html",
        "text/plain" => "txt",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        _ => "bin",
    }
}

/// Result of a refresh operation on a document.
#[allow(dead_code)]
pub enum RefreshResult {
    /// Content changed, new version added
    ContentChanged,
    /// Metadata updated (filename or server_date)
    MetadataUpdated,
    /// No changes needed
    Unchanged,
}

/// Save new version content to disk.
///
/// Returns the path where the content was saved.
#[allow(dead_code)]
pub fn save_version_content(
    content: &[u8],
    mime_type: &str,
    documents_dir: &Path,
) -> anyhow::Result<std::path::PathBuf> {
    let content_hash = DocumentVersion::compute_hash(content);
    let content_path = documents_dir.join(&content_hash[..2]).join(format!(
        "{}.{}",
        &content_hash[..8],
        mime_to_extension(mime_type)
    ));

    if let Some(parent) = content_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&content_path, content)?;

    Ok(content_path)
}
