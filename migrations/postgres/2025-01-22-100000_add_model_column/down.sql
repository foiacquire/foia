-- Remove model column from tables

-- Drop new indexes
DROP INDEX IF EXISTS idx_page_ocr_results_model;
DROP INDEX IF EXISTS idx_analysis_results_model;
DROP INDEX IF EXISTS idx_page_ocr_results_unique;
DROP INDEX IF EXISTS idx_analysis_results_page_unique;
DROP INDEX IF EXISTS idx_analysis_results_doc_unique;

-- Recreate original unique constraint without model
ALTER TABLE page_ocr_results ADD CONSTRAINT page_ocr_results_page_id_backend_key UNIQUE (page_id, backend);

CREATE UNIQUE INDEX idx_analysis_results_page_unique
ON document_analysis_results(page_id, analysis_type, backend)
WHERE page_id IS NOT NULL;

CREATE UNIQUE INDEX idx_analysis_results_doc_unique
ON document_analysis_results(document_id, version_id, analysis_type, backend)
WHERE page_id IS NULL;

-- Drop model columns
ALTER TABLE page_ocr_results DROP COLUMN model;
ALTER TABLE document_analysis_results DROP COLUMN model;
