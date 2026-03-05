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
        use sea_query::{Alias, Expr, ExprTrait, Func, Query};

        #[derive(diesel::QueryableByName)]
        struct TimelineBucket {
            #[diesel(sql_type = diesel::sql_types::Text)]
            date_bucket: String,
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            count: i64,
        }

        let coalesce = Func::coalesce([
            Expr::col(Documents::ManualDate).into(),
            Expr::col(Documents::EstimatedDate).into(),
        ]);

        let date_fn = Func::cust(Alias::new("date")).arg(coalesce.clone());

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, DocumentStatus};
    use crate::repository::diesel_document::tests::setup_test_db;
    use crate::schema::documents;
    use chrono::Utc;
    use diesel::prelude::*;

    async fn save_doc_with_date(
        repo: &DieselDocumentRepository,
        id: &str,
        source: &str,
        estimated_date: &str,
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
        match &repo.pool {
            crate::repository::pool::DbPool::Sqlite(pool) => {
                let mut conn = pool.get().await.unwrap();
                diesel::update(documents::table.find(id))
                    .set(documents::estimated_date.eq(estimated_date))
                    .execute(&mut conn)
                    .await
                    .unwrap();
            }
            #[cfg(feature = "postgres")]
            _ => unreachable!("tests use SQLite"),
        }
    }

    #[tokio::test]
    async fn test_get_timeline_buckets_empty() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let buckets = repo.get_timeline_buckets(None, None, None).await.unwrap();
        assert!(buckets.is_empty());
    }

    #[tokio::test]
    async fn test_get_timeline_buckets_groups_by_day() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_date(&repo, "d1", "src", "2024-01-15").await;
        save_doc_with_date(&repo, "d2", "src", "2024-01-15").await;
        save_doc_with_date(&repo, "d3", "src", "2024-01-16").await;

        let buckets = repo.get_timeline_buckets(None, None, None).await.unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].0, "2024-01-15");
        assert_eq!(buckets[0].2, 2);
        assert_eq!(buckets[1].0, "2024-01-16");
        assert_eq!(buckets[1].2, 1);
    }

    #[tokio::test]
    async fn test_get_timeline_buckets_date_range_filter() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_date(&repo, "d1", "src", "2024-01-10").await;
        save_doc_with_date(&repo, "d2", "src", "2024-01-15").await;
        save_doc_with_date(&repo, "d3", "src", "2024-01-20").await;

        let buckets = repo
            .get_timeline_buckets(None, Some("2024-01-12"), Some("2024-01-18"))
            .await
            .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].0, "2024-01-15");
    }

    #[tokio::test]
    async fn test_get_timeline_buckets_source_filter() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        save_doc_with_date(&repo, "d1", "src", "2024-01-15").await;
        save_doc_with_date(&repo, "d2", "other-src", "2024-01-15").await;

        let all = repo.get_timeline_buckets(None, None, None).await.unwrap();
        assert_eq!(all[0].2, 2);

        let src_only = repo.get_timeline_buckets(Some("src"), None, None).await.unwrap();
        assert_eq!(src_only[0].2, 1);
    }
}
