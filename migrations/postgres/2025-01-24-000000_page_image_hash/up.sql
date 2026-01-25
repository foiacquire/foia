-- Add image hash columns to page_ocr_results for deduplication
ALTER TABLE page_ocr_results ADD COLUMN image_hash TEXT;

-- Index for efficient duplicate lookups
CREATE INDEX idx_page_ocr_results_image_hash ON page_ocr_results(image_hash) WHERE image_hash IS NOT NULL;

-- Combined index for backend-specific deduplication
CREATE INDEX idx_page_ocr_results_hash_backend ON page_ocr_results(image_hash, backend) WHERE image_hash IS NOT NULL;
