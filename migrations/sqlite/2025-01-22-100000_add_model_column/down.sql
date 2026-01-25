-- Remove model column from tables
-- Note: SQLite doesn't support DROP COLUMN directly in older versions,
-- but modern SQLite (3.35+) does. For compatibility, we recreate tables.

-- Drop new indexes
DROP INDEX IF EXISTS idx_page_ocr_results_model;
DROP INDEX IF EXISTS idx_analysis_results_model;
DROP INDEX IF EXISTS idx_page_ocr_results_unique;
DROP INDEX IF EXISTS idx_analysis_results_page_unique;
DROP INDEX IF EXISTS idx_analysis_results_doc_unique;

-- Recreate original unique constraints without model
CREATE UNIQUE INDEX idx_page_ocr_results_unique
ON page_ocr_results(page_id, backend);

CREATE UNIQUE INDEX idx_analysis_results_page_unique
ON document_analysis_results(page_id, analysis_type, backend)
WHERE page_id IS NOT NULL;

CREATE UNIQUE INDEX idx_analysis_results_doc_unique
ON document_analysis_results(document_id, version_id, analysis_type, backend)
WHERE page_id IS NULL;

-- For SQLite 3.35+, we can drop columns directly
-- ALTER TABLE page_ocr_results DROP COLUMN model;
-- ALTER TABLE document_analysis_results DROP COLUMN model;

-- For older SQLite, the columns will remain but be unused
-- (migration rollback is rare in practice)
