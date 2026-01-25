DROP INDEX IF EXISTS idx_page_ocr_results_hash_backend;
DROP INDEX IF EXISTS idx_page_ocr_results_image_hash;
ALTER TABLE page_ocr_results DROP COLUMN IF EXISTS image_hash;
