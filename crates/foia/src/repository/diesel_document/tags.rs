//! Tag search and retrieval queries.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, TagRow};
use crate::models::Document;
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::with_conn_split;

impl DieselDocumentRepository {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::diesel_document::tests::setup_test_db;

    #[tokio::test]
    async fn test_get_by_tag_with_sql_metacharacters() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let result = repo.get_by_tag("'; DROP TABLE documents; --", None).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
