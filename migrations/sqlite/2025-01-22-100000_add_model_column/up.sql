-- Add model column to track which specific model produced results
-- e.g., "gemini-1.5-flash" vs "gemini-1.5-pro" or "llama-4-scout-17b"

-- Add model column to page_ocr_results
ALTER TABLE page_ocr_results ADD COLUMN model TEXT;

-- Add model column to document_analysis_results
ALTER TABLE document_analysis_results ADD COLUMN model TEXT;

-- Drop old unique constraint and create new one including model
DROP INDEX IF EXISTS idx_page_ocr_results_unique;

-- Recreate unique constraint with model (allows same backend with different models)
CREATE UNIQUE INDEX idx_page_ocr_results_unique
ON page_ocr_results(page_id, backend, COALESCE(model, ''));

-- For document_analysis_results, we need to recreate the unique indexes
DROP INDEX IF EXISTS idx_analysis_results_page_unique;
DROP INDEX IF EXISTS idx_analysis_results_doc_unique;

CREATE UNIQUE INDEX idx_analysis_results_page_unique
ON document_analysis_results(page_id, analysis_type, backend, COALESCE(model, ''))
WHERE page_id IS NOT NULL;

CREATE UNIQUE INDEX idx_analysis_results_doc_unique
ON document_analysis_results(document_id, version_id, analysis_type, backend, COALESCE(model, ''))
WHERE page_id IS NULL;

-- Add index for querying by model
CREATE INDEX idx_page_ocr_results_model ON page_ocr_results(model);
CREATE INDEX idx_analysis_results_model ON document_analysis_results(model);
