//! Diesel-based source repository for SQLite.
//!
//! This module provides async database access for source operations using Diesel ORM.
//! Since diesel-async only supports Postgres/MySQL, SQLite operations use sync Diesel
//! wrapped in spawn_blocking.

use chrono::{DateTime, Utc};
use diesel::prelude::*;

use super::diesel_models::{NewSource, SourceRecord};
use super::diesel_pool::{run_blocking, SqlitePool};
use super::{parse_datetime, parse_datetime_opt};
use crate::models::{Source, SourceType};
use crate::schema::sources;

/// Convert a database record to a domain model.
impl From<SourceRecord> for Source {
    fn from(record: SourceRecord) -> Self {
        Source {
            id: record.id,
            source_type: SourceType::from_str(&record.source_type).unwrap_or(SourceType::Custom),
            name: record.name,
            base_url: record.base_url,
            metadata: serde_json::from_str(&record.metadata).unwrap_or_default(),
            created_at: parse_datetime(&record.created_at),
            last_scraped: parse_datetime_opt(record.last_scraped),
        }
    }
}

/// Diesel-based source repository with compile-time query checking.
#[derive(Clone)]
pub struct DieselSourceRepository {
    pool: SqlitePool,
}

impl DieselSourceRepository {
    /// Create a new Diesel source repository with an existing pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get a source by ID.
    pub async fn get(&self, id: &str) -> Result<Option<Source>, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            sources::table
                .find(&id)
                .first::<SourceRecord>(conn)
                .optional()
        })
        .await
        .map(|opt| opt.map(Source::from))
    }

    /// Get all sources.
    pub async fn get_all(&self) -> Result<Vec<Source>, diesel::result::Error> {
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            sources::table.load::<SourceRecord>(conn)
        })
        .await
        .map(|records| records.into_iter().map(Source::from).collect())
    }

    /// Save a source (insert or update using ON CONFLICT).
    pub async fn save(&self, source: &Source) -> Result<(), diesel::result::Error> {
        let metadata_json = serde_json::to_string(&source.metadata).unwrap_or_else(|_| "{}".to_string());
        let created_at = source.created_at.to_rfc3339();
        let last_scraped = source.last_scraped.map(|dt| dt.to_rfc3339());
        let source_type = source.source_type.as_str().to_string();

        let new_source = NewSource {
            id: &source.id,
            source_type: &source_type,
            name: &source.name,
            base_url: &source.base_url,
            metadata: &metadata_json,
            created_at: &created_at,
            last_scraped: last_scraped.as_deref(),
        };

        let id = source.id.clone();
        let name = source.name.clone();
        let base_url = source.base_url.clone();
        let source_type_owned = source_type.clone();
        let metadata_owned = metadata_json.clone();
        let created_at_owned = created_at.clone();
        let last_scraped_owned = last_scraped.clone();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            // Use replace_into for SQLite upsert
            diesel::replace_into(sources::table)
                .values((
                    sources::id.eq(&id),
                    sources::source_type.eq(&source_type_owned),
                    sources::name.eq(&name),
                    sources::base_url.eq(&base_url),
                    sources::metadata.eq(&metadata_owned),
                    sources::created_at.eq(&created_at_owned),
                    sources::last_scraped.eq(&last_scraped_owned),
                ))
                .execute(conn)?;
            Ok(())
        })
        .await
    }

    /// Delete a source.
    pub async fn delete(&self, id: &str) -> Result<bool, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            let rows = diesel::delete(sources::table.find(&id)).execute(conn)?;
            Ok(rows > 0)
        })
        .await
    }

    /// Check if a source exists.
    pub async fn exists(&self, id: &str) -> Result<bool, diesel::result::Error> {
        let id = id.to_string();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            use diesel::dsl::count_star;
            let count: i64 = sources::table
                .filter(sources::id.eq(&id))
                .select(count_star())
                .first(conn)?;
            Ok(count > 0)
        })
        .await
    }

    /// Update last scraped timestamp.
    pub async fn update_last_scraped(
        &self,
        id: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<(), diesel::result::Error> {
        let id = id.to_string();
        let ts = timestamp.to_rfc3339();
        let pool = self.pool.clone();

        run_blocking(pool, move |conn| {
            diesel::update(sources::table.find(&id))
                .set(sources::last_scraped.eq(Some(&ts)))
                .execute(conn)?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::diesel_pool::create_diesel_pool_from_url;
    use tempfile::tempdir;

    async fn setup_test_db() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_url = format!("{}", db_path.display());

        let pool = create_diesel_pool_from_url(&db_url).unwrap();

        // Create tables
        run_blocking(pool.clone(), |conn| {
            diesel::sql_query(
                r#"CREATE TABLE IF NOT EXISTS sources (
                    id TEXT PRIMARY KEY,
                    source_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    base_url TEXT NOT NULL,
                    metadata TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    last_scraped TEXT
                )"#,
            )
            .execute(conn)?;
            Ok(())
        })
        .await
        .unwrap();

        (pool, dir)
    }

    #[tokio::test]
    async fn test_source_crud() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselSourceRepository::new(pool);

        // Create a source
        let source = Source::new(
            "test-source".to_string(),
            SourceType::Custom,
            "Test Source".to_string(),
            "https://example.com".to_string(),
        );

        // Save
        repo.save(&source).await.unwrap();

        // Check exists
        assert!(repo.exists("test-source").await.unwrap());

        // Get
        let fetched = repo.get("test-source").await.unwrap().unwrap();
        assert_eq!(fetched.name, "Test Source");
        assert_eq!(fetched.base_url, "https://example.com");

        // Get all
        let all = repo.get_all().await.unwrap();
        assert_eq!(all.len(), 1);

        // Delete
        let deleted = repo.delete("test-source").await.unwrap();
        assert!(deleted);

        // Verify deleted
        assert!(!repo.exists("test-source").await.unwrap());
    }
}
