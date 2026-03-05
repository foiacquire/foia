//! Timeline bucketing queries for document date visualization.

use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::repository::pool::DieselError;
use crate::with_conn;

impl DieselDocumentRepository {
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
}
