//! Complex queries, browsing, and statistics operations.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, MimeCount, TagRow};
use crate::models::{Document, DocumentStatus};
use crate::repository::document::DocumentNavigation;
use crate::repository::models::DocumentRecord;
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::{with_conn, with_conn_split};

/// Validate that a string only contains safe identifier characters (alphanumeric + underscore).
///
/// Used for values interpolated into JSON path expressions where bind parameters
/// aren't supported. Rejects anything that could be SQL injection.
fn validate_identifier(s: &str) -> Result<(), DieselError> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(diesel::result::Error::QueryBuilderError(
            format!("invalid identifier: '{}'", s).into(),
        ));
    }
    Ok(())
}

/// Parameters for browsing/filtering documents.
#[derive(Debug, Default, Clone)]
pub struct BrowseParams<'a> {
    pub source_id: Option<&'a str>,
    pub status: Option<&'a str>,
    pub categories: &'a [String],
    pub tags: &'a [String],
    pub search_query: Option<&'a str>,
    pub sort_field: Option<&'a str>,
    pub sort_order: Option<&'a str>,
    pub limit: u32,
    pub offset: u32,
}

impl DieselDocumentRepository {
    // ========================================================================
    // Counting Operations
    // ========================================================================

    /// Count all documents.
    pub async fn count(&self) -> Result<u64, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let count: i64 = documents::table
                .select(count_star())
                .get_result(&mut conn)
                .await?;
            Ok(count as u64)
        })
    }

    /// Get document counts per source.
    pub async fn get_all_source_counts(&self) -> Result<HashMap<String, u64>, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let rows: Vec<(String, i64)> = documents::table
                .group_by(documents::source_id)
                .select((documents::source_id, count_star()))
                .load(&mut conn)
                .await?;

            Ok(rows
                .into_iter()
                .map(|(id, count)| (id, count as u64))
                .collect())
        })
    }

    /// Count documents needing a specific analysis type.
    ///
    /// A document needs analysis when:
    /// - No `complete` result exists in `document_analysis_results` for the type
    /// - No `failed` result exists within the retry window
    /// - No `pending` result exists within 90 minutes (worker lock)
    pub async fn count_needing_analysis(
        &self,
        analysis_type: &str,
        source_id: Option<&str>,
        mime_type: Option<&str>,
        retry_interval_hours: u32,
    ) -> Result<u64, DieselError> {
        use crate::schema::{document_analysis_results as dar, document_versions};
        use diesel::dsl::{count_distinct, exists, not};

        let retry_cutoff =
            (Utc::now() - chrono::Duration::hours(i64::from(retry_interval_hours))).to_rfc3339();
        let lock_cutoff = (Utc::now() - chrono::Duration::minutes(90)).to_rfc3339();

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .inner_join(document_versions::table)
                .filter(documents::status.ne("failed"))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("complete")),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("failed"))
                        .filter(dar::created_at.gt(&retry_cutoff)),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("pending"))
                        .filter(dar::created_at.gt(&lock_cutoff)),
                )))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(mime) = mime_type {
                query = query.filter(document_versions::mime_type.eq(mime));
            }

            let count: i64 = query
                .select(count_distinct(documents::id))
                .first(&mut conn)
                .await?;
            Ok(count as u64)
        })
    }

    /// Count documents needing OCR.
    #[deprecated(note = "Use count_needing_analysis(\"ocr\", ...) instead")]
    pub async fn count_needing_ocr(&self, source_id: Option<&str>) -> Result<u64, DieselError> {
        self.count_needing_analysis("ocr", source_id, None, 12)
            .await
    }

    /// Count documents needing OCR with optional mime type filter.
    #[deprecated(note = "Use count_needing_analysis(\"ocr\", ...) instead")]
    pub async fn count_needing_ocr_filtered(
        &self,
        source_id: Option<&str>,
        mime_type: Option<&str>,
    ) -> Result<u64, DieselError> {
        self.count_needing_analysis("ocr", source_id, mime_type, 12)
            .await
    }

    /// Count documents needing summarization.
    /// Documents need summarization if status is 'ocr_complete' (OCR done but not indexed).
    pub async fn count_needing_summarization(
        &self,
        source_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq("ocr_complete"))
                .into_boxed();
            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            let count: i64 = query.count().get_result(&mut conn).await?;
            Ok(count as u64)
        })
    }

    /// Count documents by source.
    pub async fn count_by_source(&self, source_id: &str) -> Result<u64, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let count: i64 = documents::table
                .filter(documents::source_id.eq(source_id))
                .select(count_star())
                .get_result(&mut conn)
                .await?;
            Ok(count as u64)
        })
    }

    /// Count documents by status.
    pub async fn count_by_status(
        &self,
        source_id: Option<&str>,
    ) -> Result<HashMap<String, u64>, DieselError> {
        use diesel::dsl::count_star;

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .group_by(documents::status)
                .select((documents::status, count_star()))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            let rows: Vec<(String, i64)> = query.load(&mut conn).await?;
            let mut counts = HashMap::new();
            for (status, count) in rows {
                counts.insert(status, count as u64);
            }
            Ok(counts)
        })
    }

    /// Count all by status.
    pub async fn count_all_by_status(&self) -> Result<HashMap<String, u64>, DieselError> {
        self.count_by_status(None).await
    }

    /// Get status counts for each source.
    /// Returns a map of source_id -> (status -> count).
    pub async fn get_source_status_counts(
        &self,
    ) -> Result<HashMap<String, HashMap<String, u64>>, DieselError> {
        use diesel::dsl::count_star;

        with_conn!(self.pool, conn, {
            let rows: Vec<(String, String, i64)> = documents::table
                .group_by((documents::source_id, documents::status))
                .select((documents::source_id, documents::status, count_star()))
                .load(&mut conn)
                .await?;

            let mut result: HashMap<String, HashMap<String, u64>> = HashMap::new();
            for (source_id, status, count) in rows {
                result
                    .entry(source_id)
                    .or_default()
                    .insert(status, count as u64);
            }
            Ok(result)
        })
    }

    /// Count documents needing date estimation.
    /// These are documents without an estimated_date in metadata.
    pub async fn count_documents_needing_date_estimation(
        &self,
        source_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        use diesel::dsl::count_star;

        with_conn_split!(self.pool,
            sqlite: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "json_extract(metadata, '$.estimated_date') IS NULL",
                    ))
                    .select(count_star())
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                let count: i64 = query.first(&mut conn).await?;
                Ok(count as u64)
            },
            postgres: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "metadata->>'estimated_date' IS NULL",
                    ))
                    .select(count_star())
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                let count: i64 = query.first(&mut conn).await?;
                Ok(count as u64)
            }
        )
    }

    // ========================================================================
    // Generic Annotation Queries
    // ========================================================================

    /// Count documents needing a specific annotation type.
    ///
    /// A document needs annotation when `metadata.annotations[type]` is missing
    /// or has a version less than the requested version.
    /// For "llm_summary", also requires status = 'ocr_complete'.
    pub async fn count_documents_needing_annotation(
        &self,
        annotation_type: &str,
        version: i32,
        source_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        // For llm_summary, delegate to existing specialized query
        if annotation_type == "llm_summary" {
            return self.count_needing_summarization(source_id).await;
        }

        // For date_detection, delegate to existing specialized query
        if annotation_type == "date_detection" {
            return self
                .count_documents_needing_date_estimation(source_id)
                .await;
        }

        // annotation_type is interpolated into JSON path expressions where bind
        // params aren't supported — validate it only contains safe identifier chars
        validate_identifier(annotation_type)?;

        use diesel::dsl::count_star;

        // version (i32) is safe to interpolate; annotation_type is validated above
        with_conn_split!(self.pool,
            sqlite: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "(json_extract(metadata, '$.annotations.{annotation_type}.version') IS NULL \
                         OR json_extract(metadata, '$.annotations.{annotation_type}.version') < {version})"
                    )))
                    .select(count_star())
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                let count: i64 = query.first(&mut conn).await?;
                Ok(count as u64)
            },
            postgres: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "((metadata->'annotations'->'{annotation_type}'->>'version')::int IS NULL \
                         OR (metadata->'annotations'->'{annotation_type}'->>'version')::int < {version})"
                    )))
                    .select(count_star())
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                let count: i64 = query.first(&mut conn).await?;
                Ok(count as u64)
            }
        )
    }

    /// Get documents needing a specific annotation type.
    pub async fn get_documents_needing_annotation(
        &self,
        annotation_type: &str,
        version: i32,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        // For llm_summary, delegate to existing specialized query
        if annotation_type == "llm_summary" {
            return self.get_needing_summarization(limit).await;
        }

        // For date_detection, delegate to existing specialized query
        if annotation_type == "date_detection" {
            return self
                .get_documents_needing_date_estimation(source_id, limit)
                .await;
        }

        validate_identifier(annotation_type)?;

        // version (i32) is safe to interpolate; annotation_type is validated above
        let doc_ids: Vec<String> = with_conn_split!(self.pool,
            sqlite: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "(json_extract(metadata, '$.annotations.{annotation_type}.version') IS NULL \
                         OR json_extract(metadata, '$.annotations.{annotation_type}.version') < {version})"
                    )))
                    .select(documents::id)
                    .limit(limit as i64)
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                query.load::<String>(&mut conn).await
            },
            postgres: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "((metadata->'annotations'->'{annotation_type}'->>'version')::int IS NULL \
                         OR (metadata->'annotations'->'{annotation_type}'->>'version')::int < {version})"
                    )))
                    .select(documents::id)
                    .limit(limit as i64)
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                query.load::<String>(&mut conn).await
            }
        )?;

        self.get_batch(&doc_ids).await
    }

    // ========================================================================
    // Statistics Operations
    // ========================================================================

    /// Get type statistics - count documents by MIME type.
    pub async fn get_type_stats(&self) -> Result<HashMap<String, u64>, DieselError> {
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::DocumentVersions;
        use sea_query::{Alias, Expr, Query};

        let latest = Query::select()
            .column(DocumentVersions::DocumentId)
            .expr_as(
                Expr::col(DocumentVersions::Id).max(),
                Alias::new("max_id"),
            )
            .from(DocumentVersions::Table)
            .group_by_col(DocumentVersions::DocumentId)
            .to_owned();

        let latest_alias = Alias::new("latest");
        let stmt = Query::select()
            .expr_as(
                Expr::cust_with_expr(
                    "COALESCE($1, 'unknown')",
                    Expr::col((DocumentVersions::Table, DocumentVersions::MimeType)),
                ),
                Alias::new("mime_type"),
            )
            .expr_as(
                Expr::cust_with_expr(
                    "COUNT(DISTINCT $1)",
                    Expr::col((DocumentVersions::Table, DocumentVersions::DocumentId)),
                ),
                Alias::new("count"),
            )
            .from(DocumentVersions::Table)
            .join_subquery(
                sea_query::JoinType::InnerJoin,
                latest,
                latest_alias.clone(),
                Expr::col((DocumentVersions::Table, DocumentVersions::DocumentId))
                    .equals((latest_alias.clone(), Alias::new("document_id")))
                    .and(
                        Expr::col((DocumentVersions::Table, DocumentVersions::Id))
                            .equals((latest_alias, Alias::new("max_id"))),
                    ),
            )
            .group_by_col((DocumentVersions::Table, DocumentVersions::MimeType))
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            let results: Vec<MimeCount> =
                diesel_async::RunQueryDsl::load(diesel::sql_query(&sql), &mut conn).await?;
            let mut stats = HashMap::new();
            for row in results {
                stats.insert(row.mime_type, row.count as u64);
            }
            Ok(stats)
        })
    }

    /// Get category statistics - count documents by category_id.
    /// Uses the trigger-maintained file_categories.doc_count
    /// when no source filter is applied; falls back to GROUP BY for per-source stats.
    pub async fn get_category_stats(
        &self,
        source_id: Option<&str>,
    ) -> Result<HashMap<String, u64>, DieselError> {
        use diesel::dsl::count_star;

        with_conn!(self.pool, conn, {
            if let Some(sid) = source_id {
                let rows: Vec<(Option<String>, i64)> = documents::table
                    .filter(documents::source_id.eq(sid))
                    .group_by(documents::category_id)
                    .select((documents::category_id, count_star()))
                    .load(&mut conn)
                    .await?;

                let mut stats = HashMap::new();
                for (category_id, count) in rows {
                    let category = category_id.unwrap_or_else(|| "unknown".to_string());
                    stats.insert(category, count as u64);
                }
                Ok(stats)
            } else {
                // file_categories is a trigger-maintained table not in the Diesel schema;
                // use sea-query for portable SQL generation.
                use crate::repository::pool::build_sql;
                use crate::repository::sea_tables::FileCategories;
                use sea_query::{Alias, Expr, Query};

                #[derive(diesel::QueryableByName)]
                struct CategoryCount {
                    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
                    category_id: Option<String>,
                    #[diesel(sql_type = diesel::sql_types::BigInt)]
                    count: i64,
                }

                let stmt = Query::select()
                    .expr_as(
                        Expr::col(FileCategories::Id),
                        Alias::new("category_id"),
                    )
                    .expr_as(
                        Expr::col(FileCategories::DocCount),
                        Alias::new("count"),
                    )
                    .from(FileCategories::Table)
                    .and_where(Expr::col(FileCategories::DocCount).gt(0))
                    .to_owned();

                let sql = build_sql(&self.pool, &stmt);

                let results: Vec<CategoryCount> =
                    diesel_async::RunQueryDsl::load(diesel::sql_query(&sql), &mut conn).await?;

                let mut stats = HashMap::new();
                for row in results {
                    let category = row.category_id.unwrap_or_else(|| "unknown".to_string());
                    stats.insert(category, row.count as u64);
                }
                Ok(stats)
            }
        })
    }

    // ========================================================================
    // Browse and Search Operations
    // ========================================================================

    /// Get recent documents.
    pub async fn get_recent(&self, limit: u32) -> Result<Vec<Document>, DieselError> {
        let limit = limit as i64;
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .order(documents::updated_at.desc())
                .limit(limit)
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Browse documents.
    pub async fn browse(&self, params: BrowseParams<'_>) -> Result<Vec<Document>, DieselError> {
        let limit = params.limit as i64;
        let offset = params.offset as i64;
        let source_id = params.source_id;
        let status = params.status;
        let categories = params.categories;
        let tags = params.tags;
        let search_query = params.search_query;
        let sort_field = params.sort_field;
        let sort_order = params.sort_order;

        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            // Build query with filters first, then order and paginate
            let mut query = documents::table.into_boxed();

            // Apply filters
            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(st) = status {
                query = query.filter(documents::status.eq(st));
            }
            if !categories.is_empty() {
                query = query.filter(documents::category_id.eq_any(categories));
            }
            // Tags are stored as comma-separated, filter docs that contain any of the requested tags
            for tag in tags {
                let pattern = format!("%{}%", tag);
                query = query.filter(documents::tags.like(pattern));
            }
            // Text search on title and synopsis
            if let Some(q) = search_query {
                if !q.is_empty() {
                    let pattern = format!("%{}%", q);
                    query = query.filter(
                        documents::title
                            .like(pattern.clone())
                            .or(documents::synopsis.like(pattern)),
                    );
                }
            }

            // Apply sorting
            let is_desc = sort_order
                .map(|o| o.eq_ignore_ascii_case("desc"))
                .unwrap_or(true);
            match sort_field {
                Some("created_at") => {
                    if is_desc {
                        query = query.order(documents::created_at.desc());
                    } else {
                        query = query.order(documents::created_at.asc());
                    }
                }
                Some("title") => {
                    if is_desc {
                        query = query.order(documents::title.desc());
                    } else {
                        query = query.order(documents::title.asc());
                    }
                }
                _ => {
                    // Default: updated_at desc
                    if is_desc {
                        query = query.order(documents::updated_at.desc());
                    } else {
                        query = query.order(documents::updated_at.asc());
                    }
                }
            }

            query.limit(limit).offset(offset).load(&mut conn).await
        })?;

        // Batch load all versions in a single query
        let doc_ids: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
        let mut versions_map = self.load_versions_batch(&doc_ids).await?;

        let docs = records
            .into_iter()
            .map(|record| {
                let versions = versions_map.remove(&record.id).unwrap_or_default();
                Self::record_to_document(record, versions)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(docs)
    }

    /// Browse count.
    pub async fn browse_count(
        &self,
        source_id: Option<&str>,
        status: Option<&str>,
        categories: &[String],
        tags: &[String],
        search_query: Option<&str>,
    ) -> Result<u64, DieselError> {
        let has_filters = status.is_some()
            || !categories.is_empty()
            || !tags.is_empty()
            || search_query.is_some_and(|q| !q.is_empty());

        // Use pre-computed counts when no filters are active
        if !has_filters {
            return if let Some(sid) = source_id {
                self.count_by_source(sid).await
            } else {
                self.count().await
            };
        }

        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let mut query = documents::table.select(count_star()).into_boxed();
            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(st) = status {
                query = query.filter(documents::status.eq(st));
            }
            if !categories.is_empty() {
                query = query.filter(documents::category_id.eq_any(categories));
            }
            for tag in tags {
                let pattern = format!("%{}%", tag);
                query = query.filter(documents::tags.like(pattern));
            }
            if let Some(q) = search_query {
                if !q.is_empty() {
                    let pattern = format!("%{}%", q);
                    query = query.filter(
                        documents::title
                            .like(pattern.clone())
                            .or(documents::synopsis.like(pattern)),
                    );
                }
            }
            let count: i64 = query.first(&mut conn).await?;
            Ok(count as u64)
        })
    }

    /// Optimized browse that only loads columns needed for listing.
    /// Avoids loading `extracted_text` which can be very large (OCR text).
    /// Two-step query: fetch document page first, then batch-load latest versions.
    pub async fn browse_fast(
        &self,
        source_id: Option<&str>,
        _status: Option<&str>,
        categories: &[String],
        tags: &[String],
        limit: u32,
        offset: u32,
    ) -> Result<Vec<super::BrowseRow>, DieselError> {
        use crate::schema::document_versions;

        with_conn!(self.pool, conn, {
            // Step 1: fetch the page of documents that have at least one version
            // Use EXISTS subquery to filter out versionless documents
            let mut query = documents::table
                .select((
                    documents::id,
                    documents::title,
                    documents::source_id,
                    documents::synopsis,
                    documents::tags,
                ))
                .filter(diesel::dsl::exists(
                    document_versions::table
                        .filter(document_versions::document_id.eq(documents::id))
                        .select(document_versions::id),
                ))
                .order(documents::updated_at.desc())
                .limit(limit as i64)
                .offset(offset as i64)
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if !categories.is_empty() {
                query = query.filter(documents::category_id.eq_any(categories));
            }
            for tag in tags {
                let pattern = format!("%{}%", tag);
                query = query.filter(documents::tags.like(pattern));
            }

            #[allow(clippy::type_complexity)]
            let doc_rows: Vec<(
                String,
                String,
                String,
                Option<String>,
                Option<String>,
            )> = query.load(&mut conn).await?;

            if doc_rows.is_empty() {
                return Ok(Vec::new());
            }

            let doc_ids: Vec<&str> = doc_rows.iter().map(|r| r.0.as_str()).collect();

            // Step 2: fetch all versions for these documents, ordered by id desc
            let version_rows: Vec<(String, Option<String>, String, i32, String)> =
                document_versions::table
                    .filter(document_versions::document_id.eq_any(&doc_ids))
                    .order(document_versions::id.desc())
                    .select((
                        document_versions::document_id,
                        document_versions::original_filename,
                        document_versions::mime_type,
                        document_versions::file_size,
                        document_versions::acquired_at,
                    ))
                    .load(&mut conn)
                    .await?;

            // Take only the latest version per document (first seen per document_id)
            let mut latest_versions: HashMap<&str, (Option<String>, String, i32, String)> =
                HashMap::new();
            for (doc_id, filename, mime, size, acquired) in &version_rows {
                latest_versions
                    .entry(doc_id.as_str())
                    .or_insert_with(|| (filename.clone(), mime.clone(), *size, acquired.clone()));
            }

            // Combine in document order
            let results: Vec<super::BrowseRow> = doc_rows
                .into_iter()
                .filter_map(|(id, title, source_id, synopsis, tags)| {
                    let (filename, mime, size, acquired) = latest_versions.remove(id.as_str())?;
                    Some(super::BrowseRow {
                        id,
                        title,
                        source_id,
                        synopsis,
                        tags,
                        original_filename: filename,
                        mime_type: mime,
                        file_size: size,
                        acquired_at: acquired,
                    })
                })
                .collect();

            Ok(results)
        })
    }

    /// Get document navigation.
    pub async fn get_document_navigation(
        &self,
        document_id: &str,
        source_id: &str,
    ) -> Result<DocumentNavigation, DieselError> {
        use diesel::dsl::count_star;

        with_conn!(self.pool, conn, {
            let prev: Option<(String, String)> = documents::table
                .select((documents::id, documents::title))
                .filter(documents::source_id.eq(source_id))
                .filter(documents::id.lt(document_id))
                .order(documents::id.desc())
                .first(&mut conn)
                .await
                .optional()?;
            let next: Option<(String, String)> = documents::table
                .select((documents::id, documents::title))
                .filter(documents::source_id.eq(source_id))
                .filter(documents::id.gt(document_id))
                .order(documents::id.asc())
                .first(&mut conn)
                .await
                .optional()?;
            let position: i64 = documents::table
                .filter(documents::source_id.eq(source_id))
                .filter(documents::id.le(document_id))
                .select(count_star())
                .first(&mut conn)
                .await?;
            let total: i64 = documents::table
                .filter(documents::source_id.eq(source_id))
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(DocumentNavigation {
                prev_id: prev.as_ref().map(|(id, _)| id.clone()),
                prev_title: prev.map(|(_, title)| title),
                next_id: next.as_ref().map(|(id, _)| id.clone()),
                next_title: next.map(|(_, title)| title),
                position: position as u64,
                total: total as u64,
            })
        })
    }

    /// Search tags by prefix in document metadata.
    /// Tags are stored as JSON arrays in the metadata field.
    pub async fn search_tags(&self, query: &str) -> Result<Vec<String>, DieselError> {
        use crate::repository::sea_tables::Documents;
        use sea_query::{Alias, DynIden, Expr, Func, Query, TableRef};

        let pattern = format!("%{}%", query.to_lowercase());

        with_conn_split!(self.pool,
            sqlite: conn => {
                let json_each = Func::cust(Alias::new("json_each")).arg(
                    Expr::cust_with_expr(
                        "json_extract($1, '$.tags')",
                        Expr::col(Documents::Metadata),
                    ),
                );

                let stmt = Query::select()
                    .distinct()
                    .expr_as(Expr::cust("value"), Alias::new("tag"))
                    .from(Documents::Table)
                    .from(TableRef::FunctionCall(
                        json_each,
                        DynIden::new(Alias::new("_je")),
                    ))
                    .and_where(Expr::cust_with_expr(
                        "LOWER(value) LIKE $1",
                        Expr::val(&pattern as &str),
                    ))
                    .order_by(Alias::new("value"), sea_query::Order::Asc)
                    .limit(100)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(&pattern),
                    &mut conn,
                )
                .await
                .unwrap_or_default();
                Ok(results.into_iter().map(|r| r.tag).collect())
            },
            postgres: conn => {
                let tags_col = Alias::new("tags");
                let jsonb_elements =
                    Func::cust(Alias::new("jsonb_array_elements_text")).arg(
                        Expr::cust_with_expr(
                            "$1::jsonb",
                            Expr::col((Documents::Table, tags_col.clone())),
                        ),
                    );

                let stmt = Query::select()
                    .distinct()
                    .expr_as(Expr::cust("tag"), Alias::new("tag"))
                    .from(Documents::Table)
                    .from(TableRef::FunctionCall(
                        jsonb_elements,
                        DynIden::new(Alias::new("tag")),
                    ))
                    .and_where(
                        Expr::col((Documents::Table, tags_col.clone())).is_not_null(),
                    )
                    .and_where(Expr::cust_with_expr(
                        "$1 != '[]'",
                        Expr::col((Documents::Table, tags_col)),
                    ))
                    .and_where(Expr::cust_with_expr(
                        "LOWER(tag) LIKE $1",
                        Expr::val(&pattern as &str),
                    ))
                    .order_by(Alias::new("tag"), sea_query::Order::Asc)
                    .limit(100)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(&pattern),
                    &mut conn,
                )
                .await
                .unwrap_or_default();
                Ok(results.into_iter().map(|r| r.tag).collect())
            }
        )
    }

    /// Get all unique tags from document metadata.
    pub async fn get_all_tags(&self) -> Result<Vec<String>, DieselError> {
        use crate::repository::sea_tables::Documents;
        use sea_query::{Alias, DynIden, Expr, Func, Query, TableRef};

        let tags_col = Alias::new("tags");

        with_conn_split!(self.pool,
            sqlite: conn => {
                let json_each = Func::cust(Alias::new("json_each")).arg(
                    Expr::col((Documents::Table, tags_col.clone())),
                );

                let stmt = Query::select()
                    .distinct()
                    .expr_as(Expr::cust("value"), Alias::new("tag"))
                    .from(Documents::Table)
                    .from(TableRef::FunctionCall(
                        json_each,
                        DynIden::new(Alias::new("_je")),
                    ))
                    .and_where(
                        Expr::col((Documents::Table, tags_col.clone())).is_not_null(),
                    )
                    .and_where(Expr::cust_with_expr(
                        "$1 != '[]'",
                        Expr::col((Documents::Table, tags_col.clone())),
                    ))
                    .order_by(Alias::new("value"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql),
                    &mut conn,
                )
                .await
                .unwrap_or_default();
                Ok(results.into_iter().map(|r| r.tag).collect())
            },
            postgres: conn => {
                let jsonb_elements =
                    Func::cust(Alias::new("jsonb_array_elements_text")).arg(
                        Expr::cust_with_expr(
                            "$1::jsonb",
                            Expr::col((Documents::Table, tags_col.clone())),
                        ),
                    );

                let stmt = Query::select()
                    .distinct()
                    .expr_as(Expr::cust("tag"), Alias::new("tag"))
                    .from(Documents::Table)
                    .from(TableRef::FunctionCall(
                        jsonb_elements,
                        DynIden::new(Alias::new("tag")),
                    ))
                    .and_where(
                        Expr::col((Documents::Table, tags_col.clone())).is_not_null(),
                    )
                    .and_where(Expr::cust_with_expr(
                        "$1 != '[]'",
                        Expr::col((Documents::Table, tags_col)),
                    ))
                    .order_by(Alias::new("tag"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql),
                    &mut conn,
                )
                .await
                .unwrap_or_default();
                Ok(results.into_iter().map(|r| r.tag).collect())
            }
        )
    }

    /// Get documents by tag.
    /// Tags are stored in metadata JSON.
    pub async fn get_by_tag(
        &self,
        tag: &str,
        source_id: Option<&str>,
    ) -> Result<Vec<Document>, DieselError> {
        let doc_ids: Vec<String> = with_conn_split!(self.pool,
            sqlite: conn => {
                let tag_filter = diesel::dsl::sql::<diesel::sql_types::Bool>(
                    "EXISTS (SELECT 1 FROM json_each(json_extract(metadata, '$.tags')) WHERE value = ",
                )
                .bind::<diesel::sql_types::Text, _>(tag)
                .sql(")");

                if let Some(sid) = source_id {
                    documents::table
                        .filter(documents::source_id.eq(sid))
                        .filter(tag_filter)
                        .select(documents::id)
                        .order(documents::updated_at.desc())
                        .load::<String>(&mut conn)
                        .await
                } else {
                    documents::table
                        .filter(tag_filter)
                        .select(documents::id)
                        .order(documents::updated_at.desc())
                        .load::<String>(&mut conn)
                        .await
                }
            },
            postgres: conn => {
                let tag_filter = diesel::dsl::sql::<diesel::sql_types::Bool>(
                    "metadata->'tags' ? ",
                )
                .bind::<diesel::sql_types::Text, _>(tag);

                if let Some(sid) = source_id {
                    documents::table
                        .filter(documents::source_id.eq(sid))
                        .filter(tag_filter)
                        .select(documents::id)
                        .order(documents::updated_at.desc())
                        .load::<String>(&mut conn)
                        .await
                } else {
                    documents::table
                        .filter(tag_filter)
                        .select(documents::id)
                        .order(documents::updated_at.desc())
                        .load::<String>(&mut conn)
                        .await
                }
            }
        )?;

        self.get_batch(&doc_ids).await
    }

    /// Get documents by MIME type category.
    pub async fn get_by_type_category(
        &self,
        category: &str,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        let mime_patterns = crate::utils::category_to_mime_patterns(category);
        if mime_patterns.is_empty() {
            return Ok(vec![]);
        }

        // category_id is pre-computed on document save from the version MIME type,
        // so we can filter directly on it instead of joining versions.
        let doc_ids: Vec<String> = with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::category_id.eq(category))
                .select(documents::id)
                .order(documents::updated_at.desc())
                .limit(limit as i64)
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            query.load(&mut conn).await
        })?;

        self.get_batch(&doc_ids).await
    }

    // ========================================================================
    // Timeline Operations
    // ========================================================================

    /// Get timeline buckets (daily counts) for documents by publication date.
    ///
    /// Returns (date_string, timestamp, count) tuples grouped by day.
    /// Uses `manual_date` if set, otherwise `estimated_date`.
    /// Only includes documents that have a publication date.
    /// Optionally filtered by source_id and date range.
    pub async fn get_timeline_buckets(
        &self,
        source_id: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
    ) -> Result<Vec<(String, i64, u64)>, DieselError> {
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::Documents;
        use sea_query::{Alias, Expr, ExprTrait, Query};

        #[derive(diesel::QueryableByName)]
        struct TimelineBucket {
            #[diesel(sql_type = diesel::sql_types::Text)]
            date_bucket: String,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            count: i64,
        }

        let coalesce = Expr::cust_with_exprs(
            "COALESCE($1, $2)",
            [
                Expr::col(Documents::ManualDate).into(),
                Expr::col(Documents::EstimatedDate).into(),
            ],
        );

        let date_fn = Expr::cust_with_expr("date($1)", coalesce.clone());

        let mut stmt = Query::select()
            .expr_as(date_fn.clone(), Alias::new("date_bucket"))
            .expr_as(Expr::cust("COUNT(*)"), Alias::new("count"))
            .from(Documents::Table)
            .and_where(coalesce.is_not_null())
            .to_owned();

        if let Some(sid) = source_id {
            stmt = stmt
                .and_where(Expr::col(Documents::SourceId).eq(sid))
                .to_owned();
        }
        if let Some(start) = start_date {
            stmt = stmt.and_where(date_fn.clone().gte(start)).to_owned();
        }
        if let Some(end) = end_date {
            stmt = stmt.and_where(date_fn.clone().lte(end)).to_owned();
        }

        stmt = stmt
            .group_by_col(Alias::new("date_bucket"))
            .order_by(Alias::new("date_bucket"), sea_query::Order::Asc)
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            use diesel_async::RunQueryDsl;

            // Bind order: source_id (if present), start_date (if present), end_date (if present)
            let results: Vec<TimelineBucket> = match (source_id, start_date, end_date) {
                (Some(sid), Some(start), Some(end)) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(sid)
                        .bind::<diesel::sql_types::Text, _>(start)
                        .bind::<diesel::sql_types::Text, _>(end)
                        .load(&mut conn)
                        .await?
                }
                (Some(sid), Some(start), None) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(sid)
                        .bind::<diesel::sql_types::Text, _>(start)
                        .load(&mut conn)
                        .await?
                }
                (Some(sid), None, Some(end)) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(sid)
                        .bind::<diesel::sql_types::Text, _>(end)
                        .load(&mut conn)
                        .await?
                }
                (Some(sid), None, None) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(sid)
                        .load(&mut conn)
                        .await?
                }
                (None, Some(start), Some(end)) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(start)
                        .bind::<diesel::sql_types::Text, _>(end)
                        .load(&mut conn)
                        .await?
                }
                (None, Some(start), None) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(start)
                        .load(&mut conn)
                        .await?
                }
                (None, None, Some(end)) => {
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(end)
                        .load(&mut conn)
                        .await?
                }
                (None, None, None) => diesel::sql_query(&sql).load(&mut conn).await?,
            };

            let buckets: Vec<(String, i64, u64)> = results
                .into_iter()
                .map(|b| {
                    let timestamp = chrono::NaiveDate::parse_from_str(&b.date_bucket, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
                        .unwrap_or(0);
                    (b.date_bucket, timestamp, b.count as u64)
                })
                .collect();

            Ok(buckets)
        })
    }

    // ========================================================================
    // Document State Operations
    // ========================================================================

    /// Get documents needing date estimation.
    pub async fn get_documents_needing_date_estimation(
        &self,
        source_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        let doc_ids: Vec<String> = with_conn_split!(self.pool,
            sqlite: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "json_extract(metadata, '$.estimated_date') IS NULL",
                    ))
                    .select(documents::id)
                    .limit(limit as i64)
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                query.load::<String>(&mut conn).await
            },
            postgres: conn => {
                let mut query = documents::table
                    .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "metadata->>'estimated_date' IS NULL",
                    ))
                    .select(documents::id)
                    .limit(limit as i64)
                    .into_boxed();
                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                query.load::<String>(&mut conn).await
            }
        )?;

        self.get_batch(&doc_ids).await
    }

    /// Update estimated date in document metadata.
    pub async fn update_estimated_date(
        &self,
        id: &str,
        date: DateTime<Utc>,
        confidence: &str,
        source: &str,
    ) -> Result<(), DieselError> {
        let record: Option<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table.find(id).first(&mut conn).await.optional()
        })?;

        if let Some(record) = record {
            let mut metadata: serde_json::Value =
                serde_json::from_str(&record.metadata).unwrap_or(serde_json::json!({}));

            metadata["estimated_date"] = serde_json::json!({
                "date": date.to_rfc3339(),
                "confidence": confidence,
                "source": source,
            });

            let now = Utc::now().to_rfc3339();
            with_conn!(self.pool, conn, {
                diesel::update(documents::table.find(id))
                    .set((
                        documents::metadata.eq(metadata.to_string()),
                        documents::updated_at.eq(&now),
                    ))
                    .execute(&mut conn)
                    .await?;
                Ok::<(), DieselError>(())
            })?;
        }

        Ok(())
    }

    /// Record an annotation result in document metadata.
    pub async fn record_annotation(
        &self,
        id: &str,
        annotation_type: &str,
        version: i32,
        data: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), DieselError> {
        let record: Option<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table.find(id).first(&mut conn).await.optional()
        })?;

        if let Some(record) = record {
            let mut metadata: serde_json::Value =
                serde_json::from_str(&record.metadata).unwrap_or(serde_json::json!({}));

            let annotations = metadata
                .as_object_mut()
                .unwrap()
                .entry("annotations")
                .or_insert(serde_json::json!({}));

            annotations[annotation_type] = serde_json::json!({
                "version": version,
                "data": data,
                "error": error,
                "timestamp": Utc::now().to_rfc3339(),
            });

            let now = Utc::now().to_rfc3339();
            with_conn!(self.pool, conn, {
                diesel::update(documents::table.find(id))
                    .set((
                        documents::metadata.eq(metadata.to_string()),
                        documents::updated_at.eq(&now),
                    ))
                    .execute(&mut conn)
                    .await?;
                Ok::<(), DieselError>(())
            })?;
        }

        Ok(())
    }

    /// Get documents needing summarization.
    pub async fn get_needing_summarization(
        &self,
        limit: usize,
    ) -> Result<Vec<Document>, DieselError> {
        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::status.eq("ocr_complete"))
                .order(documents::updated_at.asc())
                .limit(limit as i64)
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Get documents needing OCR.
    #[allow(dead_code)]
    #[deprecated(note = "Use get_needing_analysis(\"ocr\", ...) instead")]
    pub async fn get_needing_ocr(&self, limit: usize) -> Result<Vec<Document>, DieselError> {
        self.get_needing_analysis("ocr", limit, None, None, None, 12)
            .await
    }

    /// Get documents needing a specific analysis type.
    ///
    /// Uses cursor-based pagination: pass `after_id` to fetch the next page.
    /// Returns documents that have no `complete` result in `document_analysis_results`
    /// for the given type, no recent `failed` result within the retry window,
    /// and no `pending` result within the lock window (90 minutes).
    pub async fn get_needing_analysis(
        &self,
        analysis_type: &str,
        limit: usize,
        source_id: Option<&str>,
        mime_type: Option<&str>,
        after_id: Option<&str>,
        retry_interval_hours: u32,
    ) -> Result<Vec<Document>, DieselError> {
        use crate::schema::{document_analysis_results as dar, document_versions};
        use diesel::dsl::{exists, not};

        let retry_cutoff =
            (Utc::now() - chrono::Duration::hours(i64::from(retry_interval_hours))).to_rfc3339();
        let lock_cutoff = (Utc::now() - chrono::Duration::minutes(90)).to_rfc3339();

        let ids: Vec<String> = with_conn!(self.pool, conn, {
            let mut query = documents::table
                .inner_join(document_versions::table)
                .filter(documents::status.ne("failed"))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("complete")),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("failed"))
                        .filter(dar::created_at.gt(&retry_cutoff)),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("pending"))
                        .filter(dar::created_at.gt(&lock_cutoff)),
                )))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(mime) = mime_type {
                query = query.filter(document_versions::mime_type.eq(mime));
            }
            if let Some(cursor) = after_id {
                query = query.filter(documents::id.gt(cursor));
            }

            let extract_text_null = diesel::dsl::sql::<diesel::sql_types::Integer>(
                "CASE WHEN documents.extracted_text IS NULL THEN 0 ELSE 1 END",
            );
            query
                .select((documents::id, documents::analysis_priority, extract_text_null))
                .distinct()
                .order((
                    documents::analysis_priority.desc(),
                    diesel::dsl::sql::<diesel::sql_types::Integer>(
                        "CASE WHEN documents.extracted_text IS NULL THEN 0 ELSE 1 END",
                    )
                    .asc(),
                    documents::id.asc(),
                ))
                .limit(limit as i64)
                .load::<(String, i32, i32)>(&mut conn)
                .await
                .map(|rows| rows.into_iter().map(|(id, _, _)| id).collect())
        })?;
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let records: Vec<DocumentRecord> = with_conn!(self.pool, conn, {
            documents::table
                .filter(documents::id.eq_any(&ids))
                .order(documents::analysis_priority.desc())
                .load(&mut conn)
                .await
        })?;

        self.records_to_documents(records).await
    }

    /// Get all document IDs needing analysis (no LIMIT, no hydration).
    ///
    /// Same NOT EXISTS logic as `get_needing_analysis` but returns only IDs.
    /// Intended for pre-fetching: run the expensive query once, then process
    /// from the cached ID list with cheap `get_batch` lookups.
    pub async fn get_needing_analysis_ids(
        &self,
        analysis_type: &str,
        source_id: Option<&str>,
        mime_type: Option<&str>,
        retry_interval_hours: u32,
    ) -> Result<Vec<String>, DieselError> {
        use crate::schema::{document_analysis_results as dar, document_versions};
        use diesel::dsl::{exists, not};

        let retry_cutoff =
            (Utc::now() - chrono::Duration::hours(i64::from(retry_interval_hours))).to_rfc3339();
        let lock_cutoff = (Utc::now() - chrono::Duration::minutes(90)).to_rfc3339();

        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .inner_join(document_versions::table)
                .filter(documents::status.ne("failed"))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("complete")),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("failed"))
                        .filter(dar::created_at.gt(&retry_cutoff)),
                )))
                .filter(not(exists(
                    dar::table
                        .filter(dar::document_id.eq(documents::id))
                        .filter(dar::version_id.eq(document_versions::id))
                        .filter(dar::analysis_type.eq(analysis_type))
                        .filter(dar::status.eq("pending"))
                        .filter(dar::created_at.gt(&lock_cutoff)),
                )))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }
            if let Some(mime) = mime_type {
                query = query.filter(document_versions::mime_type.eq(mime));
            }

            let extract_text_null = diesel::dsl::sql::<diesel::sql_types::Integer>(
                "CASE WHEN documents.extracted_text IS NULL THEN 0 ELSE 1 END",
            );
            query
                .select((documents::id, documents::analysis_priority, extract_text_null))
                .distinct()
                .order((
                    documents::analysis_priority.desc(),
                    diesel::dsl::sql::<diesel::sql_types::Integer>(
                        "CASE WHEN documents.extracted_text IS NULL THEN 0 ELSE 1 END",
                    )
                    .asc(),
                    documents::id.asc(),
                ))
                .load::<(String, i32, i32)>(&mut conn)
                .await
                .map(|rows| rows.into_iter().map(|(id, _, _)| id).collect())
        })
    }

    /// Get documents needing OCR with optional source/mime/cursor filters.
    #[deprecated(note = "Use get_needing_analysis(\"ocr\", ...) instead")]
    pub async fn get_needing_ocr_filtered(
        &self,
        limit: usize,
        source_id: Option<&str>,
        mime_type: Option<&str>,
        after_id: Option<&str>,
    ) -> Result<Vec<Document>, DieselError> {
        self.get_needing_analysis("ocr", limit, source_id, mime_type, after_id, 12)
            .await
    }

    /// Set the analysis priority for a document.
    ///
    /// Higher values = processed first. Default is 0.
    pub async fn set_analysis_priority(
        &self,
        id: &str,
        priority: i32,
    ) -> Result<(), DieselError> {
        let now = Utc::now().to_rfc3339();
        with_conn!(self.pool, conn, {
            diesel::update(documents::table.find(id))
                .set((
                    documents::analysis_priority.eq(priority),
                    documents::updated_at.eq(&now),
                ))
                .execute(&mut conn)
                .await?;
            Ok::<_, DieselError>(())
        })
    }

    /// Finalize document - mark as indexed.
    pub async fn finalize_document(&self, id: &str) -> Result<(), DieselError> {
        self.update_status(id, DocumentStatus::Indexed).await
    }

    /// Finalize pending documents - mark documents with all pages complete as indexed.
    pub async fn finalize_pending_documents(&self) -> Result<u64, DieselError> {
        let now = Utc::now().to_rfc3339();
        let count: usize = with_conn!(self.pool, conn, {
            diesel::update(documents::table.filter(documents::status.eq("ocr_complete")))
                .set((
                    documents::status.eq("indexed"),
                    documents::updated_at.eq(&now),
                ))
                .execute(&mut conn)
                .await
        })?;

        Ok(count as u64)
    }

    /// Reset annotations for documents, allowing them to be re-annotated.
    /// Sets status back to ocr_complete and clears synopsis/tags.
    pub async fn reset_annotations(&self, source_id: Option<&str>) -> Result<u64, DieselError> {
        let count: u64 = with_conn!(self.pool, conn, {
            let mut query = diesel::update(documents::table)
                .filter(documents::status.eq("indexed"))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            query
                .set((
                    documents::status.eq("ocr_complete"),
                    documents::synopsis.eq(None::<String>),
                    documents::tags.eq(None::<String>),
                ))
                .execute(&mut conn)
                .await
        })? as u64;

        Ok(count)
    }

    /// Count documents that have been annotated (status = indexed).
    pub async fn count_annotated(&self, source_id: Option<&str>) -> Result<u64, DieselError> {
        with_conn!(self.pool, conn, {
            let mut query = documents::table
                .filter(documents::status.eq("indexed"))
                .into_boxed();

            if let Some(sid) = source_id {
                query = query.filter(documents::source_id.eq(sid));
            }

            query
                .count()
                .get_result::<i64>(&mut conn)
                .await
                .map(|c| c as u64)
        })
    }

    /// Update synopsis and tags for a document.
    pub async fn update_synopsis_and_tags(
        &self,
        id: &str,
        synopsis: Option<&str>,
        tags: &[String],
    ) -> Result<(), DieselError> {
        let now = Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());

        with_conn!(self.pool, conn, {
            diesel::update(documents::table.find(id))
                .set((
                    documents::synopsis.eq(synopsis),
                    documents::tags.eq(&tags_json),
                    documents::status.eq("indexed"),
                    documents::updated_at.eq(&now),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::diesel_document::tests::setup_test_db;

    #[test]
    fn test_validate_identifier_accepts_valid() {
        assert!(validate_identifier("date_detection").is_ok());
        assert!(validate_identifier("llm_v2").is_ok());
        assert!(validate_identifier("entity_extraction").is_ok());
    }

    #[test]
    fn test_validate_identifier_rejects_sql_injection() {
        assert!(validate_identifier("'; DROP TABLE").is_err());
        assert!(validate_identifier("' OR '1'='1").is_err());
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("valid-name").is_err());
        assert!(validate_identifier("name; SELECT").is_err());
    }

    #[tokio::test]
    async fn test_get_by_tag_with_sql_metacharacters() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo.get_by_tag("'; DROP TABLE documents; --", None).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_count_documents_needing_annotation_rejects_injection() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo
            .count_documents_needing_annotation("'; DROP TABLE documents; --", 1, None)
            .await;
        assert!(result.is_err());
    }
}
