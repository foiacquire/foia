//! Document version operations.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, ReturningId};
use foia_models::DocumentVersion;
use crate::models::DocumentVersionRecord;
use crate::pool::DieselError;
use crate::schema::document_versions;
use crate::with_conn;

impl DieselDocumentRepository {
    /// Load versions for a document.
    pub(crate) async fn load_versions(
        &self,
        document_id: &str,
    ) -> Result<Vec<DocumentVersion>, DieselError> {
        with_conn!(self.pool, conn, {
            document_versions::table
                .filter(document_versions::document_id.eq(document_id))
                .order(document_versions::id.desc())
                .load::<DocumentVersionRecord>(&mut conn)
                .await
                .map(|records| {
                    records
                        .into_iter()
                        .map(Self::version_record_to_model)
                        .collect()
                })
        })
    }

    /// Load versions for multiple documents in a single query.
    /// Returns a map of document_id -> versions.
    pub(crate) async fn load_versions_batch(
        &self,
        document_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<DocumentVersion>>, DieselError> {
        if document_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let records: Vec<DocumentVersionRecord> = with_conn!(self.pool, conn, {
            document_versions::table
                .filter(document_versions::document_id.eq_any(document_ids))
                .order((document_versions::document_id, document_versions::id.desc()))
                .load(&mut conn)
                .await
        })?;

        let mut result: std::collections::HashMap<String, Vec<DocumentVersion>> =
            std::collections::HashMap::new();
        for record in records {
            let doc_id = record.document_id.clone();
            let version = Self::version_record_to_model(record);
            result.entry(doc_id).or_default().push(version);
        }
        Ok(result)
    }

    /// Add a new version.
    pub async fn add_version(
        &self,
        document_id: &str,
        version: &DocumentVersion,
    ) -> Result<i64, DieselError> {
        use crate::pool::build_sql;
        use crate::sea_tables::DocumentVersions;
        use sea_query::Query;

        let file_path = version
            .file_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());
        let acquired_at = version.acquired_at.to_rfc3339();
        let file_size = version.file_size as i32;
        let dedup_index = version.dedup_index.map(|i| i as i32);
        let server_date = version.server_date.map(|d| d.to_rfc3339());
        let page_count = version.page_count.map(|c| c as i32);
        let earliest_archived_at = version.earliest_archived_at.map(|d| d.to_rfc3339());

        let stmt = Query::insert()
            .into_table(DocumentVersions::Table)
            .columns([
                DocumentVersions::DocumentId,
                DocumentVersions::ContentHash,
                DocumentVersions::ContentHashBlake3,
                DocumentVersions::FilePath,
                DocumentVersions::FileSize,
                DocumentVersions::MimeType,
                DocumentVersions::AcquiredAt,
                DocumentVersions::SourceUrl,
                DocumentVersions::OriginalFilename,
                DocumentVersions::ServerDate,
                DocumentVersions::PageCount,
                DocumentVersions::ArchiveSnapshotId,
                DocumentVersions::EarliestArchivedAt,
                DocumentVersions::DedupIndex,
            ])
            .values_panic([
                document_id.to_string().into(),
                version.content_hash.clone().into(),
                version.content_hash_blake3.clone().into(),
                file_path.clone().into(),
                file_size.into(),
                version.mime_type.clone().into(),
                acquired_at.clone().into(),
                version.source_url.clone().into(),
                version.original_filename.clone().into(),
                server_date.clone().into(),
                page_count.into(),
                version.archive_snapshot_id.into(),
                earliest_archived_at.clone().into(),
                dedup_index.into(),
            ])
            .returning_col(DocumentVersions::Id)
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            let result: ReturningId = diesel::sql_query(&sql)
                .bind::<diesel::sql_types::Text, _>(document_id)
                .bind::<diesel::sql_types::Text, _>(&version.content_hash)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    version.content_hash_blake3.as_deref(),
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    file_path.as_deref(),
                )
                .bind::<diesel::sql_types::Integer, _>(file_size)
                .bind::<diesel::sql_types::Text, _>(&version.mime_type)
                .bind::<diesel::sql_types::Text, _>(&acquired_at)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    version.source_url.as_deref(),
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    version.original_filename.as_deref(),
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    server_date.as_deref(),
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(page_count)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(
                    version.archive_snapshot_id,
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(
                    earliest_archived_at.as_deref(),
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(dedup_index)
                .get_result(&mut conn)
                .await?;
            Ok(result.id as i64)
        })
    }

    /// Get latest version.
    #[allow(dead_code)]
    pub async fn get_latest_version(
        &self,
        document_id: &str,
    ) -> Result<Option<DocumentVersion>, DieselError> {
        with_conn!(self.pool, conn, {
            document_versions::table
                .filter(document_versions::document_id.eq(document_id))
                .order(document_versions::id.desc())
                .first::<DocumentVersionRecord>(&mut conn)
                .await
                .optional()
                .map(|opt| opt.map(Self::version_record_to_model))
        })
    }

    /// Get current version ID.
    pub async fn get_current_version_id(
        &self,
        document_id: &str,
    ) -> Result<Option<i64>, DieselError> {
        with_conn!(self.pool, conn, {
            let version: Option<i32> = document_versions::table
                .filter(document_versions::document_id.eq(document_id))
                .order(document_versions::id.desc())
                .select(document_versions::id)
                .first(&mut conn)
                .await
                .optional()?;
            Ok(version.map(|v| v as i64))
        })
    }

    /// Update version mime type.
    pub async fn update_version_mime_type(
        &self,
        version_id: i64,
        mime_type: &str,
    ) -> Result<(), DieselError> {
        with_conn!(self.pool, conn, {
            diesel::update(document_versions::table.find(version_id as i32))
                .set(document_versions::mime_type.eq(mime_type))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Set version page count.
    /// Note: page_count is not stored in the database schema, so this is a no-op.
    /// The count can be derived from document_pages table.
    pub async fn set_version_page_count(
        &self,
        _version_id: i64,
        _count: u32,
    ) -> Result<(), DieselError> {
        // Page count is derived from document_pages, not stored directly
        Ok(())
    }

    /// Find an existing file by dual hash and size for deduplication.
    ///
    /// Returns the file_path if a matching file already exists, allowing
    /// the caller to skip writing a duplicate file to disk.
    ///
    /// Uses SHA-256 + BLAKE3 + file_size for collision-resistant matching.
    pub async fn find_existing_file(
        &self,
        sha256_hash: &str,
        blake3_hash: &str,
        file_size: i64,
    ) -> Result<Option<String>, DieselError> {
        with_conn!(self.pool, conn, {
            document_versions::table
                .filter(document_versions::content_hash.eq(sha256_hash))
                .filter(document_versions::content_hash_blake3.eq(blake3_hash))
                .filter(document_versions::file_size.eq(file_size as i32))
                .select(document_versions::file_path)
                .first::<Option<String>>(&mut conn)
                .await
                .optional()
                .map(|opt| opt.flatten())
        })
    }

    /// Clear the stored file_path (migrate to deterministic) and set dedup_index.
    pub async fn clear_version_file_path(
        &self,
        version_id: i64,
        dedup_index: Option<i32>,
    ) -> Result<(), DieselError> {
        with_conn!(self.pool, conn, {
            diesel::update(document_versions::table.find(version_id as i32))
                .set((
                    document_versions::file_path.eq(None::<String>),
                    document_versions::dedup_index.eq(dedup_index),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Count versions with non-null file_path.
    pub async fn count_legacy_file_paths(&self) -> Result<u64, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let n: i64 = document_versions::table
                .filter(document_versions::file_path.is_not_null())
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(n as u64)
        })
    }

    /// Clear file_path for multiple versions at once.
    pub async fn clear_version_file_paths_batch(
        &self,
        version_ids: &[i32],
    ) -> Result<usize, DieselError> {
        if version_ids.is_empty() {
            return Ok(0);
        }
        with_conn!(self.pool, conn, {
            diesel::update(
                document_versions::table.filter(document_versions::id.eq_any(version_ids)),
            )
            .set(document_versions::file_path.eq(None::<String>))
            .execute(&mut conn)
            .await
        })
    }

    /// Get versions with non-null file_path for legacy migration.
    /// Returns (version_id, document_id, file_path, source_url, title, version) tuples
    /// in batches using cursor pagination on version id.
    pub async fn get_legacy_file_path_versions(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<(DocumentVersion, String, String)>, DieselError> {
        use crate::schema::documents;

        let records: Vec<(DocumentVersionRecord, String, String)> = with_conn!(self.pool, conn, {
            document_versions::table
                .inner_join(documents::table)
                .filter(document_versions::file_path.is_not_null())
                .filter(document_versions::id.gt(after_id as i32))
                .order(document_versions::id.asc())
                .limit(limit as i64)
                .select((
                    DocumentVersionRecord::as_select(),
                    documents::source_url,
                    documents::title,
                ))
                .load(&mut conn)
                .await
        })?;

        Ok(records
            .into_iter()
            .map(|(rec, url, title)| (Self::version_record_to_model(rec), url, title))
            .collect())
    }

    /// Get all content hashes for duplicate detection.
    /// Returns (doc_id, source_id, content_hash, title) tuples
    ///
    /// Uses sea-query for the correlated subquery that Diesel DSL cannot express
    /// (same-table `MAX(id) WHERE document_id = dv.document_id`).
    pub async fn get_content_hashes(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, DieselError> {
        use crate::pool::build_sql;
        use crate::sea_tables::{DocumentVersions, Documents};
        use sea_query::{Alias, Expr, Query};

        #[derive(diesel::QueryableByName)]
        struct HashRow {
            #[diesel(sql_type = diesel::sql_types::Text)]
            document_id: String,
            #[diesel(sql_type = diesel::sql_types::Text)]
            source_id: String,
            #[diesel(sql_type = diesel::sql_types::Text)]
            content_hash: String,
            #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
            title: Option<String>,
        }

        let dv2 = Alias::new("dv2");
        let max_version = Query::select()
            .expr(Expr::col((dv2.clone(), DocumentVersions::Id)).max())
            .from_as(DocumentVersions::Table, dv2.clone())
            .and_where(
                Expr::col((dv2, DocumentVersions::DocumentId))
                    .equals((DocumentVersions::Table, DocumentVersions::DocumentId)),
            )
            .to_owned();

        let stmt = Query::select()
            .column((DocumentVersions::Table, DocumentVersions::DocumentId))
            .column((Documents::Table, Documents::SourceId))
            .column((DocumentVersions::Table, DocumentVersions::ContentHash))
            .column((Documents::Table, Documents::Title))
            .from(DocumentVersions::Table)
            .join(
                sea_query::JoinType::Join,
                Documents::Table,
                Expr::col((DocumentVersions::Table, DocumentVersions::DocumentId))
                    .equals((Documents::Table, Documents::Id)),
            )
            .and_where(
                Expr::col((DocumentVersions::Table, DocumentVersions::ContentHash)).is_not_null(),
            )
            .and_where(
                Expr::col((DocumentVersions::Table, DocumentVersions::Id))
                    .in_subquery(max_version),
            )
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        let results: Vec<HashRow> = with_conn!(self.pool, conn, {
            diesel::sql_query(&sql).load(&mut conn).await
        })?;

        Ok(results
            .into_iter()
            .map(|r| {
                (
                    r.document_id,
                    r.source_id,
                    r.content_hash,
                    r.title.unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Find documents by content hash.
    /// Returns (source_id, document_id, title) tuples
    pub async fn find_sources_by_hash(
        &self,
        content_hash: &str,
        exclude_source: Option<&str>,
    ) -> Result<Vec<(String, String, String)>, DieselError> {
        use crate::schema::documents;

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .inner_join(document_versions::table)
                .filter(document_versions::content_hash.eq(content_hash))
                .select((documents::source_id, documents::id, documents::title))
                .into_boxed();

            if let Some(exclude) = exclude_source {
                query = query.filter(documents::source_id.ne(exclude));
            }

            query.load::<(String, String, String)>(&mut conn).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diesel_document::tests::setup_test_db;

    use foia_models::{Document, DocumentStatus, DocumentVersion};
    use chrono::Utc;

    fn make_doc(id: &str, source: &str) -> Document {
        Document {
            id: id.to_string(),
            source_id: source.to_string(),
            title: format!("Doc {id}"),
            source_url: format!("https://example.com/{id}"),
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status: DocumentStatus::Pending,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        }
    }

    fn make_version(hash: &str, mime: &str) -> DocumentVersion {
        DocumentVersion {
            id: 0,
            content_hash: hash.to_string(),
            content_hash_blake3: Some(format!("b3-{hash}")),
            file_path: None,
            file_size: 1024,
            mime_type: mime.to_string(),
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

    async fn save_with_version(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        hash: &str,
        mime: &str,
    ) -> i64 {
        repo.save(&make_doc(id, source)).await.unwrap();
        repo.add_version(id, &make_version(hash, mime))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_find_sources_by_hash_with_sql_metacharacters() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo
            .find_sources_by_hash("'; DROP TABLE documents; --", Some("' OR '1'='1"))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_current_version_id() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let v1 = repo
            .add_version("d1", &make_version("h1", "application/pdf"))
            .await
            .unwrap();
        let v2 = repo
            .add_version("d1", &make_version("h2", "application/pdf"))
            .await
            .unwrap();
        assert!(v2 > v1);

        let current = repo.get_current_version_id("d1").await.unwrap();
        assert_eq!(current, Some(v2));

        let missing = repo.get_current_version_id("nonexistent").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn test_update_version_mime_type() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let vid = save_with_version(&repo, "d1", "src", "h1", "application/pdf");

        repo.update_version_mime_type(vid.await, "image/tiff")
            .await
            .unwrap();

        let latest = repo.get_latest_version("d1").await.unwrap().unwrap();
        assert_eq!(latest.mime_type, "image/tiff");
    }

    #[tokio::test]
    async fn test_find_existing_file() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let mut v = make_version("sha256-abc", "application/pdf");
        v.content_hash_blake3 = Some("blake3-abc".to_string());
        v.file_size = 2048;
        v.file_path = Some(std::path::PathBuf::from("/data/files/abc.pdf"));
        repo.add_version("d1", &v).await.unwrap();

        let found = repo
            .find_existing_file("sha256-abc", "blake3-abc", 2048)
            .await
            .unwrap();
        assert_eq!(found, Some("/data/files/abc.pdf".to_string()));

        let wrong_hash = repo
            .find_existing_file("sha256-wrong", "blake3-abc", 2048)
            .await
            .unwrap();
        assert_eq!(wrong_hash, None);

        let wrong_size = repo
            .find_existing_file("sha256-abc", "blake3-abc", 9999)
            .await
            .unwrap();
        assert_eq!(wrong_size, None);
    }

    #[tokio::test]
    async fn test_clear_version_file_path() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let mut v = make_version("h1", "application/pdf");
        v.file_path = Some(std::path::PathBuf::from("/old/path.pdf"));
        let vid = repo.add_version("d1", &v).await.unwrap();

        repo.clear_version_file_path(vid, Some(42)).await.unwrap();

        let latest = repo.get_latest_version("d1").await.unwrap().unwrap();
        assert!(latest.file_path.is_none());
        assert_eq!(latest.dedup_index, Some(42));
    }

    #[tokio::test]
    async fn test_clear_version_file_paths_batch() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let mut v1 = make_version("h1", "application/pdf");
        v1.file_path = Some(std::path::PathBuf::from("/path1"));
        let vid1 = repo.add_version("d1", &v1).await.unwrap();

        let mut v2 = make_version("h2", "application/pdf");
        v2.file_path = Some(std::path::PathBuf::from("/path2"));
        let vid2 = repo.add_version("d1", &v2).await.unwrap();

        let mut v3 = make_version("h3", "application/pdf");
        v3.file_path = Some(std::path::PathBuf::from("/path3"));
        let _vid3 = repo.add_version("d1", &v3).await.unwrap();

        let cleared = repo
            .clear_version_file_paths_batch(&[vid1 as i32, vid2 as i32])
            .await
            .unwrap();
        assert_eq!(cleared, 2);

        let remaining = repo.count_legacy_file_paths().await.unwrap();
        assert_eq!(remaining, 1);

        let empty = repo
            .clear_version_file_paths_batch(&[])
            .await
            .unwrap();
        assert_eq!(empty, 0);
    }

    #[tokio::test]
    async fn test_count_legacy_file_paths() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let mut v1 = make_version("h1", "application/pdf");
        v1.file_path = Some(std::path::PathBuf::from("/path1"));
        repo.add_version("d1", &v1).await.unwrap();

        let mut v2 = make_version("h2", "application/pdf");
        v2.file_path = Some(std::path::PathBuf::from("/path2"));
        repo.add_version("d1", &v2).await.unwrap();

        let v3 = make_version("h3", "application/pdf");
        repo.add_version("d1", &v3).await.unwrap();

        let count = repo.count_legacy_file_paths().await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_get_legacy_file_path_versions() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src")).await.unwrap();
        let mut v1 = make_version("h1", "application/pdf");
        v1.file_path = Some(std::path::PathBuf::from("/path1"));
        let vid1 = repo.add_version("d1", &v1).await.unwrap();

        let mut v2 = make_version("h2", "image/png");
        v2.file_path = Some(std::path::PathBuf::from("/path2"));
        let _vid2 = repo.add_version("d1", &v2).await.unwrap();

        let batch1 = repo
            .get_legacy_file_path_versions(0, 1)
            .await
            .unwrap();
        assert_eq!(batch1.len(), 1);
        assert_eq!(batch1[0].0.content_hash, "h1");
        assert_eq!(batch1[0].1, "https://example.com/d1");
        assert_eq!(batch1[0].2, "Doc d1");

        let batch2 = repo
            .get_legacy_file_path_versions(vid1, 10)
            .await
            .unwrap();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].0.content_hash, "h2");
    }

    #[tokio::test]
    async fn test_get_content_hashes() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        repo.save(&make_doc("d1", "src-a")).await.unwrap();
        repo.add_version("d1", &make_version("old-hash", "application/pdf"))
            .await
            .unwrap();
        repo.add_version("d1", &make_version("new-hash", "application/pdf"))
            .await
            .unwrap();

        repo.save(&make_doc("d2", "src-b")).await.unwrap();
        repo.add_version("d2", &make_version("d2-hash", "image/png"))
            .await
            .unwrap();

        let hashes = repo.get_content_hashes().await.unwrap();
        assert_eq!(hashes.len(), 2);

        let d1_row = hashes.iter().find(|r| r.0 == "d1").unwrap();
        assert_eq!(d1_row.1, "src-a");
        assert_eq!(d1_row.2, "new-hash");
        assert_eq!(d1_row.3, "Doc d1");

        let d2_row = hashes.iter().find(|r| r.0 == "d2").unwrap();
        assert_eq!(d2_row.2, "d2-hash");
    }

    #[tokio::test]
    async fn test_find_sources_by_hash() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_with_version(&repo, "d1", "src-a", "shared-hash", "application/pdf").await;
        save_with_version(&repo, "d2", "src-b", "shared-hash", "application/pdf").await;
        save_with_version(&repo, "d3", "src-c", "different", "application/pdf").await;

        let all = repo
            .find_sources_by_hash("shared-hash", None)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);

        let excluded = repo
            .find_sources_by_hash("shared-hash", Some("src-a"))
            .await
            .unwrap();
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, "src-b");

        let none = repo
            .find_sources_by_hash("nonexistent", None)
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}
