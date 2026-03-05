//! Basic document counting queries.

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::repository::pool::DieselError;
use crate::schema::documents;
use crate::with_conn;

impl DieselDocumentRepository {
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
}
