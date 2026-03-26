//! Document statistics queries (MIME type and category counts).

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, MimeCount};
use crate::pool::DieselError;
use crate::schema::documents;
use crate::with_conn;

impl DieselDocumentRepository {
    /// Get type statistics - count documents by MIME type.
    pub async fn get_type_stats(&self) -> Result<HashMap<String, u64>, DieselError> {
        use crate::pool::build_sql;
        use crate::sea_tables::DocumentVersions;
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
        let mime_col = Expr::col((DocumentVersions::Table, DocumentVersions::MimeType));
        let doc_id_col = Expr::col((DocumentVersions::Table, DocumentVersions::DocumentId));

        let stmt = Query::select()
            .expr_as(
                Expr::case(mime_col.clone().is_null(), Expr::val("unknown"))
                    .finally(mime_col.clone()),
                Alias::new("mime_type"),
            )
            .expr_as(
                doc_id_col.count_distinct(),
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
                use crate::pool::build_sql;
                use crate::sea_tables::FileCategories;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use foia_models::{Document, DocumentStatus, DocumentVersion};
    use crate::diesel_document::tests::setup_test_db;
    use chrono::Utc;

    async fn save_doc_with_version(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        mime_type: &str,
        category: &str,
    ) {
        let doc = Document {
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
        };
        repo.save(&doc).await.unwrap();
        // Update category_id directly (bypassing the macro since test helpers don't return Result)
        match &repo.pool {
            crate::pool::DbPool::Sqlite(pool) => {
                let mut conn = pool.get().await.unwrap();
                diesel::update(documents::table.find(id))
                    .set(documents::category_id.eq(category))
                    .execute(&mut conn)
                    .await
                    .unwrap();
            }
            #[cfg(feature = "postgres")]
            _ => unreachable!("tests use SQLite"),
        }

        let version = DocumentVersion {
            id: 0,
            content_hash: format!("hash-{id}"),
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
        };
        repo.add_version(id, &version).await.unwrap();
    }

    #[tokio::test]
    async fn test_get_type_stats() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src", "application/pdf", "documents").await;
        save_doc_with_version(&repo, "d2", "src", "application/pdf", "documents").await;
        save_doc_with_version(&repo, "d3", "src", "image/png", "images").await;

        let stats = repo.get_type_stats().await.unwrap();
        assert_eq!(stats.get("application/pdf"), Some(&2));
        assert_eq!(stats.get("image/png"), Some(&1));
    }

    #[tokio::test]
    async fn test_get_category_stats_with_source_filter() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_version(&repo, "d1", "src-a", "application/pdf", "documents").await;
        save_doc_with_version(&repo, "d2", "src-a", "application/pdf", "documents").await;
        save_doc_with_version(&repo, "d3", "src-b", "image/png", "images").await;

        let stats = repo.get_category_stats(Some("src-a")).await.unwrap();
        assert_eq!(stats.get("documents"), Some(&2));
        assert_eq!(stats.get("images"), None);
    }

    #[tokio::test]
    async fn test_get_type_stats_empty() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let stats = repo.get_type_stats().await.unwrap();
        assert!(stats.is_empty());
    }
}
