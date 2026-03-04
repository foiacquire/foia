//! Document version operations.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, ReturningId};
use crate::models::DocumentVersion;
use crate::repository::models::DocumentVersionRecord;
use crate::repository::pool::DieselError;
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
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::DocumentVersions;
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
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::{DocumentVersions, Documents};
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
    use crate::repository::diesel_document::tests::setup_test_db;

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
}
