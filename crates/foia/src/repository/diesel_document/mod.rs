//! Diesel-based document repository.
//!
//! Uses diesel-async for async database support. Works with both SQLite and PostgreSQL.
//!
//! This module is split into submodules by domain:
//! - `mod.rs` (this file): Core CRUD, virtual files, helpers
//! - `versions.rs`: Document version operations
//! - `pages.rs`: Document page and OCR operations
//! - `analysis.rs`: Analysis result storage and worker claims
//! - `counting.rs`: Basic document counting queries
//! - `stats.rs`: MIME type and category statistics
//! - `browsing.rs`: Browse, filter, and navigation queries
//! - `tags.rs`: Tag search and retrieval
//! - `timeline.rs`: Timeline bucketing queries
//! - `annotations.rs`: Metadata-based annotation queries
//! - `pipeline.rs`: Analysis pipeline queries (needing analysis, priority, finalization)
//! - `entities.rs`: Entity and spatial queries

mod analysis;
mod annotations;
mod browsing;
mod counting;
pub mod entities;
mod pages;
mod pipeline;
mod stats;
mod tags;
mod timeline;
mod versions;

pub use browsing::BrowseParams;

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::models::{DocumentRecord, DocumentVersionRecord, VirtualFileRecord};
use super::pool::{DbPool, DieselError};
use super::{parse_datetime, parse_datetime_opt};
use crate::models::{Document, DocumentStatus, DocumentVersion, VirtualFile, VirtualFileStatus};
use crate::schema::{document_versions, documents, virtual_files};
use crate::with_conn;

/// OCR result for a page.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OcrResult {
    pub backend: String,
    pub model: Option<String>,
    pub text: Option<String>,
    pub confidence: Option<f32>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Diesel-based document repository with compile-time query checking.
#[derive(Clone)]
pub struct DieselDocumentRepository {
    pub pool: DbPool,
}

const ARCHIVE_MIME_TYPES: [&str; 7] = [
    "application/zip",
    "application/x-zip",
    "application/x-zip-compressed",
    "application/x-tar",
    "application/gzip",
    "application/x-rar-compressed",
    "application/x-7z-compressed",
];

impl DieselDocumentRepository {
    /// Create a new Diesel document repository.
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    // ========================================================================
    // Core CRUD Operations
    // ========================================================================

    /// Convert document records to documents with batch-loaded versions.
    ///
    /// Single query for all versions instead of one query per document.
    async fn records_to_documents(
        &self,
        records: Vec<DocumentRecord>,
    ) -> Result<Vec<Document>, DieselError> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let doc_ids: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
        let mut versions_map = self.load_versions_batch(&doc_ids).await?;
        records
            .into_iter()
            .map(|record| {
                let versions = versions_map.remove(&record.id).unwrap_or_default();
                Self::record_to_document(record, versions)
            })
            .collect()
    }

    /// Get multiple documents by IDs in a single batch query.
    pub async fn get_batch(&self, ids: &[String]) -> Result<Vec<Document>, DieselError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::id.eq_any(ids))
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Get a document by ID.
    pub async fn get(&self, id: &str) -> Result<Option<Document>, DieselError> {
        let record: Option<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table.find(id).first(&mut conn).await.optional()
        })?;

        match record {
            Some(record) => {
                let versions = self.load_versions(&record.id).await?;
                Ok(Some(Self::record_to_document(record, versions)?))
            }
            None => Ok(None),
        }
    }

    /// Get all documents for a source.
    pub async fn get_by_source(&self, source_id: &str) -> Result<Vec<Document>, DieselError> {
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::source_id.eq(source_id))
                .order(documents::created_at.desc())
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Get documents by URL.
    pub async fn get_by_url(&self, url: &str) -> Result<Vec<Document>, DieselError> {
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::source_url.eq(url))
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Check if a document exists.
    #[allow(dead_code)]
    pub async fn exists(&self, id: &str) -> Result<bool, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let count: i64 = documents::table
                .filter(documents::id.eq(id))
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(count > 0)
        })
    }

    /// Save a document.
    ///
    /// This also computes and sets the category_id based on the document's
    /// current version's MIME type.
    pub async fn save(&self, doc: &Document) -> Result<(), DieselError> {
        let metadata = serde_json::to_string(&doc.metadata)
            .map_err(|e| diesel::result::Error::SerializationError(Box::new(e)))?;
        let created_at = doc.created_at.to_rfc3339();
        let updated_at = doc.updated_at.to_rfc3339();
        let status = doc.status.as_str().to_string();

        let category_id: Option<String> = doc.current_version().map(|v| {
            crate::utils::mime_type_category(&v.mime_type)
                .id()
                .to_string()
        });

        with_conn!(self.pool, conn, {
            diesel::insert_into(documents::table)
                .values((
                    documents::id.eq(&doc.id),
                    documents::source_id.eq(&doc.source_id),
                    documents::source_url.eq(&doc.source_url),
                    documents::title.eq(&doc.title),
                    documents::status.eq(&status),
                    documents::metadata.eq(&metadata),
                    documents::created_at.eq(&created_at),
                    documents::updated_at.eq(&updated_at),
                    documents::category_id.eq(category_id.as_deref()),
                ))
                .on_conflict(documents::id)
                .do_update()
                .set((
                    documents::source_id.eq(&doc.source_id),
                    documents::source_url.eq(&doc.source_url),
                    documents::title.eq(&doc.title),
                    documents::status.eq(&status),
                    documents::metadata.eq(&metadata),
                    documents::updated_at.eq(&updated_at),
                    documents::category_id.eq(category_id.as_deref()),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Save a document and persist any new versions (id == 0).
    ///
    /// Use this instead of `save()` when creating a new document or adding
    /// versions, so the version rows are actually written to document_versions.
    /// Use `save()` alone when only updating document metadata/status.
    pub async fn save_with_versions(&self, doc: &Document) -> Result<(), DieselError> {
        self.save(doc).await?;

        for version in &doc.versions {
            if version.id == 0 {
                self.add_version(&doc.id, version).await?;
            }
        }

        Ok(())
    }

    /// Delete a document.
    #[allow(dead_code)]
    pub async fn delete(&self, id: &str) -> Result<bool, DieselError> {
        use crate::schema::document_pages;
        use diesel_async::AsyncConnection;

        with_conn!(self.pool, conn, {
            conn.transaction(|conn| {
                Box::pin(async move {
                    diesel::delete(
                        document_versions::table.filter(document_versions::document_id.eq(id)),
                    )
                    .execute(conn)
                    .await?;
                    diesel::delete(
                        document_pages::table.filter(document_pages::document_id.eq(id)),
                    )
                    .execute(conn)
                    .await?;
                    diesel::delete(virtual_files::table.filter(virtual_files::document_id.eq(id)))
                        .execute(conn)
                        .await?;
                    let rows = diesel::delete(documents::table.find(id))
                        .execute(conn)
                        .await?;
                    Ok(rows > 0)
                })
            })
            .await
        })
    }

    /// Update document status.
    pub async fn update_status(&self, id: &str, status: DocumentStatus) -> Result<(), DieselError> {
        let status_str = status.as_str().to_string();
        let updated_at = Utc::now().to_rfc3339();

        with_conn!(self.pool, conn, {
            diesel::update(documents::table.find(id))
                .set((
                    documents::status.eq(&status_str),
                    documents::updated_at.eq(&updated_at),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Get all documents.
    pub async fn get_all(&self) -> Result<Vec<Document>, DieselError> {
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .order(documents::created_at.desc())
                .load(&mut conn)
                .await
        })?;

        let mut docs = Vec::with_capacity(records.len());
        for record in records {
            let versions = self.load_versions(&record.id).await?;
            docs.push(Self::record_to_document(record, versions)?);
        }
        Ok(docs)
    }

    /// Get all document URLs as a HashSet.
    ///
    /// Only includes documents that have at least one version row, since
    /// documents without versions are incomplete imports that should be retried.
    pub async fn get_all_urls_set(&self) -> Result<std::collections::HashSet<String>, DieselError> {
        with_conn!(self.pool, conn, {
            let urls: Vec<String> = documents::table
                .filter(diesel::dsl::exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id)),
                ))
                .select(documents::source_url)
                .load(&mut conn)
                .await?;
            Ok(urls.into_iter().collect())
        })
    }

    /// Get URLs by source.
    pub async fn get_urls_by_source(&self, source_id: &str) -> Result<Vec<String>, DieselError> {
        with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::source_id.eq(source_id))
                .select(documents::source_url)
                .load(&mut conn)
                .await
        })
    }

    // ========================================================================
    // Virtual File Operations
    // ========================================================================

    /// Insert virtual file.
    pub async fn insert_virtual_file(&self, vf: &VirtualFile) -> Result<(), DieselError> {
        let now = Utc::now().to_rfc3339();

        with_conn!(self.pool, conn, {
            diesel::insert_into(virtual_files::table)
                .values((
                    virtual_files::id.eq(&vf.id),
                    virtual_files::document_id.eq(&vf.document_id),
                    virtual_files::version_id.eq(vf.version_id as i32),
                    virtual_files::archive_path.eq(&vf.archive_path),
                    virtual_files::filename.eq(&vf.filename),
                    virtual_files::mime_type.eq(&vf.mime_type),
                    virtual_files::file_size.eq(vf.file_size as i32),
                    virtual_files::extracted_text.eq(&vf.extracted_text),
                    virtual_files::synopsis.eq(&vf.synopsis),
                    virtual_files::tags.eq(serde_json::to_string(&vf.tags)
                        .map_err(|e| diesel::result::Error::SerializationError(Box::new(e)))?
                        .as_str()),
                    virtual_files::status.eq(vf.status.as_str()),
                    virtual_files::created_at.eq(&now),
                    virtual_files::updated_at.eq(&now),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Get virtual files.
    pub async fn get_virtual_files(
        &self,
        document_id: &str,
        version: i32,
    ) -> Result<Vec<VirtualFile>, DieselError> {
        with_conn!(self.pool, conn, {
            virtual_files::table
                .filter(virtual_files::document_id.eq(document_id))
                .filter(virtual_files::version_id.eq(version))
                .load::<VirtualFileRecord>(&mut conn)
                .await
                .and_then(|records| {
                    records
                        .into_iter()
                        .map(Self::virtual_file_record_to_model)
                        .collect()
                })
        })
    }

    /// Count unprocessed archives.
    pub async fn count_unprocessed_archives(
        &self,
        source_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        use diesel::dsl::{count_star, exists};

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq_any(["pending", "downloaded"]))
                .filter(exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id))
                        .filter(document_versions::mime_type.eq_any(ARCHIVE_MIME_TYPES)),
                ))
                .select(count_star())
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            let count: i64 = query.first(&mut conn).await?;
            Ok(count as u64)
        })
    }

    /// Count unprocessed emails.
    pub async fn count_unprocessed_emails(
        &self,
        source_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        use diesel::dsl::{count_star, exists};

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq_any(["pending", "downloaded"]))
                .filter(exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id))
                        .filter(
                            document_versions::mime_type
                                .like("message/%")
                                .or(document_versions::mime_type.like("%rfc822%")),
                        ),
                ))
                .select(count_star())
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            let count: i64 = query.first(&mut conn).await?;
            Ok(count as u64)
        })
    }

    /// Get unprocessed archives.
    pub async fn get_unprocessed_archives(
        &self,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        use diesel::dsl::exists;

        let ids: Vec<String> = with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq_any(["pending", "downloaded"]))
                .filter(exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id))
                        .filter(document_versions::mime_type.eq_any(ARCHIVE_MIME_TYPES)),
                ))
                .select(documents::id)
                .order(documents::updated_at.asc())
                .limit(limit as i64)
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            query.load(&mut conn).await
        })?;

        let mut docs = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Ok(Some(doc)) = self.get(id).await {
                docs.push(doc);
            }
        }
        Ok(docs)
    }

    /// Get unprocessed emails.
    pub async fn get_unprocessed_emails(
        &self,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        use diesel::dsl::exists;

        let ids: Vec<String> = with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq_any(["pending", "downloaded"]))
                .filter(exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id))
                        .filter(
                            document_versions::mime_type
                                .like("message/%")
                                .or(document_versions::mime_type.like("%rfc822%")),
                        ),
                ))
                .select(documents::id)
                .order(documents::updated_at.asc())
                .limit(limit as i64)
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            query.load(&mut conn).await
        })?;

        let mut docs = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Ok(Some(doc)) = self.get(id).await {
                docs.push(doc);
            }
        }
        Ok(docs)
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    pub(crate) fn record_to_document(
        record: DocumentRecord,
        versions: Vec<DocumentVersion>,
    ) -> Result<Document, DieselError> {
        let tags = match record.tags {
            Some(ref s) => serde_json::from_str(s).map_err(|e| {
                diesel::result::Error::DeserializationError(
                    format!("Invalid tags JSON for document '{}': {}", record.id, e).into(),
                )
            })?,
            None => Vec::new(),
        };
        let status = DocumentStatus::from_str(&record.status).ok_or_else(|| {
            diesel::result::Error::DeserializationError(
                format!(
                    "Invalid document status '{}' for document '{}'",
                    record.status, record.id
                )
                .into(),
            )
        })?;
        let metadata = serde_json::from_str(&record.metadata).map_err(|e| {
            diesel::result::Error::DeserializationError(
                format!("Invalid metadata JSON for document '{}': {}", record.id, e).into(),
            )
        })?;

        Ok(Document {
            id: record.id,
            source_id: record.source_id,
            title: record.title,
            source_url: record.source_url,
            extracted_text: record.extracted_text,
            synopsis: record.synopsis,
            tags,
            status,
            metadata,
            created_at: parse_datetime(&record.created_at),
            updated_at: parse_datetime(&record.updated_at),
            discovery_method: record.discovery_method,
            versions,
        })
    }

    pub(crate) fn version_record_to_model(record: DocumentVersionRecord) -> DocumentVersion {
        DocumentVersion {
            id: record.id as i64,
            content_hash: record.content_hash,
            content_hash_blake3: record.content_hash_blake3,
            file_path: record.file_path.map(PathBuf::from),
            file_size: record.file_size as u64,
            mime_type: record.mime_type,
            acquired_at: parse_datetime(&record.acquired_at),
            source_url: record.source_url,
            original_filename: record.original_filename,
            server_date: parse_datetime_opt(record.server_date),
            page_count: record.page_count.map(|c| c as u32),
            archive_snapshot_id: record.archive_snapshot_id,
            earliest_archived_at: parse_datetime_opt(record.earliest_archived_at),
            dedup_index: record.dedup_index.map(|i| i as u32),
        }
    }

    fn virtual_file_record_to_model(record: VirtualFileRecord) -> Result<VirtualFile, DieselError> {
        let tags = match record.tags {
            Some(ref s) => serde_json::from_str(s).map_err(|e| {
                diesel::result::Error::DeserializationError(
                    format!("Invalid tags JSON for virtual file '{}': {}", record.id, e).into(),
                )
            })?,
            None => Vec::new(),
        };
        let status = VirtualFileStatus::from_str(&record.status).ok_or_else(|| {
            diesel::result::Error::DeserializationError(
                format!(
                    "Invalid virtual file status '{}' for file '{}'",
                    record.status, record.id
                )
                .into(),
            )
        })?;

        Ok(VirtualFile {
            id: record.id,
            document_id: record.document_id,
            version_id: record.version_id as i64,
            archive_path: record.archive_path,
            filename: record.filename,
            file_size: record.file_size as u64,
            mime_type: record.mime_type,
            extracted_text: record.extracted_text,
            synopsis: record.synopsis,
            tags,
            status,
            created_at: parse_datetime(&record.created_at),
            updated_at: parse_datetime(&record.updated_at),
        })
    }
}

// Helper structs for SQL queries
#[derive(diesel::QueryableByName)]
pub(crate) struct MimeCount {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub mime_type: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub count: i64,
}

#[derive(diesel::QueryableByName)]
pub(crate) struct TagRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub tag: String,
}

#[derive(diesel::QueryableByName)]
pub struct DocIdRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub id: String,
}

#[derive(diesel::QueryableByName)]
#[allow(dead_code)]
pub(crate) struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub count: i64,
}

/// Lightweight browse result that excludes large text fields.
/// Used for document listing pages to avoid loading extracted_text.
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct BrowseRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub id: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub title: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub source_id: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub synopsis: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub tags: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub original_filename: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub mime_type: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub file_size: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub acquired_at: String,
}

#[derive(diesel::QueryableByName)]
pub(crate) struct ReturningId {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub id: i32,
}

#[cfg(test)]
mod tests {
    use super::super::pool::SqlitePool;
    use super::*;
    use diesel_async::SimpleAsyncConnection;
    use tempfile::tempdir;

    pub(crate) async fn setup_test_db() -> (DbPool, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let sqlite_pool = SqlitePool::from_path(&db_path);
        let mut conn = sqlite_pool.get().await.unwrap();

        conn.batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY,
                source_id TEXT NOT NULL,
                title TEXT NOT NULL,
                source_url TEXT NOT NULL,
                extracted_text TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                metadata TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                synopsis TEXT,
                tags TEXT,
                estimated_date TEXT,
                date_confidence TEXT,
                date_source TEXT,
                manual_date TEXT,
                discovery_method TEXT NOT NULL DEFAULT 'import',
                category_id TEXT,
                analysis_priority INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS document_versions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                content_hash_blake3 TEXT,
                file_path TEXT,
                file_size INTEGER NOT NULL,
                mime_type TEXT NOT NULL,
                acquired_at TEXT NOT NULL,
                source_url TEXT,
                original_filename TEXT,
                server_date TEXT,
                page_count INTEGER,
                archive_snapshot_id INTEGER,
                earliest_archived_at TEXT,
                dedup_index INTEGER
            );

            CREATE TABLE IF NOT EXISTS document_pages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL,
                version_id INTEGER NOT NULL,
                page_number INTEGER NOT NULL,
                search_text TEXT,
                ocr_status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(document_id, version_id, page_number)
            );

            CREATE TABLE IF NOT EXISTS virtual_files (
                id TEXT PRIMARY KEY,
                document_id TEXT NOT NULL,
                version_id INTEGER NOT NULL,
                archive_path TEXT NOT NULL,
                filename TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                extracted_text TEXT,
                synopsis TEXT,
                tags TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS page_ocr_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                page_id INTEGER NOT NULL,
                backend TEXT NOT NULL,
                text TEXT,
                confidence REAL,
                quality_score REAL,
                char_count INTEGER,
                word_count INTEGER,
                processing_time_ms INTEGER,
                error_message TEXT,
                created_at TEXT NOT NULL,
                model TEXT,
                image_hash TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_page_ocr_results_unique
                ON page_ocr_results(page_id, backend, COALESCE(model, ''));

            CREATE TABLE IF NOT EXISTS document_analysis_results (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                page_id INTEGER,
                document_id TEXT NOT NULL,
                version_id INTEGER NOT NULL,
                analysis_type TEXT NOT NULL,
                backend TEXT NOT NULL,
                result_text TEXT,
                confidence REAL,
                processing_time_ms INTEGER,
                error TEXT,
                status TEXT NOT NULL DEFAULT 'complete',
                created_at TEXT NOT NULL,
                metadata TEXT,
                model TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_analysis_results_page_unique
                ON document_analysis_results(page_id, analysis_type, backend, COALESCE(model, ''))
                WHERE page_id IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_analysis_results_doc_unique
                ON document_analysis_results(document_id, version_id, analysis_type, backend, COALESCE(model, ''))
                WHERE page_id IS NULL;

            CREATE TABLE IF NOT EXISTS document_entities (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id TEXT NOT NULL,
                entity_type TEXT NOT NULL,
                entity_text TEXT NOT NULL,
                normalized_text TEXT NOT NULL,
                latitude REAL,
                longitude REAL,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_de_type_text_doc
                ON document_entities(entity_type, normalized_text, document_id);
            "#,
        )
        .await
        .unwrap();

        (DbPool::Sqlite(sqlite_pool), dir)
    }

    #[tokio::test]
    async fn test_document_crud() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

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

        repo.save(&doc).await.unwrap();
        assert!(repo.exists("doc-1").await.unwrap());

        let fetched = repo.get("doc-1").await.unwrap().unwrap();
        assert_eq!(fetched.title, "Test Document");

        repo.update_status("doc-1", DocumentStatus::Downloaded)
            .await
            .unwrap();
        let updated = repo.get("doc-1").await.unwrap().unwrap();
        assert_eq!(updated.status, DocumentStatus::Downloaded);

        let deleted = repo.delete("doc-1").await.unwrap();
        assert!(deleted);
        assert!(!repo.exists("doc-1").await.unwrap());
    }

    #[tokio::test]
    async fn test_document_versions() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

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

        let version = DocumentVersion {
            id: 1,
            content_hash: "abc123".to_string(),
            content_hash_blake3: Some("def456".to_string()),
            file_path: None,
            file_size: 1024,
            mime_type: "application/pdf".to_string(),
            acquired_at: Utc::now(),
            source_url: None,
            original_filename: None,
            server_date: None,
            page_count: None,
            archive_snapshot_id: None,
            earliest_archived_at: None,
            dedup_index: None,
        };
        repo.add_version("doc-2", &version).await.unwrap();

        let latest = repo.get_latest_version("doc-2").await.unwrap().unwrap();
        assert_eq!(latest.content_hash, "abc123");
        assert_eq!(latest.file_size, 1024);
    }

    #[tokio::test]
    async fn test_count_unprocessed_archives_with_sql_metacharacters() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo
            .count_unprocessed_archives(Some("'; DROP TABLE documents; --"))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_count_unprocessed_emails_with_sql_metacharacters() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo
            .count_unprocessed_emails(Some("'; DROP TABLE documents; --"))
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    fn make_doc(id: &str, source: &str, status: DocumentStatus) -> Document {
        Document {
            id: id.to_string(),
            source_id: source.to_string(),
            title: format!("Doc {id}"),
            source_url: format!("https://example.com/{id}"),
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        }
    }

    fn make_version(hash: &str, mime_type: &str) -> DocumentVersion {
        DocumentVersion {
            id: 0,
            content_hash: hash.to_string(),
            content_hash_blake3: None,
            file_path: None,
            file_size: 1024,
            mime_type: mime_type.to_string(),
            acquired_at: Utc::now(),
            source_url: None,
            original_filename: None,
            server_date: None,
            page_count: None,
            archive_snapshot_id: None,
            earliest_archived_at: None,
            dedup_index: None,
        }
    }

    async fn save_doc_with_version(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        status: DocumentStatus,
        mime_type: &str,
    ) {
        let doc = make_doc(id, source, status);
        repo.save(&doc).await.unwrap();
        let version = make_version(&format!("hash-{id}"), mime_type);
        repo.add_version(id, &version).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_batch() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src", DocumentStatus::Pending, "application/pdf")
            .await;
        save_doc_with_version(&repo, "d2", "src", DocumentStatus::Pending, "application/pdf")
            .await;
        save_doc_with_version(&repo, "d3", "src", DocumentStatus::Pending, "image/png").await;

        let batch = repo
            .get_batch(&["d1".into(), "d3".into()])
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
        let ids: Vec<&str> = batch.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"d1"));
        assert!(ids.contains(&"d3"));

        let empty = repo.get_batch(&[]).await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn test_get_by_source() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src-a", DocumentStatus::Pending, "application/pdf")
            .await;
        save_doc_with_version(&repo, "d2", "src-a", DocumentStatus::Pending, "application/pdf")
            .await;
        save_doc_with_version(&repo, "d3", "src-b", DocumentStatus::Pending, "image/png").await;

        let src_a = repo.get_by_source("src-a").await.unwrap();
        assert_eq!(src_a.len(), 2);
        assert!(src_a.iter().all(|d| d.source_id == "src-a"));

        let src_b = repo.get_by_source("src-b").await.unwrap();
        assert_eq!(src_b.len(), 1);
        assert_eq!(src_b[0].id, "d3");

        let none = repo.get_by_source("nonexistent").await.unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn test_get_by_url() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let mut doc1 = make_doc("d1", "src", DocumentStatus::Pending);
        doc1.source_url = "https://example.com/shared".to_string();
        repo.save(&doc1).await.unwrap();

        let mut doc2 = make_doc("d2", "src", DocumentStatus::Pending);
        doc2.source_url = "https://example.com/shared".to_string();
        repo.save(&doc2).await.unwrap();

        let mut doc3 = make_doc("d3", "src", DocumentStatus::Pending);
        doc3.source_url = "https://example.com/other".to_string();
        repo.save(&doc3).await.unwrap();

        let shared = repo
            .get_by_url("https://example.com/shared")
            .await
            .unwrap();
        assert_eq!(shared.len(), 2);

        let other = repo
            .get_by_url("https://example.com/other")
            .await
            .unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].id, "d3");

        let missing = repo.get_by_url("https://nope.com").await.unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn test_get_all() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        for i in 1..=3 {
            let id = format!("d{i}");
            let mut doc = make_doc(&id, "src", DocumentStatus::Pending);
            doc.created_at = Utc::now() + chrono::Duration::seconds(i);
            repo.save(&doc).await.unwrap();
        }

        let all = repo.get_all().await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "d3");
        assert_eq!(all[2].id, "d1");
    }

    #[tokio::test]
    async fn test_get_all_urls_set() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src", DocumentStatus::Pending, "application/pdf")
            .await;
        save_doc_with_version(&repo, "d2", "src", DocumentStatus::Pending, "image/png").await;

        let doc_no_version = make_doc("d3", "src", DocumentStatus::Pending);
        repo.save(&doc_no_version).await.unwrap();

        let urls = repo.get_all_urls_set().await.unwrap();
        assert_eq!(urls.len(), 2);
        assert!(urls.contains("https://example.com/d1"));
        assert!(urls.contains("https://example.com/d2"));
        assert!(!urls.contains("https://example.com/d3"));
    }

    #[tokio::test]
    async fn test_get_urls_by_source() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src-a", DocumentStatus::Pending))
            .await
            .unwrap();
        repo.save(&make_doc("d2", "src-a", DocumentStatus::Pending))
            .await
            .unwrap();
        repo.save(&make_doc("d3", "src-b", DocumentStatus::Pending))
            .await
            .unwrap();

        let urls_a = repo.get_urls_by_source("src-a").await.unwrap();
        assert_eq!(urls_a.len(), 2);
        assert!(urls_a.contains(&"https://example.com/d1".to_string()));
        assert!(urls_a.contains(&"https://example.com/d2".to_string()));

        let urls_b = repo.get_urls_by_source("src-b").await.unwrap();
        assert_eq!(urls_b.len(), 1);
    }

    #[tokio::test]
    async fn test_save_with_versions() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let mut doc = make_doc("d1", "src", DocumentStatus::Pending);
        doc.versions.push(make_version("hash-v1", "application/pdf"));

        repo.save_with_versions(&doc).await.unwrap();

        let fetched = repo.get("d1").await.unwrap().unwrap();
        assert_eq!(fetched.versions.len(), 1);
        assert_eq!(fetched.versions[0].content_hash, "hash-v1");
        assert_eq!(fetched.versions[0].mime_type, "application/pdf");
    }

    #[tokio::test]
    async fn test_virtual_files() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src", DocumentStatus::Pending, "application/zip")
            .await;

        let vf1 = VirtualFile {
            id: "vf-1".to_string(),
            document_id: "d1".to_string(),
            version_id: 1,
            archive_path: "archive/path1.txt".to_string(),
            filename: "file1.txt".to_string(),
            file_size: 100,
            mime_type: "text/plain".to_string(),
            extracted_text: Some("hello world".to_string()),
            synopsis: None,
            tags: vec!["tag1".to_string()],
            status: VirtualFileStatus::Pending,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let vf2 = VirtualFile {
            id: "vf-2".to_string(),
            document_id: "d1".to_string(),
            version_id: 1,
            archive_path: "archive/path2.pdf".to_string(),
            filename: "file2.pdf".to_string(),
            file_size: 2048,
            mime_type: "application/pdf".to_string(),
            extracted_text: None,
            synopsis: Some("A PDF".to_string()),
            tags: vec![],
            status: VirtualFileStatus::OcrComplete,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        repo.insert_virtual_file(&vf1).await.unwrap();
        repo.insert_virtual_file(&vf2).await.unwrap();

        let files = repo.get_virtual_files("d1", 1).await.unwrap();
        assert_eq!(files.len(), 2);

        let f1 = files.iter().find(|f| f.id == "vf-1").unwrap();
        assert_eq!(f1.filename, "file1.txt");
        assert_eq!(f1.extracted_text.as_deref(), Some("hello world"));
        assert_eq!(f1.tags, vec!["tag1".to_string()]);
        assert_eq!(f1.status, VirtualFileStatus::Pending);

        let f2 = files.iter().find(|f| f.id == "vf-2").unwrap();
        assert_eq!(f2.synopsis.as_deref(), Some("A PDF"));
        assert_eq!(f2.status, VirtualFileStatus::OcrComplete);

        let empty = repo.get_virtual_files("d1", 999).await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn test_count_unprocessed_archives() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(
            &repo, "d1", "src-a", DocumentStatus::Pending, "application/zip",
        )
        .await;
        save_doc_with_version(
            &repo, "d2", "src-a", DocumentStatus::Downloaded, "application/x-tar",
        )
        .await;
        save_doc_with_version(
            &repo, "d3", "src-b", DocumentStatus::Pending, "application/zip",
        )
        .await;
        save_doc_with_version(
            &repo, "d4", "src-a", DocumentStatus::Pending, "application/pdf",
        )
        .await;
        save_doc_with_version(
            &repo,
            "d5",
            "src-a",
            DocumentStatus::OcrComplete,
            "application/zip",
        )
        .await;

        let all = repo.count_unprocessed_archives(None).await.unwrap();
        assert_eq!(all, 3);

        let src_a = repo
            .count_unprocessed_archives(Some("src-a"))
            .await
            .unwrap();
        assert_eq!(src_a, 2);

        let src_b = repo
            .count_unprocessed_archives(Some("src-b"))
            .await
            .unwrap();
        assert_eq!(src_b, 1);
    }

    #[tokio::test]
    async fn test_get_unprocessed_archives() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(
            &repo, "d1", "src", DocumentStatus::Pending, "application/zip",
        )
        .await;
        save_doc_with_version(
            &repo, "d2", "src", DocumentStatus::Pending, "application/zip",
        )
        .await;
        save_doc_with_version(
            &repo, "d3", "src", DocumentStatus::Pending, "application/zip",
        )
        .await;

        let limited = repo.get_unprocessed_archives(None, 2).await.unwrap();
        assert_eq!(limited.len(), 2);

        let all = repo.get_unprocessed_archives(None, 100).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_count_unprocessed_emails() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(
            &repo, "d1", "src-a", DocumentStatus::Pending, "message/rfc822",
        )
        .await;
        save_doc_with_version(
            &repo, "d2", "src-a", DocumentStatus::Downloaded, "application/x-rfc822",
        )
        .await;
        save_doc_with_version(
            &repo, "d3", "src-b", DocumentStatus::Pending, "message/rfc822",
        )
        .await;
        save_doc_with_version(
            &repo, "d4", "src-a", DocumentStatus::Pending, "application/pdf",
        )
        .await;

        let all = repo.count_unprocessed_emails(None).await.unwrap();
        assert_eq!(all, 3);

        let src_a = repo
            .count_unprocessed_emails(Some("src-a"))
            .await
            .unwrap();
        assert_eq!(src_a, 2);
    }

    #[tokio::test]
    async fn test_get_unprocessed_emails() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(
            &repo, "d1", "src", DocumentStatus::Pending, "message/rfc822",
        )
        .await;
        save_doc_with_version(
            &repo, "d2", "src", DocumentStatus::Pending, "message/rfc822",
        )
        .await;

        let emails = repo.get_unprocessed_emails(None, 10).await.unwrap();
        assert_eq!(emails.len(), 2);

        let limited = repo.get_unprocessed_emails(None, 1).await.unwrap();
        assert_eq!(limited.len(), 1);
    }
}
