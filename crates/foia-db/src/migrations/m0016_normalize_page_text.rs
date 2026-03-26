use cetane::prelude::*;

pub fn migration() -> Migration {
    Migration::new("0016_normalize_page_text")
        .depends_on(&["0014_search_indexes"])
        // Step 1a: Copy pdf_text to page_ocr_results where not already present
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    r#"INSERT OR IGNORE INTO page_ocr_results (page_id, backend, text, char_count, word_count, created_at)
SELECT
    dp.id,
    'pdftotext',
    dp.pdf_text,
    LENGTH(dp.pdf_text),
    LENGTH(dp.pdf_text) - LENGTH(REPLACE(dp.pdf_text, ' ', '')) + 1,
    dp.created_at
FROM document_pages dp
WHERE dp.pdf_text IS NOT NULL AND dp.pdf_text != ''
  AND NOT EXISTS (
    SELECT 1 FROM page_ocr_results por
    WHERE por.page_id = dp.id AND por.backend = 'pdftotext'
  )"#,
                )
                .for_backend(
                    "postgres",
                    r#"DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'document_pages' AND column_name = 'pdf_text') THEN
    INSERT INTO page_ocr_results (page_id, backend, text, char_count, word_count, created_at)
    SELECT dp.id, 'pdftotext', dp.pdf_text, LENGTH(dp.pdf_text),
           array_length(regexp_split_to_array(dp.pdf_text, '\s+'), 1), dp.created_at
    FROM document_pages dp
    WHERE dp.pdf_text IS NOT NULL AND dp.pdf_text != ''
      AND NOT EXISTS (SELECT 1 FROM page_ocr_results por WHERE por.page_id = dp.id AND por.backend = 'pdftotext')
    ON CONFLICT DO NOTHING;
  END IF;
END $$"#,
                ),
        )
        // Step 1b: Copy ocr_text to page_ocr_results (pre-m0006 OCR data)
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    r#"INSERT OR IGNORE INTO page_ocr_results (page_id, backend, text, char_count, word_count, created_at)
SELECT
    dp.id,
    'legacy_ocr',
    dp.ocr_text,
    LENGTH(dp.ocr_text),
    LENGTH(dp.ocr_text) - LENGTH(REPLACE(dp.ocr_text, ' ', '')) + 1,
    dp.created_at
FROM document_pages dp
WHERE dp.ocr_text IS NOT NULL AND dp.ocr_text != ''
  AND NOT EXISTS (
    SELECT 1 FROM page_ocr_results por
    WHERE por.page_id = dp.id AND por.backend = 'legacy_ocr'
  )"#,
                )
                .for_backend(
                    "postgres",
                    r#"DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'document_pages' AND column_name = 'ocr_text') THEN
    INSERT INTO page_ocr_results (page_id, backend, text, char_count, word_count, created_at)
    SELECT dp.id, 'legacy_ocr', dp.ocr_text, LENGTH(dp.ocr_text),
           array_length(regexp_split_to_array(dp.ocr_text, '\s+'), 1), dp.created_at
    FROM document_pages dp
    WHERE dp.ocr_text IS NOT NULL AND dp.ocr_text != ''
      AND NOT EXISTS (SELECT 1 FROM page_ocr_results por WHERE por.page_id = dp.id AND por.backend = 'legacy_ocr')
    ON CONFLICT DO NOTHING;
  END IF;
END $$"#,
                ),
        )
        // Step 2: SQLite backfill (needed before table rebuild). Postgres does this after rename.
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    r#"UPDATE document_pages
SET final_text = COALESCE(final_text, ocr_text, pdf_text)
WHERE final_text IS NULL AND (ocr_text IS NOT NULL OR pdf_text IS NOT NULL)"#,
                )
                .for_backend("postgres", "SELECT 1"),
        )
        // Step 3: Rename final_text -> search_text
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    r#"CREATE TABLE document_pages_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id TEXT NOT NULL,
    version_id INTEGER NOT NULL,
    page_number INTEGER NOT NULL,
    search_text TEXT,
    ocr_status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (document_id) REFERENCES documents(id),
    FOREIGN KEY (version_id) REFERENCES document_versions(id),
    UNIQUE(document_id, version_id, page_number)
);
INSERT INTO document_pages_new (id, document_id, version_id, page_number, search_text, ocr_status, created_at, updated_at)
SELECT id, document_id, version_id, page_number, final_text, ocr_status, created_at, updated_at
FROM document_pages;
DROP TABLE document_pages;
ALTER TABLE document_pages_new RENAME TO document_pages;
CREATE INDEX idx_document_pages_document ON document_pages(document_id);
CREATE INDEX idx_document_pages_version ON document_pages(version_id);
CREATE INDEX idx_document_pages_ocr_status ON document_pages(ocr_status);
CREATE INDEX idx_pages_doc_version ON document_pages(document_id, version_id);
CREATE INDEX idx_pages_with_text ON document_pages(document_id) WHERE search_text IS NOT NULL"#,
                )
                .for_backend(
                    "postgres",
                    r#"DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'document_pages' AND column_name = 'final_text') THEN
    ALTER TABLE document_pages RENAME COLUMN final_text TO search_text;
  END IF;
END $$"#,
                ),
        )
        // Step 4: Postgres: backfill search_text from page_ocr_results (reads small indexed
        // table instead of scanning huge text columns). Only updates rows where search_text
        // is NULL but page_ocr_results has data.
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    r#"UPDATE document_pages dp
SET search_text = por.text
FROM (
    SELECT DISTINCT ON (page_id) page_id, text
    FROM page_ocr_results
    WHERE text IS NOT NULL
    ORDER BY page_id, char_count DESC NULLS LAST
) por
WHERE dp.id = por.page_id AND dp.search_text IS NULL"#,
                ),
        )
        // Step 5a: Drop pdf_text column
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    "ALTER TABLE document_pages DROP COLUMN IF EXISTS pdf_text",
                ),
        )
        // Step 5b: Drop ocr_text column
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    "ALTER TABLE document_pages DROP COLUMN IF EXISTS ocr_text",
                ),
        )
        // Step 6a: Drop old FTS index
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend("postgres", "DROP INDEX IF EXISTS idx_pages_fts"),
        )
        // Step 6b: Rebuild FTS index on search_text
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    r#"CREATE INDEX IF NOT EXISTS idx_pages_fts ON document_pages
  USING GIN (to_tsvector('english', COALESCE(search_text, '')))"#,
                ),
        )
        // Step 7a: Drop old partial index
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend("postgres", "DROP INDEX IF EXISTS idx_pages_with_text"),
        )
        // Step 7b: Rebuild partial index on search_text
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    "CREATE INDEX IF NOT EXISTS idx_pages_with_text ON document_pages(document_id) WHERE search_text IS NOT NULL",
                ),
        )
}
