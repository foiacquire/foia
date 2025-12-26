//! Document repository for SQLite persistence.
//!
//! This module is split into submodules for maintainability:
//! - `schema`: Database schema initialization and migrations
//! - `crud`: Basic create, read, update, delete operations
//! - `query`: Complex queries, browsing, search
//! - `stats`: Counting and statistics
//! - `pages`: Document page and OCR operations
//! - `virtual_files`: Archive/email virtual file handling
//! - `annotations`: Document annotation tracking
//! - `dates`: Date estimation and management
//! - `helpers`: Shared parsing and query building utilities
//!
//! This module contains both sync (rusqlite) and async (sqlx) implementations.

#![allow(dead_code)]

mod annotations;
mod crud;
mod dates;
mod helpers;
mod pages;
mod query;
mod schema;
mod stats;
mod virtual_files;

use chrono::{DateTime, Utc};
use sqlx::sqlite::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::Result;
use crate::models::{Document, DocumentStatus, DocumentVersion, VirtualFile};

// Re-export public types
pub use helpers::{
    extract_filename_parts, sanitize_filename, BrowseResult, DocumentNavigation, DocumentSummary,
    VersionSummary,
};

/// Current storage format version. Increment when changing file naming scheme.
pub(crate) const STORAGE_FORMAT_VERSION: i32 = 13;

/// SQLite-backed document repository.
pub struct DocumentRepository {
    pub(crate) db_path: PathBuf,
    pub(crate) documents_dir: PathBuf,
}

impl DocumentRepository {
    /// Create a new document repository.
    pub fn new(db_path: &Path, documents_dir: &Path) -> Result<Self> {
        let repo = Self {
            db_path: db_path.to_path_buf(),
            documents_dir: documents_dir.to_path_buf(),
        };
        repo.init_schema()?;
        repo.migrate_storage()?;
        Ok(repo)
    }

    pub(crate) fn connect(&self) -> Result<Connection> {
        super::connect(&self.db_path)
    }

    /// Get the documents directory path.
    pub fn documents_dir(&self) -> &Path {
        &self.documents_dir
    }

    /// Get the database path.
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

// ============================================================================
// ASYNC (sqlx) implementation - for new code and gradual migration
// ============================================================================

use helpers::DocumentPartial;

/// Row type for Document SQLx query mapping.
#[derive(sqlx::FromRow)]
struct DocumentRow {
    id: String,
    source_id: String,
    title: String,
    source_url: String,
    extracted_text: Option<String>,
    synopsis: Option<String>,
    tags: Option<String>,
    status: String,
    metadata: String,
    created_at: String,
    updated_at: String,
    discovery_method: String,
}

impl DocumentRow {
    fn into_partial(self) -> DocumentPartial {
        let tags: Vec<String> = self
            .tags
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        DocumentPartial {
            id: self.id,
            source_id: self.source_id,
            title: self.title,
            source_url: self.source_url,
            extracted_text: self.extracted_text,
            synopsis: self.synopsis,
            tags,
            status: DocumentStatus::from_str(&self.status).unwrap_or(DocumentStatus::Pending),
            metadata: serde_json::from_str(&self.metadata)
                .unwrap_or(serde_json::Value::Object(Default::default())),
            created_at: DateTime::parse_from_rfc3339(&self.created_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            updated_at: DateTime::parse_from_rfc3339(&self.updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            discovery_method: self.discovery_method,
        }
    }
}

/// Row type for DocumentVersion SQLx query mapping.
#[derive(sqlx::FromRow)]
struct VersionRow {
    id: i64,
    #[allow(dead_code)]
    document_id: String,
    content_hash: String,
    file_path: String,
    file_size: i64,
    mime_type: String,
    acquired_at: String,
    source_url: Option<String>,
    original_filename: Option<String>,
    server_date: Option<String>,
    page_count: Option<i64>,
}

impl From<VersionRow> for DocumentVersion {
    fn from(row: VersionRow) -> Self {
        DocumentVersion {
            id: row.id,
            content_hash: row.content_hash,
            file_path: PathBuf::from(row.file_path),
            file_size: row.file_size as u64,
            mime_type: row.mime_type,
            acquired_at: DateTime::parse_from_rfc3339(&row.acquired_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
            source_url: row.source_url,
            original_filename: row.original_filename,
            server_date: row
                .server_date
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            page_count: row.page_count.map(|c| c as u32),
        }
    }
}

/// Async SQLx-backed document repository.
#[derive(Clone)]
pub struct AsyncDocumentRepository {
    pool: SqlitePool,
    documents_dir: PathBuf,
}

impl AsyncDocumentRepository {
    /// Create a new async document repository with an existing pool.
    pub fn new(pool: SqlitePool, documents_dir: PathBuf) -> Self {
        Self {
            pool,
            documents_dir,
        }
    }

    /// Get the documents directory path.
    pub fn documents_dir(&self) -> &Path {
        &self.documents_dir
    }

    // ========================================================================
    // Core CRUD operations
    // ========================================================================

    /// Get a document by ID.
    pub async fn get(&self, id: &str) -> Result<Option<Document>> {
        let row = sqlx::query_as!(
            DocumentRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                title as "title!",
                source_url as "source_url!",
                extracted_text,
                synopsis,
                tags,
                status as "status!",
                metadata as "metadata!",
                created_at as "created_at!",
                updated_at as "updated_at!",
                discovery_method as "discovery_method!"
               FROM documents WHERE id = ?"#,
            id
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let partial = row.into_partial();
                let versions = self.load_versions(&partial.id).await?;
                Ok(Some(partial.with_versions(versions)))
            }
            None => Ok(None),
        }
    }

    /// Get a document by source URL.
    pub async fn get_by_url(&self, url: &str) -> Result<Option<Document>> {
        let row = sqlx::query_as!(
            DocumentRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                title as "title!",
                source_url as "source_url!",
                extracted_text,
                synopsis,
                tags,
                status as "status!",
                metadata as "metadata!",
                created_at as "created_at!",
                updated_at as "updated_at!",
                discovery_method as "discovery_method!"
               FROM documents WHERE source_url = ?"#,
            url
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let partial = row.into_partial();
                let versions = self.load_versions(&partial.id).await?;
                Ok(Some(partial.with_versions(versions)))
            }
            None => Ok(None),
        }
    }

    /// Get just the source URLs for a source (lightweight, for URL analysis).
    pub async fn get_urls_by_source(&self, source_id: &str) -> Result<Vec<String>> {
        let urls = sqlx::query_scalar!(
            r#"SELECT source_url as "source_url!" FROM documents WHERE source_id = ?"#,
            source_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(urls)
    }

    /// Get all source URLs as a HashSet for fast duplicate detection during import.
    pub async fn get_all_urls_set(&self) -> Result<HashSet<String>> {
        let urls = sqlx::query_scalar!(r#"SELECT source_url as "source_url!" FROM documents"#)
            .fetch_all(&self.pool)
            .await?;

        Ok(urls.into_iter().collect())
    }

    /// Get all content hashes as a HashSet for fast content deduplication during import.
    pub async fn get_all_content_hashes(&self) -> Result<HashSet<String>> {
        let hashes = sqlx::query_scalar!(
            r#"SELECT DISTINCT content_hash as "content_hash!" FROM document_versions"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(hashes.into_iter().collect())
    }

    /// Check if a document exists.
    pub async fn exists(&self, id: &str) -> Result<bool> {
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM documents WHERE id = ?"#,
            id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Check if content hash exists.
    pub async fn content_exists(&self, content_hash: &str) -> Result<bool> {
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM document_versions WHERE content_hash = ?"#,
            content_hash
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Save a document (insert or update).
    pub async fn save(&self, doc: &Document) -> Result<()> {
        let tags_json = serde_json::to_string(&doc.tags)?;
        let metadata_json = serde_json::to_string(&doc.metadata)?;
        let created_at = doc.created_at.to_rfc3339();
        let updated_at = doc.updated_at.to_rfc3339();
        let status = doc.status.as_str();

        sqlx::query!(
            r#"INSERT INTO documents (id, source_id, title, source_url, extracted_text, synopsis, tags, status, metadata, created_at, updated_at, discovery_method)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
               ON CONFLICT(id) DO UPDATE SET
                   title = excluded.title,
                   source_url = excluded.source_url,
                   extracted_text = excluded.extracted_text,
                   synopsis = excluded.synopsis,
                   tags = excluded.tags,
                   status = excluded.status,
                   metadata = excluded.metadata,
                   updated_at = excluded.updated_at"#,
            doc.id,
            doc.source_id,
            doc.title,
            doc.source_url,
            doc.extracted_text,
            doc.synopsis,
            tags_json,
            status,
            metadata_json,
            created_at,
            updated_at,
            doc.discovery_method
        )
        .execute(&self.pool)
        .await?;

        // Get existing version hashes
        let existing_hashes: Vec<String> = sqlx::query_scalar!(
            r#"SELECT content_hash as "content_hash!" FROM document_versions WHERE document_id = ?"#,
            doc.id
        )
        .fetch_all(&self.pool)
        .await?;

        // Insert new versions
        for version in &doc.versions {
            if !existing_hashes.contains(&version.content_hash) {
                let file_path = version.file_path.to_string_lossy().to_string();
                let file_size = version.file_size as i64;
                let acquired_at = version.acquired_at.to_rfc3339();
                let server_date = version.server_date.map(|d| d.to_rfc3339());
                let page_count = version.page_count.map(|c| c as i64);

                sqlx::query!(
                    r#"INSERT INTO document_versions
                        (document_id, content_hash, file_path, file_size, mime_type, acquired_at, source_url, original_filename, server_date, page_count)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
                    doc.id,
                    version.content_hash,
                    file_path,
                    file_size,
                    version.mime_type,
                    acquired_at,
                    version.source_url,
                    version.original_filename,
                    server_date,
                    page_count
                )
                .execute(&self.pool)
                .await?;
            }
        }

        Ok(())
    }

    /// Delete a document and its versions.
    pub async fn delete(&self, id: &str) -> Result<bool> {
        sqlx::query!("DELETE FROM document_versions WHERE document_id = ?", id)
            .execute(&self.pool)
            .await?;

        let result = sqlx::query!("DELETE FROM documents WHERE id = ?", id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Update the MIME type of a specific document version.
    pub async fn update_version_mime_type(
        &self,
        document_id: &str,
        version_id: i64,
        new_mime_type: &str,
    ) -> Result<()> {
        sqlx::query!(
            "UPDATE document_versions SET mime_type = ?1 WHERE document_id = ?2 AND id = ?3",
            new_mime_type,
            document_id,
            version_id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ========================================================================
    // Version loading helpers
    // ========================================================================

    /// Load versions for a document.
    async fn load_versions(&self, document_id: &str) -> Result<Vec<DocumentVersion>> {
        let rows = sqlx::query_as!(
            VersionRow,
            r#"SELECT
                id as "id!",
                document_id as "document_id!",
                content_hash as "content_hash!",
                file_path as "file_path!",
                file_size as "file_size!",
                mime_type as "mime_type!",
                acquired_at as "acquired_at!",
                source_url,
                original_filename,
                server_date,
                page_count
               FROM document_versions
               WHERE document_id = ?
               ORDER BY acquired_at DESC"#,
            document_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(DocumentVersion::from).collect())
    }

    /// Load versions for multiple documents in a single query.
    pub async fn load_versions_bulk(
        &self,
        document_ids: &[String],
    ) -> Result<HashMap<String, Vec<DocumentVersion>>> {
        if document_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // SQLx doesn't support IN with dynamic arrays in query! macro,
        // so we use a different approach - fetch all then filter
        // For large datasets, this should be batched
        let mut versions_map: HashMap<String, Vec<DocumentVersion>> = HashMap::new();

        // Process in batches
        const BATCH_SIZE: usize = 100;
        for chunk in document_ids.chunks(BATCH_SIZE) {
            for doc_id in chunk {
                let versions = self.load_versions(doc_id).await?;
                versions_map.insert(doc_id.clone(), versions);
            }
        }

        Ok(versions_map)
    }

    // ========================================================================
    // Stats and counts
    // ========================================================================

    /// Count total documents.
    pub async fn count(&self) -> Result<u64> {
        let count: i32 = sqlx::query_scalar!(r#"SELECT COUNT(*) as "count!: i32" FROM documents"#)
            .fetch_one(&self.pool)
            .await?;

        Ok(count as u64)
    }

    /// Count documents by source.
    pub async fn count_by_source(&self, source_id: &str) -> Result<u64> {
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM documents WHERE source_id = ?"#,
            source_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count as u64)
    }

    /// Count documents by status.
    pub async fn count_by_status(&self, status: DocumentStatus) -> Result<u64> {
        let status_str = status.as_str();
        let count: i32 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i32" FROM documents WHERE status = ?"#,
            status_str
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count as u64)
    }

    /// Update document status.
    pub async fn update_status(&self, id: &str, status: DocumentStatus) -> Result<()> {
        let status_str = status.as_str();
        let now = Utc::now().to_rfc3339();

        sqlx::query!(
            "UPDATE documents SET status = ?, updated_at = ? WHERE id = ?",
            status_str,
            now,
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update document extracted text.
    pub async fn update_extracted_text(&self, id: &str, text: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        sqlx::query!(
            "UPDATE documents SET extracted_text = ?, updated_at = ? WHERE id = ?",
            text,
            now,
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update document synopsis.
    pub async fn update_synopsis(&self, id: &str, synopsis: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        sqlx::query!(
            "UPDATE documents SET synopsis = ?, updated_at = ? WHERE id = ?",
            synopsis,
            now,
            id
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get all documents by source.
    pub async fn get_by_source(&self, source_id: &str) -> Result<Vec<Document>> {
        let rows = sqlx::query_as!(
            DocumentRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                title as "title!",
                source_url as "source_url!",
                extracted_text,
                synopsis,
                tags,
                status as "status!",
                metadata as "metadata!",
                created_at as "created_at!",
                updated_at as "updated_at!",
                discovery_method as "discovery_method!"
               FROM documents WHERE source_id = ?"#,
            source_id
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get documents by status.
    pub async fn get_by_status(&self, status: DocumentStatus) -> Result<Vec<Document>> {
        let status_str = status.as_str();

        let rows = sqlx::query_as!(
            DocumentRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                title as "title!",
                source_url as "source_url!",
                extracted_text,
                synopsis,
                tags,
                status as "status!",
                metadata as "metadata!",
                created_at as "created_at!",
                updated_at as "updated_at!",
                discovery_method as "discovery_method!"
               FROM documents WHERE status = ?"#,
            status_str
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get all documents.
    pub async fn get_all(&self) -> Result<Vec<Document>> {
        let rows = sqlx::query_as!(
            DocumentRow,
            r#"SELECT
                id as "id!",
                source_id as "source_id!",
                title as "title!",
                source_url as "source_url!",
                extracted_text,
                synopsis,
                tags,
                status as "status!",
                metadata as "metadata!",
                created_at as "created_at!",
                updated_at as "updated_at!",
                discovery_method as "discovery_method!"
               FROM documents ORDER BY updated_at DESC"#
        )
        .fetch_all(&self.pool)
        .await?;

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get documents filtered by tag.
    pub async fn get_by_tag(&self, tag: &str, source_id: Option<&str>) -> Result<Vec<Document>> {
        let tag_pattern = format!("%\"{}%", tag.to_lowercase());

        let rows = match source_id {
            Some(sid) => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT
                        id as "id!",
                        source_id as "source_id!",
                        title as "title!",
                        source_url as "source_url!",
                        extracted_text,
                        synopsis,
                        tags,
                        status as "status!",
                        metadata as "metadata!",
                        created_at as "created_at!",
                        updated_at as "updated_at!",
                        discovery_method as "discovery_method!"
                       FROM documents WHERE LOWER(tags) LIKE ?1 AND source_id = ?2 ORDER BY updated_at DESC"#,
                    tag_pattern,
                    sid
                )
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT
                        id as "id!",
                        source_id as "source_id!",
                        title as "title!",
                        source_url as "source_url!",
                        extracted_text,
                        synopsis,
                        tags,
                        status as "status!",
                        metadata as "metadata!",
                        created_at as "created_at!",
                        updated_at as "updated_at!",
                        discovery_method as "discovery_method!"
                       FROM documents WHERE LOWER(tags) LIKE ?1 ORDER BY updated_at DESC"#,
                    tag_pattern
                )
                .fetch_all(&self.pool)
                .await?
            }
        };

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get documents filtered by type category (pdf, image, office, etc.).
    pub async fn get_by_type_category(
        &self,
        type_name: &str,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>> {
        use crate::utils::mime_type_sql_condition;

        let mime_condition = match mime_type_sql_condition(type_name) {
            Some(cond) => cond,
            None => return Ok(vec![]), // Unknown type category
        };
        let limit_val = limit.max(1) as i64;

        // Build dynamic query based on source_id
        let rows = match source_id {
            Some(sid) => {
                // Use raw query since mime_condition is dynamic
                sqlx::query_as::<_, DocumentRow>(&format!(
                    r#"SELECT d.id, d.source_id, d.title, d.source_url, d.extracted_text,
                              d.synopsis, d.tags, d.status, d.metadata, d.created_at,
                              d.updated_at, d.discovery_method
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE {} AND d.source_id = ?
                       GROUP BY d.id
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    mime_condition
                ))
                .bind(sid)
                .bind(limit_val)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, DocumentRow>(&format!(
                    r#"SELECT d.id, d.source_id, d.title, d.source_url, d.extracted_text,
                              d.synopsis, d.tags, d.status, d.metadata, d.created_at,
                              d.updated_at, d.discovery_method
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE {}
                       GROUP BY d.id
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    mime_condition
                ))
                .bind(limit_val)
                .fetch_all(&self.pool)
                .await?
            }
        };

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get the version ID for a document's current version.
    pub async fn get_current_version_id(&self, document_id: &str) -> Result<Option<i64>> {
        let id: Option<i64> = sqlx::query_scalar!(
            r#"SELECT id as "id!" FROM document_versions WHERE document_id = ? ORDER BY acquired_at DESC LIMIT 1"#,
            document_id
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(id)
    }

    /// Insert a new virtual file.
    pub async fn insert_virtual_file(&self, vf: &VirtualFile) -> Result<()> {
        let tags_json = serde_json::to_string(&vf.tags).unwrap_or_else(|_| "[]".to_string());
        let file_size = vf.file_size as i64;
        let created_at = vf.created_at.to_rfc3339();
        let updated_at = vf.updated_at.to_rfc3339();
        let status = vf.status.as_str();

        sqlx::query!(
            r#"INSERT INTO virtual_files (id, document_id, version_id, archive_path, filename, mime_type, file_size, extracted_text, synopsis, tags, status, created_at, updated_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
            vf.id,
            vf.document_id,
            vf.version_id,
            vf.archive_path,
            vf.filename,
            vf.mime_type,
            file_size,
            vf.extracted_text,
            vf.synopsis,
            tags_json,
            status,
            created_at,
            updated_at
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Count archive documents that haven't been processed for virtual files.
    pub async fn count_unprocessed_archives(&self, source_id: Option<&str>) -> Result<u64> {
        let count: i32 = match source_id {
            Some(sid) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) as "count!: i32" FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE (dv.mime_type = 'application/zip' OR dv.mime_type = 'application/x-zip-compressed')
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       AND d.source_id = ?"#,
                    sid
                )
                .fetch_one(&self.pool)
                .await?
            }
            None => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) as "count!: i32" FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE (dv.mime_type = 'application/zip' OR dv.mime_type = 'application/x-zip-compressed')
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)"#
                )
                .fetch_one(&self.pool)
                .await?
            }
        };

        Ok(count as u64)
    }

    /// Count email documents that haven't been processed for attachments.
    pub async fn count_unprocessed_emails(&self, source_id: Option<&str>) -> Result<u64> {
        let count: i32 = match source_id {
            Some(sid) => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) as "count!: i32" FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE dv.mime_type = 'message/rfc822'
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       AND d.source_id = ?"#,
                    sid
                )
                .fetch_one(&self.pool)
                .await?
            }
            None => {
                sqlx::query_scalar!(
                    r#"SELECT COUNT(*) as "count!: i32" FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE dv.mime_type = 'message/rfc822'
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)"#
                )
                .fetch_one(&self.pool)
                .await?
            }
        };

        Ok(count as u64)
    }

    /// Get archive documents that haven't been processed for virtual files.
    pub async fn get_unprocessed_archives(
        &self,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>> {
        let limit_val = limit.max(1) as i64;

        let rows = match source_id {
            Some(sid) => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT d.id as "id!", d.source_id as "source_id!", d.title as "title!",
                              d.source_url as "source_url!", d.extracted_text, d.synopsis, d.tags,
                              d.status as "status!", d.metadata as "metadata!", d.created_at as "created_at!",
                              d.updated_at as "updated_at!", d.discovery_method as "discovery_method!"
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE (dv.mime_type = 'application/zip' OR dv.mime_type = 'application/x-zip-compressed')
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       AND d.source_id = ?
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    sid,
                    limit_val
                )
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT d.id as "id!", d.source_id as "source_id!", d.title as "title!",
                              d.source_url as "source_url!", d.extracted_text, d.synopsis, d.tags,
                              d.status as "status!", d.metadata as "metadata!", d.created_at as "created_at!",
                              d.updated_at as "updated_at!", d.discovery_method as "discovery_method!"
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE (dv.mime_type = 'application/zip' OR dv.mime_type = 'application/x-zip-compressed')
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    limit_val
                )
                .fetch_all(&self.pool)
                .await?
            }
        };

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }

    /// Get email documents that haven't been processed for attachments.
    pub async fn get_unprocessed_emails(
        &self,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>> {
        let limit_val = limit.max(1) as i64;

        let rows = match source_id {
            Some(sid) => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT d.id as "id!", d.source_id as "source_id!", d.title as "title!",
                              d.source_url as "source_url!", d.extracted_text, d.synopsis, d.tags,
                              d.status as "status!", d.metadata as "metadata!", d.created_at as "created_at!",
                              d.updated_at as "updated_at!", d.discovery_method as "discovery_method!"
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE dv.mime_type = 'message/rfc822'
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       AND d.source_id = ?
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    sid,
                    limit_val
                )
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as!(
                    DocumentRow,
                    r#"SELECT d.id as "id!", d.source_id as "source_id!", d.title as "title!",
                              d.source_url as "source_url!", d.extracted_text, d.synopsis, d.tags,
                              d.status as "status!", d.metadata as "metadata!", d.created_at as "created_at!",
                              d.updated_at as "updated_at!", d.discovery_method as "discovery_method!"
                       FROM documents d
                       JOIN document_versions dv ON d.id = dv.document_id
                       WHERE dv.mime_type = 'message/rfc822'
                       AND dv.id = (SELECT MAX(dv2.id) FROM document_versions dv2 WHERE dv2.document_id = d.id)
                       AND NOT EXISTS (SELECT 1 FROM virtual_files vf WHERE vf.version_id = dv.id)
                       ORDER BY d.updated_at DESC
                       LIMIT ?"#,
                    limit_val
                )
                .fetch_all(&self.pool)
                .await?
            }
        };

        if rows.is_empty() {
            return Ok(vec![]);
        }

        let doc_ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
        let versions_map = self.load_versions_bulk(&doc_ids).await?;

        let docs = rows
            .into_iter()
            .map(|row| {
                let id = row.id.clone();
                let partial = row.into_partial();
                let versions = versions_map.get(&id).cloned().unwrap_or_default();
                partial.with_versions(versions)
            })
            .collect();

        Ok(docs)
    }
}
