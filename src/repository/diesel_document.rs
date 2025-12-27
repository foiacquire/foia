//! Diesel-based document repository for SQLite.
//!
//! This module provides async database access for document operations using Diesel ORM.
//! Note: This uses the simplified schema from context.rs. The full SQLx implementation
//! uses additional columns that would require schema migration.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::diesel_models::{DocumentPageRecord, DocumentRecord, DocumentVersionRecord, NewDocument, VirtualFileRecord};
use super::diesel_pool::{run_blocking, SqlitePool};
use super::{parse_datetime, parse_datetime_opt};
use crate::models::{Document, DocumentStatus, DocumentVersion, VirtualFile, VirtualFileStatus};
use crate::schema::{document_pages, document_versions, documents, virtual_files};

/// Summary of a document for list views.
#[derive(Debug, Clone)]
pub struct DieselDocumentSummary {
    pub id: String,
    pub source_id: String,
    pub url: String,
    pub title: Option<String>,
    pub status: DocumentStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version_count: u32,
    pub latest_file_size: Option<u64>,
}

/// Diesel-based document repository with compile-time query checking.
#[derive(Clone)]
pub struct DieselDocumentRepository {
    pool: SqlitePool,
    documents_dir: PathBuf,
}

impl DieselDocumentRepository {
    /// Create a new Diesel document repository.
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

    /// Count all documents.
    pub async fn count(&self) -> Result<u64, diesel::result::Error> {
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let count: i64 = documents::table.select(count_star()).first(conn)?;
            Ok(count as u64)
        })
        .await
    }

    /// Get document counts per source (for stats).
    pub async fn get_all_source_counts(
        &self,
    ) -> Result<std::collections::HashMap<String, u64>, diesel::result::Error> {
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            let rows: Vec<SourceCount> = diesel::sql_query(
                "SELECT source_id, COUNT(*) as count FROM documents GROUP BY source_id",
            )
            .load(conn)?;

            let mut counts = std::collections::HashMap::new();
            for SourceCount { source_id, count } in rows {
                counts.insert(source_id, count as u64);
            }
            Ok(counts)
        })
        .await
    }

    /// Count documents needing OCR (stub - returns 0 for now).
    pub async fn count_needing_ocr(&self, _source_id: Option<&str>) -> Result<u64, diesel::result::Error> {
        // This would require checking document pages for OCR status
        // For now, return 0 as a stub
        Ok(0)
    }

    /// Count documents needing summarization (stub - returns 0 for now).
    pub async fn count_needing_summarization(&self, _source_id: Option<&str>) -> Result<u64, diesel::result::Error> {
        // This would require checking synopsis field
        // For now, return 0 as a stub
        Ok(0)
    }

    /// Get type statistics (stub - returns empty for now).
    pub async fn get_type_stats(&self) -> Result<std::collections::HashMap<String, u64>, diesel::result::Error> {
        // This would require checking MIME types of versions
        // For now, return empty as a stub
        Ok(std::collections::HashMap::new())
    }

    /// Get recent documents (stub).
    pub async fn get_recent(&self, limit: u32) -> Result<Vec<Document>, diesel::result::Error> {
        let limit = limit as i64;
        let pool = self.pool.clone();

        let records = run_blocking(pool.clone(), move |conn| {
            documents::table
                .order(documents::updated_at.desc())
                .limit(limit)
                .load::<DocumentRecord>(conn)
        })
        .await?;

        let mut docs = Vec::with_capacity(records.len());
        for record in records {
            let versions = self.load_versions(&record.id).await?;
            docs.push(Self::record_to_document(record, versions));
        }
        Ok(docs)
    }

    /// Get category statistics (stub - returns empty).
    pub async fn get_category_stats(&self) -> Result<std::collections::HashMap<String, u64>, diesel::result::Error> {
        Ok(std::collections::HashMap::new())
    }

    /// Search tags (stub - returns empty).
    pub async fn search_tags(&self, _query: &str) -> Result<Vec<String>, diesel::result::Error> {
        Ok(vec![])
    }

    /// Get all tags (stub - returns empty).
    pub async fn get_all_tags(&self) -> Result<Vec<String>, diesel::result::Error> {
        Ok(vec![])
    }

    /// Browse documents (stub).
    pub async fn browse(
        &self,
        source_id: Option<&str>,
        status: Option<&str>,
        _category: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Document>, diesel::result::Error> {
        let source_id = source_id.map(|s| s.to_string());
        let status = status.map(|s| s.to_string());
        let limit = limit as i64;
        let offset = offset as i64;
        let pool = self.pool.clone();

        let records = run_blocking(pool.clone(), move |conn| {
            let mut query = documents::table
                .order(documents::updated_at.desc())
                .limit(limit)
                .offset(offset)
                .into_boxed();

            if let Some(ref sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(ref st) = status {
                query = query.filter(documents::status.eq(st));
            }

            query.load::<DocumentRecord>(conn)
        })
        .await?;

        let mut docs = Vec::with_capacity(records.len());
        for record in records {
            let versions = self.load_versions(&record.id).await?;
            docs.push(Self::record_to_document(record, versions));
        }
        Ok(docs)
    }

    /// Browse count (stub).
    pub async fn browse_count(
        &self,
        source_id: Option<&str>,
        status: Option<&str>,
        _category: Option<&str>,
    ) -> Result<u64, diesel::result::Error> {
        let source_id = source_id.map(|s| s.to_string());
        let status = status.map(|s| s.to_string());
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let mut query = documents::table.select(count_star()).into_boxed();

            if let Some(ref sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(ref st) = status {
                query = query.filter(documents::status.eq(st));
            }

            let count: i64 = query.first(conn)?;
            Ok(count as u64)
        })
        .await
    }

    /// Get document navigation with prev/next documents, position, and total.
    pub async fn get_document_navigation(
        &self,
        document_id: &str,
        source_id: &str,
    ) -> Result<super::document::DocumentNavigation, diesel::result::Error> {
        use super::document::DocumentNavigation;
        use diesel::dsl::count_star;

        let doc_id = document_id.to_string();
        let source_id = source_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            // Get previous document (id and title)
            let prev: Option<(String, Option<String>)> = documents::table
                .select((documents::id, documents::title))
                .filter(documents::source_id.eq(&source_id))
                .filter(documents::id.lt(&doc_id))
                .order(documents::id.desc())
                .first(conn)
                .optional()?;

            // Get next document (id and title)
            let next: Option<(String, Option<String>)> = documents::table
                .select((documents::id, documents::title))
                .filter(documents::source_id.eq(&source_id))
                .filter(documents::id.gt(&doc_id))
                .order(documents::id.asc())
                .first(conn)
                .optional()?;

            // Get position (1-indexed: count of docs with id <= current)
            let position: i64 = documents::table
                .filter(documents::source_id.eq(&source_id))
                .filter(documents::id.le(&doc_id))
                .select(count_star())
                .first(conn)?;

            // Get total count for this source
            let total: i64 = documents::table
                .filter(documents::source_id.eq(&source_id))
                .select(count_star())
                .first(conn)?;

            Ok(DocumentNavigation {
                prev_id: prev.as_ref().map(|(id, _)| id.clone()),
                prev_title: prev.and_then(|(_, title)| title),
                next_id: next.as_ref().map(|(id, _)| id.clone()),
                next_title: next.and_then(|(_, title)| title),
                position: position as u64,
                total: total as u64,
            })
        })
        .await
    }

    /// Count pages for a document (stub).
    pub async fn count_pages(&self, document_id: &str, version: i32) -> Result<u32, diesel::result::Error> {
        let document_id = document_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let count: i64 = document_pages::table
                .filter(document_pages::document_id.eq(&document_id))
                .filter(document_pages::version.eq(version))
                .select(count_star())
                .first(conn)?;
            Ok(count as u32)
        })
        .await
    }

    // ========================================================================
    // Core CRUD Operations
    // ========================================================================

    /// Get a document by ID (without versions).
    pub async fn get(&self, id: &str) -> Result<Option<Document>, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        let record = run_blocking(pool.clone(), move |conn| {
            documents::table.find(&id).first::<DocumentRecord>(conn).optional()
        })
        .await?;

        match record {
            Some(record) => {
                let doc_id = record.id.clone();
                let versions = self.load_versions(&doc_id).await?;
                Ok(Some(Self::record_to_document(record, versions)))
            }
            None => Ok(None),
        }
    }

    /// Get all documents for a source.
    pub async fn get_by_source(&self, source_id: &str) -> Result<Vec<Document>, diesel::result::Error> {
        let source_id = source_id.to_string();
        let pool = self.pool.clone();

        let records = run_blocking(pool.clone(), move |conn| {
            documents::table
                .filter(documents::source_id.eq(&source_id))
                .order(documents::created_at.desc())
                .load::<DocumentRecord>(conn)
        })
        .await?;

        let mut docs = Vec::with_capacity(records.len());
        for record in records {
            let versions = self.load_versions(&record.id).await?;
            docs.push(Self::record_to_document(record, versions));
        }
        Ok(docs)
    }

    /// Get documents by URL.
    pub async fn get_by_url(&self, url: &str) -> Result<Vec<Document>, diesel::result::Error> {
        let url = url.to_string();
        let pool = self.pool.clone();

        let records = run_blocking(pool.clone(), move |conn| {
            documents::table
                .filter(documents::url.eq(&url))
                .load::<DocumentRecord>(conn)
        })
        .await?;

        let mut docs = Vec::with_capacity(records.len());
        for record in records {
            let versions = self.load_versions(&record.id).await?;
            docs.push(Self::record_to_document(record, versions));
        }
        Ok(docs)
    }

    /// Check if a document exists.
    pub async fn exists(&self, id: &str) -> Result<bool, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let count: i64 = documents::table
                .filter(documents::id.eq(&id))
                .select(count_star())
                .first(conn)?;
            Ok(count > 0)
        })
        .await
    }

    /// Save a document (insert or update).
    pub async fn save(&self, doc: &Document) -> Result<(), diesel::result::Error> {
        let id = doc.id.clone();
        let source_id = doc.source_id.clone();
        let url = doc.source_url.clone();
        let title = doc.title.clone();
        let status = doc.status.as_str().to_string();
        let metadata = serde_json::to_string(&doc.metadata).unwrap_or_else(|_| "{}".to_string());
        let created_at = doc.created_at.to_rfc3339();
        let updated_at = doc.updated_at.to_rfc3339();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            diesel::replace_into(documents::table)
                .values((
                    documents::id.eq(&id),
                    documents::source_id.eq(&source_id),
                    documents::url.eq(&url),
                    documents::title.eq(&title),
                    documents::status.eq(&status),
                    documents::metadata.eq(&metadata),
                    documents::created_at.eq(&created_at),
                    documents::updated_at.eq(&updated_at),
                ))
                .execute(conn)?;
            Ok(())
        })
        .await
    }

    /// Delete a document and all its versions.
    pub async fn delete(&self, id: &str) -> Result<bool, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            conn.transaction::<_, diesel::result::Error, _>(|conn| {
                // Delete versions
                diesel::delete(document_versions::table.filter(document_versions::document_id.eq(&id)))
                    .execute(conn)?;

                // Delete pages
                diesel::delete(document_pages::table.filter(document_pages::document_id.eq(&id)))
                    .execute(conn)?;

                // Delete virtual files
                diesel::delete(virtual_files::table.filter(virtual_files::document_id.eq(&id)))
                    .execute(conn)?;

                // Delete document
                let rows = diesel::delete(documents::table.find(&id)).execute(conn)?;
                Ok(rows > 0)
            })
        })
        .await
    }

    /// Update document status.
    pub async fn update_status(
        &self,
        id: &str,
        status: DocumentStatus,
    ) -> Result<(), diesel::result::Error> {
        let id = id.to_string();
        let status_str = status.as_str().to_string();
        let updated_at = Utc::now().to_rfc3339();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            diesel::update(documents::table.find(&id))
                .set((
                    documents::status.eq(&status_str),
                    documents::updated_at.eq(&updated_at),
                ))
                .execute(conn)?;
            Ok(())
        })
        .await
    }

    // ========================================================================
    // Version Operations
    // ========================================================================

    /// Load versions for a document.
    async fn load_versions(&self, document_id: &str) -> Result<Vec<DocumentVersion>, diesel::result::Error> {
        let document_id = document_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            document_versions::table
                .filter(document_versions::document_id.eq(&document_id))
                .order(document_versions::version.desc())
                .load::<DocumentVersionRecord>(conn)
        })
        .await
        .map(|records| records.into_iter().map(Self::version_record_to_model).collect())
    }

    /// Add a new version to a document.
    pub async fn add_version(
        &self,
        document_id: &str,
        version: &DocumentVersion,
    ) -> Result<i64, diesel::result::Error> {
        let document_id = document_id.to_string();
        let version_num = version.id as i32;
        let file_path = version.file_path.to_string_lossy().to_string();
        let content_hash = version.content_hash.clone();
        let mime_type = version.mime_type.clone();
        let file_size = version.file_size as i32;
        let fetched_at = version.acquired_at.to_rfc3339();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            diesel::insert_into(document_versions::table)
                .values((
                    document_versions::document_id.eq(&document_id),
                    document_versions::version.eq(version_num),
                    document_versions::file_path.eq(Some(&file_path)),
                    document_versions::content_hash.eq(Some(&content_hash)),
                    document_versions::mime_type.eq(Some(&mime_type)),
                    document_versions::file_size.eq(Some(file_size)),
                    document_versions::fetched_at.eq(&fetched_at),
                ))
                .execute(conn)?;

            // Get the last insert ID
            diesel::sql_query("SELECT last_insert_rowid()")
                .get_result::<LastInsertRowId>(conn)
                .map(|r| r.id)
        })
        .await
    }

    /// Get the latest version for a document.
    pub async fn get_latest_version(
        &self,
        document_id: &str,
    ) -> Result<Option<DocumentVersion>, diesel::result::Error> {
        let document_id = document_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            document_versions::table
                .filter(document_versions::document_id.eq(&document_id))
                .order(document_versions::version.desc())
                .first::<DocumentVersionRecord>(conn)
                .optional()
        })
        .await
        .map(|opt| opt.map(Self::version_record_to_model))
    }

    // ========================================================================
    // Statistics
    // ========================================================================

    /// Count documents by source.
    pub async fn count_by_source(&self, source_id: &str) -> Result<u64, diesel::result::Error> {
        let source_id = source_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let count: i64 = documents::table
                .filter(documents::source_id.eq(&source_id))
                .select(count_star())
                .first(conn)?;
            Ok(count as u64)
        })
        .await
    }

    /// Count documents by status.
    pub async fn count_by_status(
        &self,
        source_id: Option<&str>,
    ) -> Result<std::collections::HashMap<String, u64>, diesel::result::Error> {
        let source_id = source_id.map(|s| s.to_string());
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            let query = if let Some(ref sid) = source_id {
                format!(
                    "SELECT status, COUNT(*) as count FROM documents WHERE source_id = '{}' GROUP BY status",
                    sid
                )
            } else {
                "SELECT status, COUNT(*) as count FROM documents GROUP BY status".to_string()
            };

            let rows: Vec<StatusCount> = diesel::sql_query(&query).load(conn)?;

            let mut counts = std::collections::HashMap::new();
            for StatusCount { status, count } in rows {
                counts.insert(status, count as u64);
            }
            Ok(counts)
        })
        .await
    }

    /// Get document summaries for a source.
    pub async fn get_summaries(
        &self,
        source_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<DieselDocumentSummary>, diesel::result::Error> {
        let source_id = source_id.to_string();
        let limit = limit as i64;
        let offset = offset as i64;
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            let records: Vec<DocumentRecord> = documents::table
                .filter(documents::source_id.eq(&source_id))
                .order(documents::updated_at.desc())
                .limit(limit)
                .offset(offset)
                .load(conn)?;

            let mut summaries = Vec::with_capacity(records.len());
            for record in records {
                // Count versions
                let version_count: i64 = document_versions::table
                    .filter(document_versions::document_id.eq(&record.id))
                    .count()
                    .get_result(conn)?;

                // Get latest version file size
                let latest_size: Option<i32> = document_versions::table
                    .filter(document_versions::document_id.eq(&record.id))
                    .order(document_versions::version.desc())
                    .select(document_versions::file_size)
                    .first(conn)
                    .optional()?
                    .flatten();

                summaries.push(DieselDocumentSummary {
                    id: record.id,
                    source_id: record.source_id,
                    url: record.url,
                    title: record.title,
                    status: DocumentStatus::from_str(&record.status).unwrap_or(DocumentStatus::Pending),
                    created_at: parse_datetime(&record.created_at),
                    updated_at: parse_datetime(&record.updated_at),
                    version_count: version_count as u32,
                    latest_file_size: latest_size.map(|s| s as u64),
                });
            }

            Ok(summaries)
        })
        .await
    }

    // ========================================================================
    // Virtual File Operations
    // ========================================================================

    /// Get virtual files for a document version.
    pub async fn get_virtual_files(
        &self,
        document_id: &str,
        version: i32,
    ) -> Result<Vec<VirtualFile>, diesel::result::Error> {
        let document_id = document_id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            virtual_files::table
                .filter(virtual_files::document_id.eq(&document_id))
                .filter(virtual_files::version.eq(version))
                .load::<VirtualFileRecord>(conn)
        })
        .await
        .map(|records| records.into_iter().map(Self::virtual_file_record_to_model).collect())
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    fn record_to_document(record: DocumentRecord, versions: Vec<DocumentVersion>) -> Document {
        Document {
            id: record.id,
            source_id: record.source_id,
            title: record.title.unwrap_or_default(),
            source_url: record.url,
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status: DocumentStatus::from_str(&record.status).unwrap_or(DocumentStatus::Pending),
            metadata: serde_json::from_str(&record.metadata)
                .unwrap_or(serde_json::Value::Object(Default::default())),
            created_at: parse_datetime(&record.created_at),
            updated_at: parse_datetime(&record.updated_at),
            discovery_method: "unknown".to_string(),
            versions,
        }
    }

    fn version_record_to_model(record: DocumentVersionRecord) -> DocumentVersion {
        DocumentVersion {
            id: record.id as i64,
            content_hash: record.content_hash.unwrap_or_default(),
            file_path: PathBuf::from(record.file_path.unwrap_or_default()),
            file_size: record.file_size.unwrap_or(0) as u64,
            mime_type: record.mime_type.unwrap_or_default(),
            acquired_at: parse_datetime(&record.fetched_at),
            source_url: None,
            original_filename: None,
            server_date: None,
            page_count: None,
        }
    }

    fn virtual_file_record_to_model(record: VirtualFileRecord) -> VirtualFile {
        VirtualFile {
            id: record.id.to_string(),
            document_id: record.document_id,
            version_id: record.version as i64,
            archive_path: record.path,
            filename: String::new(),
            file_size: record.file_size.unwrap_or(0) as u64,
            mime_type: record.mime_type.unwrap_or_default(),
            extracted_text: record.ocr_text,
            synopsis: None,
            tags: vec![],
            status: VirtualFileStatus::from_str(&record.status).unwrap_or(VirtualFileStatus::Pending),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

// Helper struct for SQL query results
#[derive(diesel::QueryableByName)]
struct StatusCount {
    #[diesel(sql_type = diesel::sql_types::Text)]
    status: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

#[derive(diesel::QueryableByName)]
struct SourceCount {
    #[diesel(sql_type = diesel::sql_types::Text)]
    source_id: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    count: i64,
}

#[derive(diesel::QueryableByName)]
struct LastInsertRowId {
    #[diesel(sql_type = diesel::sql_types::BigInt, column_name = "last_insert_rowid()")]
    id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::diesel_pool::create_diesel_pool_from_url;
    use tempfile::tempdir;

    async fn setup_test_db() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_url = format!("{}", db_path.display());

        let pool = create_diesel_pool_from_url(&db_url).unwrap();

        // Create tables
        run_blocking(pool.clone(), |conn| {
            diesel::sql_query(
                r#"CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL,
                    url TEXT NOT NULL,
                    title TEXT,
                    status TEXT NOT NULL DEFAULT 'pending',
                    metadata TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )"#,
            )
            .execute(conn)?;

            diesel::sql_query(
                r#"CREATE TABLE IF NOT EXISTS document_versions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    document_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    file_path TEXT,
                    content_hash TEXT,
                    mime_type TEXT,
                    file_size INTEGER,
                    fetched_at TEXT NOT NULL,
                    UNIQUE(document_id, version)
                )"#,
            )
            .execute(conn)?;

            diesel::sql_query(
                r#"CREATE TABLE IF NOT EXISTS document_pages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    document_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    page_number INTEGER NOT NULL,
                    text_content TEXT,
                    ocr_text TEXT,
                    has_images INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'pending',
                    UNIQUE(document_id, version, page_number)
                )"#,
            )
            .execute(conn)?;

            diesel::sql_query(
                r#"CREATE TABLE IF NOT EXISTS virtual_files (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    document_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    mime_type TEXT,
                    file_size INTEGER,
                    status TEXT NOT NULL DEFAULT 'pending',
                    ocr_text TEXT,
                    UNIQUE(document_id, version, path)
                )"#,
            )
            .execute(conn)?;

            Ok(())
        })
        .await
        .unwrap();

        (pool, dir)
    }

    #[tokio::test]
    async fn test_document_crud() {
        let (pool, dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool, dir.path().to_path_buf());

        // Create a document
        let doc = Document {
            id: "doc-1".to_string(),
            source_id: "test-source".to_string(),
            title: "Test Document".to_string(),
            source_url: "https://example.com/doc.pdf".to_string(),
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status: DocumentStatus::Pending,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        };

        // Save
        repo.save(&doc).await.unwrap();

        // Check exists
        assert!(repo.exists("doc-1").await.unwrap());
        assert!(!repo.exists("nonexistent").await.unwrap());

        // Get
        let fetched = repo.get("doc-1").await.unwrap().unwrap();
        assert_eq!(fetched.title, "Test Document");
        assert_eq!(fetched.source_url, "https://example.com/doc.pdf");

        // Update status
        repo.update_status("doc-1", DocumentStatus::Downloaded).await.unwrap();
        let updated = repo.get("doc-1").await.unwrap().unwrap();
        assert_eq!(updated.status, DocumentStatus::Downloaded);

        // Get by source
        let by_source = repo.get_by_source("test-source").await.unwrap();
        assert_eq!(by_source.len(), 1);

        // Count by source
        let count = repo.count_by_source("test-source").await.unwrap();
        assert_eq!(count, 1);

        // Delete
        let deleted = repo.delete("doc-1").await.unwrap();
        assert!(deleted);

        // Verify deleted
        assert!(!repo.exists("doc-1").await.unwrap());
    }

    #[tokio::test]
    async fn test_document_versions() {
        let (pool, dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool, dir.path().to_path_buf());

        // Create a document
        let doc = Document {
            id: "doc-2".to_string(),
            source_id: "test-source".to_string(),
            title: "Versioned Doc".to_string(),
            source_url: "https://example.com/versioned.pdf".to_string(),
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status: DocumentStatus::Pending,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        };
        repo.save(&doc).await.unwrap();

        // Add a version
        let version = DocumentVersion {
            id: 1,
            content_hash: "abc123".to_string(),
            file_path: PathBuf::from("/tmp/test.pdf"),
            file_size: 1024,
            mime_type: "application/pdf".to_string(),
            acquired_at: Utc::now(),
            source_url: None,
            original_filename: None,
            server_date: None,
            page_count: None,
        };
        repo.add_version("doc-2", &version).await.unwrap();

        // Get latest version
        let latest = repo.get_latest_version("doc-2").await.unwrap().unwrap();
        assert_eq!(latest.content_hash, "abc123");
        assert_eq!(latest.file_size, 1024);
    }
}
