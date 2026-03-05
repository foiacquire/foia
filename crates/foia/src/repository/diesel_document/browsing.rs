//! Document browsing, filtering, and navigation queries.

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::models::Document;
use crate::repository::document::DocumentNavigation;
use crate::repository::models::DocumentRecord;
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::with_conn;

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
            let mut query = documents::table.into_boxed();

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
                    if is_desc {
                        query = query.order(documents::updated_at.desc());
                    } else {
                        query = query.order(documents::updated_at.asc());
                    }
                }
            }

            query.limit(limit).offset(offset).load(&mut conn).await
        })?;

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

            let mut latest_versions: HashMap<&str, (Option<String>, String, i32, String)> =
                HashMap::new();
            for (doc_id, filename, mime, size, acquired) in &version_rows {
                latest_versions
                    .entry(doc_id.as_str())
                    .or_insert_with(|| (filename.clone(), mime.clone(), *size, acquired.clone()));
            }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DocumentStatus;
    use crate::repository::diesel_document::tests::setup_test_db;
    use chrono::Utc;

    async fn save_doc(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        status: DocumentStatus,
        title: &str,
    ) {
        let doc = Document {
            id: id.to_string(),
            source_id: source.to_string(),
            title: title.to_string(),
            source_url: format!("https://example.com/{id}"),
            extracted_text: None,
            synopsis: Some(format!("Synopsis for {id}")),
            tags: vec![],
            status,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        };
        repo.save(&doc).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_recent() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src", DocumentStatus::Pending, "First").await;
        save_doc(&repo, "d2", "src", DocumentStatus::Pending, "Second").await;

        let recent = repo.get_recent(1).await.unwrap();
        assert_eq!(recent.len(), 1);

        let all = repo.get_recent(10).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_browse_with_source_filter() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending, "Doc A1").await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending, "Doc A2").await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending, "Doc B1").await;

        let params = BrowseParams {
            source_id: Some("src-a"),
            limit: 10,
            ..Default::default()
        };
        let results = repo.browse(params).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|d| d.source_id == "src-a"));
    }

    #[tokio::test]
    async fn test_browse_with_status_filter() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src", DocumentStatus::Pending, "Pending").await;
        save_doc(&repo, "d2", "src", DocumentStatus::Downloaded, "Downloaded").await;

        let params = BrowseParams {
            status: Some("downloaded"),
            limit: 10,
            ..Default::default()
        };
        let results = repo.browse(params).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, DocumentStatus::Downloaded);
    }

    #[tokio::test]
    async fn test_browse_with_search_query() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src", DocumentStatus::Pending, "Alpha Report").await;
        save_doc(&repo, "d2", "src", DocumentStatus::Pending, "Beta Report").await;
        save_doc(&repo, "d3", "src", DocumentStatus::Pending, "Gamma Study").await;

        let params = BrowseParams {
            search_query: Some("Report"),
            limit: 10,
            ..Default::default()
        };
        let results = repo.browse(params).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_browse_pagination() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        for i in 0..5 {
            save_doc(&repo, &format!("d{i}"), "src", DocumentStatus::Pending, &format!("Doc {i}")).await;
        }

        let page1 = BrowseParams { limit: 2, offset: 0, ..Default::default() };
        let page2 = BrowseParams { limit: 2, offset: 2, ..Default::default() };
        let page3 = BrowseParams { limit: 2, offset: 4, ..Default::default() };

        assert_eq!(repo.browse(page1).await.unwrap().len(), 2);
        assert_eq!(repo.browse(page2).await.unwrap().len(), 2);
        assert_eq!(repo.browse(page3).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_browse_count() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending, "Doc 1").await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Downloaded, "Doc 2").await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending, "Doc 3").await;

        let total = repo.browse_count(None, None, &[], &[], None).await.unwrap();
        assert_eq!(total, 3);

        let src_a = repo.browse_count(Some("src-a"), None, &[], &[], None).await.unwrap();
        assert_eq!(src_a, 2);

        let filtered = repo.browse_count(None, Some("downloaded"), &[], &[], None).await.unwrap();
        assert_eq!(filtered, 1);
    }

    #[tokio::test]
    async fn test_get_document_navigation() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src", DocumentStatus::Pending, "First").await;
        save_doc(&repo, "d2", "src", DocumentStatus::Pending, "Second").await;
        save_doc(&repo, "d3", "src", DocumentStatus::Pending, "Third").await;

        let nav = repo.get_document_navigation("d2", "src").await.unwrap();
        assert_eq!(nav.prev_id.as_deref(), Some("d1"));
        assert_eq!(nav.next_id.as_deref(), Some("d3"));
        assert_eq!(nav.position, 2);
        assert_eq!(nav.total, 3);

        let first_nav = repo.get_document_navigation("d1", "src").await.unwrap();
        assert_eq!(first_nav.prev_id, None);
        assert_eq!(first_nav.next_id.as_deref(), Some("d2"));
    }
}
