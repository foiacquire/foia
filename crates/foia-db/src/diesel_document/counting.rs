//! Basic document counting queries.

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::DieselDocumentRepository;
use crate::pool::DieselError;
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

#[cfg(test)]
mod tests {
    use super::*;
    use foia_models::{Document, DocumentStatus};
    use crate::diesel_document::tests::setup_test_db;
    use chrono::Utc;

    async fn save_doc(repo: &DieselDocumentRepository, id: &str, source: &str, status: DocumentStatus) {
        let doc = Document {
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
        };
        repo.save(&doc).await.unwrap();
    }

    #[tokio::test]
    async fn test_count_empty() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_count_multiple() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-b", DocumentStatus::Downloaded).await;
        assert_eq!(repo.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_count_by_source() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;
        assert_eq!(repo.count_by_source("src-a").await.unwrap(), 2);
        assert_eq!(repo.count_by_source("src-b").await.unwrap(), 1);
        assert_eq!(repo.count_by_source("src-missing").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_get_all_source_counts() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;
        let counts = repo.get_all_source_counts().await.unwrap();
        assert_eq!(counts.get("src-a"), Some(&2));
        assert_eq!(counts.get("src-b"), Some(&1));
    }

    #[tokio::test]
    async fn test_count_by_status() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Downloaded).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        let all = repo.count_all_by_status().await.unwrap();
        assert_eq!(all.get("pending"), Some(&2));
        assert_eq!(all.get("downloaded"), Some(&1));

        let src_a = repo.count_by_status(Some("src-a")).await.unwrap();
        assert_eq!(src_a.get("pending"), Some(&1));
        assert_eq!(src_a.get("downloaded"), Some(&1));
    }

    #[tokio::test]
    async fn test_get_source_status_counts() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        save_doc(&repo, "d1", "src-a", DocumentStatus::Pending).await;
        save_doc(&repo, "d2", "src-a", DocumentStatus::Downloaded).await;
        save_doc(&repo, "d3", "src-b", DocumentStatus::Pending).await;

        let result = repo.get_source_status_counts().await.unwrap();
        assert_eq!(result["src-a"]["pending"], 1);
        assert_eq!(result["src-a"]["downloaded"], 1);
        assert_eq!(result["src-b"]["pending"], 1);
    }
}
