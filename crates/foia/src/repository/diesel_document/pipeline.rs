//! Analysis pipeline queries (counting/fetching documents needing analysis, priority, finalization).

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::models::{Document, DocumentStatus};
use crate::repository::models::DocumentRecord;
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::with_conn;

impl DieselDocumentRepository {
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
}
