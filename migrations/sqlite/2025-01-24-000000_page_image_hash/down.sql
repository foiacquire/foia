DROP INDEX IF EXISTS idx_page_ocr_results_hash_backend;
DROP INDEX IF EXISTS idx_page_ocr_results_image_hash;
-- SQLite doesn't support DROP COLUMN before 3.35.0, recreate table if needed
-- For simplicity, we leave the column (it will be unused)
