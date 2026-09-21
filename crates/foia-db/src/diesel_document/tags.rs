//! Tag search and retrieval queries.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, TagRow};
use foia_models::Document;
use crate::pool::DieselError;
use crate::schema::documents;
use crate::with_conn_split;

impl DieselDocumentRepository {
    /// Search tags by prefix in document metadata.
    /// Tags are stored as JSON arrays in the metadata field.
    pub async fn search_tags(&self, query: &str) -> Result<Vec<String>, DieselError> {
        use crate::sea_tables::Documents;
        use sea_query::{Alias, DynIden, Expr, Func, Query, TableRef};

        let pattern = format!("%{}%", query.to_lowercase());

        with_conn_split!(self.pool,
            sqlite: conn => {
                let json_extract = Func::cust(Alias::new("json_extract"))
                    .arg(Expr::col(Documents::Metadata))
                    .arg(Expr::cust("'$.tags'"));
                let json_each = Func::cust(Alias::new("json_each")).arg(json_extract);

                let stmt = Query::select()
                    .distinct()
                    .expr_as(Expr::cust("value"), Alias::new("tag"))
                    .from(Documents::Table)
                    .from(TableRef::FunctionCall(
                        json_each,
                        DynIden::new(Alias::new("_je")),
                    ))
                    .and_where(Expr::cust("LOWER(value) LIKE ?"))
                    .order_by(Alias::new("value"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);
                let sql = format!("{sql} LIMIT 100");
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(&pattern),
                    &mut conn,
                )
                .await?;
                Ok(results.into_iter().map(|r| r.tag).collect())
            },
            postgres: conn => {
                let tags_col = Alias::new("tags");
                let jsonb_elements =
                    Func::cust(Alias::new("jsonb_array_elements_text")).arg(
                        Expr::col((Documents::Table, tags_col.clone()))
                            .cast_as(Alias::new("jsonb")),
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
                    .and_where(
                        Expr::col((Documents::Table, tags_col))
                            .ne(Expr::cust("'[]'")),
                    )
                    .and_where(Expr::cust("LOWER(tag) LIKE $1"))
                    .order_by(Alias::new("tag"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);
                let sql = format!("{sql} LIMIT 100");
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql)
                        .bind::<diesel::sql_types::Text, _>(&pattern),
                    &mut conn,
                )
                .await?;
                Ok(results.into_iter().map(|r| r.tag).collect())
            }
        )
    }

    /// Get all unique tags from document metadata.
    pub async fn get_all_tags(&self) -> Result<Vec<String>, DieselError> {
        use crate::sea_tables::Documents;
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
                    .and_where(
                        Expr::col((Documents::Table, tags_col.clone()))
                            .ne(Expr::cust("'[]'")),
                    )
                    .order_by(Alias::new("value"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql),
                    &mut conn,
                )
                .await?;
                Ok(results.into_iter().map(|r| r.tag).collect())
            },
            postgres: conn => {
                let jsonb_elements =
                    Func::cust(Alias::new("jsonb_array_elements_text")).arg(
                        Expr::col((Documents::Table, tags_col.clone()))
                            .cast_as(Alias::new("jsonb")),
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
                    .and_where(
                        Expr::col((Documents::Table, tags_col))
                            .ne(Expr::cust("'[]'")),
                    )
                    .order_by(Alias::new("tag"), sea_query::Order::Asc)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);
                let results: Vec<TagRow> = diesel_async::RunQueryDsl::load(
                    diesel::sql_query(&sql),
                    &mut conn,
                )
                .await?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use foia_models::DocumentStatus;
    use crate::diesel_document::tests::setup_test_db;
    use chrono::Utc;

    async fn save_doc_with_tags(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        tags: &[&str],
    ) {
        let tags_vec: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        // metadata.tags is read by search_tags (SQLite json_extract path)
        let metadata = serde_json::json!({ "tags": tags });
        let doc = Document {
            id: id.to_string(),
            source_id: source.to_string(),
            title: format!("Doc {id}"),
            source_url: format!("https://example.com/{id}"),
            extracted_text: None,
            synopsis: None,
            tags: tags_vec.clone(),
            status: DocumentStatus::Pending,
            metadata,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        };
        repo.save(&doc).await.unwrap();
        // documents.tags column is written by update_synopsis_and_tags,
        // which get_all_tags reads via json_each(documents.tags)
        repo.update_synopsis_and_tags(id, None, &tags_vec)
            .await
            .unwrap();
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
    async fn test_get_by_tag_returns_matching_docs() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_tags(&repo, "d1", "src", &["foia", "government"]).await;
        save_doc_with_tags(&repo, "d2", "src", &["foia"]).await;
        save_doc_with_tags(&repo, "d3", "src", &["other"]).await;

        let foia_docs = repo.get_by_tag("foia", None).await.unwrap();
        assert_eq!(foia_docs.len(), 2);

        let other_docs = repo.get_by_tag("other", None).await.unwrap();
        assert_eq!(other_docs.len(), 1);
        assert_eq!(other_docs[0].id, "d3");
    }

    #[tokio::test]
    async fn test_get_by_tag_filters_by_source() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_tags(&repo, "d1", "src-a", &["foia"]).await;
        save_doc_with_tags(&repo, "d2", "src-b", &["foia"]).await;

        let filtered = repo.get_by_tag("foia", Some("src-a")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "d1");
    }

    #[tokio::test]
    async fn test_get_all_tags_returns_unique_tags() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_tags(&repo, "d1", "src", &["alpha", "beta"]).await;
        save_doc_with_tags(&repo, "d2", "src", &["beta", "gamma"]).await;

        let all_tags = repo.get_all_tags().await.unwrap();
        assert_eq!(all_tags.len(), 3);
        assert!(all_tags.contains(&"alpha".to_string()));
        assert!(all_tags.contains(&"beta".to_string()));
        assert!(all_tags.contains(&"gamma".to_string()));
    }

    #[tokio::test]
    async fn test_get_all_tags_empty_db() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let all_tags = repo.get_all_tags().await.unwrap();
        assert!(all_tags.is_empty());
    }

    #[tokio::test]
    async fn test_search_tags_matches_substring() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_tags(&repo, "d1", "src", &["government", "governance"]).await;
        save_doc_with_tags(&repo, "d2", "src", &["policy"]).await;

        let results = repo.search_tags("govern").await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.contains(&"government".to_string()));
        assert!(results.contains(&"governance".to_string()));
    }
}
