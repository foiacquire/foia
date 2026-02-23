use cetane::prelude::*;

pub fn migration() -> Migration {
    Migration::new("0016_normalize_page_text")
        .depends_on(&["0014_search_indexes"])
        // Step 1: Insert any missing pdftotext results (pages created after m0006)
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
                    r#"INSERT INTO page_ocr_results (page_id, backend, text, char_count, word_count, created_at)
SELECT
    dp.id,
    'pdftotext',
    dp.pdf_text,
    LENGTH(dp.pdf_text),
    array_length(regexp_split_to_array(dp.pdf_text, '\s+'), 1),
    dp.created_at
FROM document_pages dp
WHERE dp.pdf_text IS NOT NULL AND dp.pdf_text != ''
  AND NOT EXISTS (
    SELECT 1 FROM page_ocr_results por
    WHERE por.page_id = dp.id AND por.backend = 'pdftotext'
  )
ON CONFLICT DO NOTHING"#,
                ),
        )
        // Step 2: Backfill final_text where NULL but other text columns have data
        .operation(
            RunSql::new(
                r#"UPDATE document_pages
SET final_text = COALESCE(final_text, ocr_text, pdf_text)
WHERE final_text IS NULL AND (ocr_text IS NOT NULL OR pdf_text IS NOT NULL)"#,
            ),
        )
        // Step 3: Rename final_text -> search_text, drop pdf_text and ocr_text
        .operation(
            RunSql::portable()
                .for_backend(
                    "sqlite",
                    // SQLite: rebuild table without pdf_text/ocr_text, renaming final_text
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
                    r#"ALTER TABLE document_pages RENAME COLUMN final_text TO search_text;
ALTER TABLE document_pages DROP COLUMN pdf_text;
ALTER TABLE document_pages DROP COLUMN ocr_text"#,
                ),
        )
        // Step 4: Rebuild FTS index on search_text (Postgres only)
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    r#"DROP INDEX IF EXISTS idx_pages_fts;
CREATE INDEX idx_pages_fts ON document_pages
  USING GIN (to_tsvector('english', COALESCE(search_text, '')))"#,
                ),
        )
        // Step 5: Update partial index on pages with text (Postgres)
        .operation(
            RunSql::portable()
                .for_backend("sqlite", "SELECT 1")
                .for_backend(
                    "postgres",
                    r#"DROP INDEX IF EXISTS idx_pages_with_text;
CREATE INDEX idx_pages_with_text ON document_pages(document_id) WHERE search_text IS NOT NULL"#,
                ),
        )
}
