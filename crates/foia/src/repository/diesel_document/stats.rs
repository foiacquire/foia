//! Document statistics queries (MIME type and category counts).

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, MimeCount};
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::with_conn;

impl DieselDocumentRepository {
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
}
