//! Metadata-based annotation queries (legacy annotation system, date estimation, synopsis/tags).

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::models::Document;
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

impl DieselDocumentRepository {
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
        if annotation_type == "llm_summary" {
            return self.count_needing_summarization(source_id).await;
        }

        if annotation_type == "date_detection" {
            return self
                .count_documents_needing_date_estimation(source_id)
                .await;
        }

        validate_identifier(annotation_type)?;

        use diesel::dsl::count_star;

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
        if annotation_type == "llm_summary" {
            return self.get_needing_summarization(limit).await;
        }

        if annotation_type == "date_detection" {
            return self
                .get_documents_needing_date_estimation(source_id, limit)
                .await;
        }

        validate_identifier(annotation_type)?;

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
    use crate::models::{DocumentStatus, DocumentVersion};
    use crate::repository::diesel_document::tests::setup_test_db;

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

    async fn save_doc(repo: &DieselDocumentRepository, id: &str, source: &str, status: DocumentStatus) {
        let doc = make_doc(id, source, status);
        repo.save(&doc).await.unwrap();
        let version = DocumentVersion {
            id: 0,
            content_hash: format!("hash-{id}"),
            content_hash_blake3: None,
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
        repo.add_version(id, &version).await.unwrap();
    }

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
    async fn test_count_documents_needing_annotation_rejects_injection() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo
            .count_documents_needing_annotation("'; DROP TABLE documents; --", 1, None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_count_documents_needing_annotation() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        // All docs need "ner" annotation (no annotations recorded)
        let count = repo
            .count_documents_needing_annotation("ner", 1, None)
            .await
            .unwrap();
        assert_eq!(count, 3);

        // With source filter
        let count_a = repo
            .count_documents_needing_annotation("ner", 1, Some("src-a"))
            .await
            .unwrap();
        assert_eq!(count_a, 2);

        // Record annotation for d1 at version 1
        repo.record_annotation("d1", "ner", 1, Some("data"), None)
            .await
            .unwrap();

        // d1 no longer needs ner v1
        let count = repo
            .count_documents_needing_annotation("ner", 1, None)
            .await
            .unwrap();
        assert_eq!(count, 2);

        // d1 still needs ner v2
        let count_v2 = repo
            .count_documents_needing_annotation("ner", 2, None)
            .await
            .unwrap();
        assert_eq!(count_v2, 3);
    }

    #[tokio::test]
    async fn test_get_documents_needing_annotation() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src", DocumentStatus::Pending).await;

        let docs = repo
            .get_documents_needing_annotation("ner", 1, None, 10)
            .await
            .unwrap();
        assert_eq!(docs.len(), 2);

        let limited = repo
            .get_documents_needing_annotation("ner", 1, None, 1)
            .await
            .unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[tokio::test]
    async fn test_count_needing_date_estimation() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        // All docs need date estimation (metadata has no estimated_date)
        let count = repo
            .count_documents_needing_date_estimation(None)
            .await
            .unwrap();
        assert_eq!(count, 3);

        let count_a = repo
            .count_documents_needing_date_estimation(Some("src-a"))
            .await
            .unwrap();
        assert_eq!(count_a, 2);

        // Update d1 with estimated_date
        repo.update_estimated_date("d1", Utc::now(), "high", "llm")
            .await
            .unwrap();

        let count_after = repo
            .count_documents_needing_date_estimation(None)
            .await
            .unwrap();
        assert_eq!(count_after, 2);
    }

    #[tokio::test]
    async fn test_update_estimated_date() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src", DocumentStatus::Pending).await;

        let date = Utc::now();
        repo.update_estimated_date("d1", date, "high", "llm")
            .await
            .unwrap();

        let doc = repo.get("d1").await.unwrap().unwrap();
        let ed = &doc.metadata["estimated_date"];
        assert_eq!(ed["confidence"].as_str(), Some("high"));
        assert_eq!(ed["source"].as_str(), Some("llm"));
        assert!(ed["date"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_record_annotation() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src", DocumentStatus::Pending).await;

        repo.record_annotation("d1", "ner", 2, Some("entities found"), None)
            .await
            .unwrap();

        let doc = repo.get("d1").await.unwrap().unwrap();
        let ann = &doc.metadata["annotations"]["ner"];
        assert_eq!(ann["version"].as_i64(), Some(2));
        assert_eq!(ann["data"].as_str(), Some("entities found"));
        assert!(ann["error"].is_null());
    }

    #[tokio::test]
    async fn test_reset_annotations() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        // Set d1 and d2 to indexed with synopsis/tags
        repo.update_synopsis_and_tags(
            "d1",
            Some("Synopsis 1"),
            &["tag1".to_string()],
        )
        .await
        .unwrap();
        repo.update_synopsis_and_tags(
            "d2",
            Some("Synopsis 2"),
            &["tag2".to_string()],
        )
        .await
        .unwrap();

        // Reset only src-a
        let reset = repo.reset_annotations(Some("src-a")).await.unwrap();
        assert_eq!(reset, 2);

        let d1 = repo.get("d1").await.unwrap().unwrap();
        assert_eq!(d1.status, DocumentStatus::OcrComplete);
        assert!(d1.synopsis.is_none());
        assert!(d1.tags.is_empty());

        // d3 should be unchanged (different source, not indexed)
        let d3 = repo.get("d3").await.unwrap().unwrap();
        assert_eq!(d3.status, DocumentStatus::Pending);
    }

    #[tokio::test]
    async fn test_count_annotated() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        assert_eq!(repo.count_annotated(None).await.unwrap(), 0);

        // Mark d1 and d3 as indexed
        repo.update_synopsis_and_tags("d1", Some("S1"), &[])
            .await
            .unwrap();
        repo.update_synopsis_and_tags("d3", Some("S3"), &[])
            .await
            .unwrap();

        assert_eq!(repo.count_annotated(None).await.unwrap(), 2);
        assert_eq!(repo.count_annotated(Some("src-a")).await.unwrap(), 1);
        assert_eq!(repo.count_annotated(Some("src-b")).await.unwrap(), 1);
    }
}
