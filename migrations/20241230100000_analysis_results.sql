-- Migration: Unified analysis results table
-- Replaces page_ocr_results with a more flexible table that supports:
-- - Multiple analysis types (ocr, whisper, custom commands)
-- - Both page-level and document-level analysis

-- Create new unified analysis results table
CREATE TABLE IF NOT EXISTS document_analysis_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- NULL for document-level analysis (whisper, custom), set for page-level (OCR)
    page_id INTEGER,
    document_id TEXT NOT NULL,
    version_id INTEGER NOT NULL,
    -- Analysis type: "ocr", "whisper", "custom:name"
    analysis_type TEXT NOT NULL,
    -- Backend identifier: "tesseract", "deepseek", "whisper-base", etc.
    backend TEXT NOT NULL,
    -- Extracted text/transcription result
    result_text TEXT,
    -- Confidence score (0.0 - 1.0), if provided by backend
    confidence REAL,
    -- Processing time in milliseconds
    processing_time_ms INTEGER,
    -- Error message if analysis failed
    error TEXT,
    -- Status: "pending", "complete", "failed"
    status TEXT NOT NULL DEFAULT 'complete',
    -- When this result was created
    created_at TEXT NOT NULL,
    -- Additional metadata as JSON (model version, language detected, etc.)
    metadata TEXT,
    FOREIGN KEY (page_id) REFERENCES document_pages(id),
    FOREIGN KEY (document_id) REFERENCES documents(id),
    FOREIGN KEY (version_id) REFERENCES document_versions(id)
);

-- Indexes for efficient querying
CREATE INDEX IF NOT EXISTS idx_analysis_results_document ON document_analysis_results(document_id);
CREATE INDEX IF NOT EXISTS idx_analysis_results_page ON document_analysis_results(page_id);
CREATE INDEX IF NOT EXISTS idx_analysis_results_type ON document_analysis_results(analysis_type);
CREATE INDEX IF NOT EXISTS idx_analysis_results_status ON document_analysis_results(status);
CREATE INDEX IF NOT EXISTS idx_analysis_results_type_backend ON document_analysis_results(analysis_type, backend);

-- Unique constraint for page-level results
CREATE UNIQUE INDEX IF NOT EXISTS idx_analysis_results_page_unique
ON document_analysis_results(page_id, analysis_type, backend)
WHERE page_id IS NOT NULL;

-- Unique constraint for document-level results
CREATE UNIQUE INDEX IF NOT EXISTS idx_analysis_results_doc_unique
ON document_analysis_results(document_id, version_id, analysis_type, backend)
WHERE page_id IS NULL;

-- Migrate existing page_ocr_results data
INSERT INTO document_analysis_results (
    page_id, document_id, version_id, analysis_type, backend,
    result_text, confidence, processing_time_ms, status, created_at
)
SELECT
    por.page_id,
    dp.document_id,
    dp.version_id,
    'ocr' AS analysis_type,
    por.backend,
    por.ocr_text,
    por.confidence,
    por.processing_time_ms,
    CASE WHEN por.ocr_text IS NOT NULL THEN 'complete' ELSE 'failed' END AS status,
    por.created_at
FROM page_ocr_results por
JOIN document_pages dp ON dp.id = por.page_id;

-- Drop old table and its indexes
DROP INDEX IF EXISTS idx_page_ocr_results_page;
DROP INDEX IF EXISTS idx_page_ocr_results_backend;
DROP TABLE IF EXISTS page_ocr_results;
