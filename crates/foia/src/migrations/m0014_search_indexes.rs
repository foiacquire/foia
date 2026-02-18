use cetane::prelude::*;

pub fn migration() -> Migration {
    Migration::new("0014_search_indexes")
        .depends_on(&["0009_document_entities", "0006_page_ocr_results"])
        // GIN index for full-text search on page content (Postgres only)
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    r#"CREATE INDEX IF NOT EXISTS idx_pages_fts
                       ON document_pages
                       USING GIN (to_tsvector('english',
                           COALESCE(final_text, ocr_text, pdf_text, '')))"#,
                ),
        )
        // Entity type index for top_entities() GROUP BY queries
        .operation(
            RunSql::new(
                "CREATE INDEX IF NOT EXISTS idx_document_entities_entity_type ON document_entities(entity_type)",
            ),
        )
        // Partial index for geocoded entity lookups
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    "CREATE INDEX IF NOT EXISTS idx_document_entities_geocoded ON document_entities(latitude, longitude) WHERE latitude IS NOT NULL",
                )
                .for_backend(
                    "postgres",
                    "CREATE INDEX IF NOT EXISTS idx_document_entities_geocoded ON document_entities(latitude, longitude) WHERE latitude IS NOT NULL",
                ),
        )
}
