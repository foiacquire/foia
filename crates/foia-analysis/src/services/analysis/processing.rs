//! OCR processing helper functions.

use std::fs::File;
use std::io::Read;

use tempfile::TempDir;

use crate::ocr::{BackendConfig, FallbackOcrBackend, OcrBackend, TextExtractor};
use foia::config::OcrConfig;
use foia::models::{Document, DocumentPage, PageOcrStatus};
use foia::repository::DieselDocumentRepository;

use super::types::PageOcrResult;

/// Detect MIME type from file content and check if it differs from the stored type.
///
/// Returns `Some((detected_mime, old_mime))` if they differ meaningfully, `None` otherwise.
/// Reads the first 8KB of the file for magic-byte detection.
pub fn detect_mime_mismatch(
    path: &std::path::Path,
    stored_mime: &str,
) -> Option<(String, String)> {
    let mut file = File::open(path).ok()?;
    let mut buffer = [0u8; 8192];
    let bytes_read = file.read(&mut buffer).ok()?;

    if bytes_read == 0 {
        return None;
    }

    let detected = infer::get(&buffer[..bytes_read])?;
    let detected_mime = detected.mime_type();

    let stored_normalized = stored_mime
        .split(';')
        .next()
        .unwrap_or(stored_mime)
        .trim()
        .to_lowercase();

    if detected_mime != stored_normalized {
        if stored_normalized == "application/octet-stream"
            || stored_normalized == "binary/octet-stream"
        {
            return Some((detected_mime.to_string(), stored_normalized));
        }

        let stored_base = stored_normalized.split('/').next().unwrap_or("");
        let detected_base = detected_mime.split('/').next().unwrap_or("");

        if stored_base != detected_base {
            return Some((detected_mime.to_string(), stored_normalized));
        }
    }

    None
}

/// Extract text from a document per-page using pdftotext.
/// This function runs in a blocking context and uses the runtime handle to call async methods.
pub fn extract_document_text_per_page(
    doc: &Document,
    doc_repo: &DieselDocumentRepository,
    handle: &tokio::runtime::Handle,
    documents_dir: &std::path::Path,
) -> anyhow::Result<usize> {
    let extractor = TextExtractor::new();

    let version = doc
        .current_version()
        .ok_or_else(|| anyhow::anyhow!("Document has no versions"))?;

    let file_path = version.resolve_path(documents_dir, &doc.source_url, &doc.title);

    // Only process PDFs with per-page extraction
    if version.mime_type != "application/pdf" {
        // For non-PDFs, use the old extraction method
        let result = extractor.extract(&file_path, &version.mime_type)?;

        // Create a single "page" for non-PDF documents
        let mut page = DocumentPage::new(doc.id.clone(), version.id, 1);
        page.search_text = Some(result.text.clone());
        page.ocr_status = PageOcrStatus::OcrComplete;
        let page_id = handle.block_on(doc_repo.save_page(&page))?;

        // Store extraction result in normalized table
        let _ = handle.block_on(doc_repo.store_page_ocr_result(
            page_id,
            "pdftotext",
            None,
            Some(&result.text),
            None,
            None,
            None,
        ));

        // Cache page count (1 for non-PDFs)
        handle.block_on(doc_repo.set_version_page_count(version.id, 1))?;

        // Non-PDFs are complete immediately - finalize the document
        handle.block_on(doc_repo.finalize_document(&doc.id))?;

        // Record completion so this document won't be picked up again
        let _ = handle.block_on(doc_repo.store_analysis_result_for_document(
            &doc.id,
            version.id as i32,
            "ocr",
            "text_extraction",
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        return Ok(1);
    }

    // Get page count (use cached value if available)
    let page_count = version.page_count.unwrap_or_else(|| {
        tracing::debug!(
            "Getting page count for document {}: {}",
            doc.id,
            file_path.display()
        );
        let count = extractor.get_pdf_page_count(&file_path).unwrap_or(1);
        tracing::debug!("Document {} has {} pages", doc.id, count);
        count
    });

    // Cache page count if not already cached
    if version.page_count.is_none() {
        handle.block_on(doc_repo.set_version_page_count(version.id, page_count))?;
    }

    // Skip if pages already exist for this version (text extraction already done)
    let existing_pages = handle.block_on(doc_repo.count_pages(&doc.id, version.id as i32))?;
    if existing_pages > 0 {
        tracing::debug!(
            "Document {} already has {} pages, skipping text extraction",
            doc.id,
            existing_pages
        );
        return Ok(0);
    }

    // Extract all pages in a single pdftotext call, split on form-feed
    let page_texts = match extractor.extract_all_pdf_page_texts(&file_path, page_count) {
        Ok(texts) => texts,
        Err(e) => {
            tracing::debug!("pdftotext failed for {}: {}, creating empty pages", doc.id, e);
            vec![String::new(); page_count as usize]
        }
    };

    // Build all page records in memory
    let mut pages = Vec::with_capacity(page_texts.len());
    for (i, pdf_text) in page_texts.iter().enumerate() {
        let page_num = (i + 1) as u32;
        let mut page = DocumentPage::new(doc.id.clone(), version.id, page_num);
        page.search_text = Some(pdf_text.clone());
        page.ocr_status = PageOcrStatus::TextExtracted;
        pages.push(page);
    }

    // Bulk insert all pages at once
    if !pages.is_empty() {
        tracing::debug!(
            "Saving {} pages to database for document {}",
            pages.len(),
            doc.id
        );
        handle.block_on(doc_repo.save_pages_batch(&pages))?;

        // Store pdftotext results in normalized page_ocr_results table
        handle.block_on(doc_repo.store_pdftotext_results_batch(
            &doc.id,
            version.id as i32,
        ))?;
    }

    Ok(pages.len())
}

/// Run OCR on a page and compare with existing text.
/// If all pages for this document are now complete, the document is finalized
/// (status set to OcrComplete, combined text saved).
/// This function runs in a blocking context and uses the runtime handle to call async methods.
///
/// Uses the default tesseract backend. For configurable fallback chains, use
/// `ocr_document_page_with_config`.
#[allow(dead_code)]
pub fn ocr_document_page(
    page: &DocumentPage,
    doc_repo: &DieselDocumentRepository,
    handle: &tokio::runtime::Handle,
    documents_dir: &std::path::Path,
) -> anyhow::Result<PageOcrResult> {
    ocr_document_page_with_config(page, doc_repo, handle, &OcrConfig::default(), documents_dir)
}

/// Run OCR on a page using configured backend entries.
///
/// Each backend entry produces a separate result:
/// - Single backend: runs and stores result
/// - Fallback chain: tries backends in order until one succeeds, stores result
///
/// Example config: `["tesseract", ["groq", "gemini"]]`
/// - Runs tesseract, stores as "tesseract"
/// - Runs groq (falls back to gemini if rate limited), stores as "groq" or "gemini"
#[allow(dead_code)]
pub fn ocr_document_page_with_config(
    page: &DocumentPage,
    doc_repo: &DieselDocumentRepository,
    handle: &tokio::runtime::Handle,
    ocr_config: &OcrConfig,
    documents_dir: &std::path::Path,
) -> anyhow::Result<PageOcrResult> {
    let backends: Vec<FallbackOcrBackend> = ocr_config
        .backends
        .iter()
        .map(|entry| {
            let names: Vec<&str> = entry.backends();
            FallbackOcrBackend::from_names(&names, BackendConfig::default())
        })
        .collect();

    ocr_document_page_with_backends(page, doc_repo, handle, ocr_config, &backends, documents_dir)
}

/// Run OCR on a page using pre-built backends.
///
/// This is the primary entry point for batch OCR. Backends are constructed once
/// and reused across all pages, avoiding per-page HTTP client creation and
/// redundant backend initialization.
///
/// The page image is rendered once via pdftoppm and reused for both hash
/// computation (deduplication) and OCR, eliminating the double-render overhead.
pub fn ocr_document_page_with_backends(
    page: &DocumentPage,
    doc_repo: &DieselDocumentRepository,
    handle: &tokio::runtime::Handle,
    ocr_config: &OcrConfig,
    backends: &[FallbackOcrBackend],
    documents_dir: &std::path::Path,
) -> anyhow::Result<PageOcrResult> {
    // Get the document to find the file path
    let doc = handle
        .block_on(doc_repo.get(&page.document_id))?
        .ok_or_else(|| anyhow::anyhow!("Document not found"))?;

    let version = doc
        .versions
        .iter()
        .find(|v| v.id == page.version_id)
        .ok_or_else(|| anyhow::anyhow!("Version not found"))?;

    let file_path = version.resolve_path(documents_dir, &doc.source_url, &doc.title);

    // Render page image once — used for both hashing and OCR
    let temp_dir = TempDir::new()?;
    let image_result =
        crate::ocr::pdf_utils::pdf_page_to_image(&file_path, page.page_number, temp_dir.path());

    let (image_path, image_hash) = match image_result {
        Ok(path) => {
            let hash = crate::ocr::pdf_utils::compute_file_hash(&path).ok();
            (Some(path), hash)
        }
        Err(e) => {
            tracing::debug!(
                "Failed to render page {} image: {}",
                page.page_number,
                e
            );
            (None, None)
        }
    };

    let mut updated_page = page.clone();
    let mut improved = false;
    let mut any_succeeded = false;
    let mut best_char_count = 0usize;

    let existing_chars = page
        .search_text
        .as_ref()
        .map(|t| t.chars().filter(|c| !c.is_whitespace()).count())
        .unwrap_or(0);

    // Process each backend entry (paired with pre-built backends)
    for (entry, fallback) in ocr_config.backends.iter().zip(backends.iter()) {
        let backend_names: Vec<&str> = entry.backends();

        // Check for existing result from any backend in this entry
        let existing = if let Some(ref hash) = image_hash {
            backend_names.iter().find_map(|name| {
                handle
                    .block_on(doc_repo.find_ocr_result_by_image_hash(hash, name))
                    .ok()
                    .flatten()
                    .map(|r| (r, name.to_string()))
            })
        } else {
            None
        };

        if let Some((existing_result, backend_name)) = existing {
            // Reuse existing result
            let ocr_text = existing_result.text.clone().unwrap_or_default();
            let ocr_chars = ocr_text.chars().filter(|c| !c.is_whitespace()).count();

            // Store reference for this page
            handle.block_on(doc_repo.store_page_ocr_result(
                page.id,
                &backend_name,
                existing_result.model.as_deref(),
                Some(&ocr_text),
                existing_result.confidence,
                existing_result.processing_time_ms,
                image_hash.as_deref(),
            ))?;

            tracing::debug!(
                "Reused existing {} result for page {} (hash match)",
                backend_name,
                page.page_number
            );

            any_succeeded = true;
            if ocr_chars > best_char_count {
                best_char_count = ocr_chars;
            }
        } else {
            // Run OCR using pre-rendered image (or fall back to pdf_page for robustness)
            let ocr_result = if let Some(ref img) = image_path {
                fallback.ocr_image(img)
            } else {
                fallback.ocr_pdf_page(&file_path, page.page_number)
            };

            match ocr_result {
                Ok(result) => {
                    let ocr_text = result.text;
                    let backend_name = result.backend.as_str();
                    let ocr_chars = ocr_text.chars().filter(|c| !c.is_whitespace()).count();

                    // Store result
                    handle.block_on(doc_repo.store_page_ocr_result(
                        page.id,
                        backend_name,
                        result.model.as_deref(),
                        Some(&ocr_text),
                        result.confidence,
                        Some(result.processing_time_ms as i32),
                        image_hash.as_deref(),
                    ))?;

                    tracing::debug!(
                        "OCR completed for page {} using {} backend ({} chars)",
                        page.page_number,
                        backend_name,
                        ocr_chars
                    );

                    any_succeeded = true;
                    if ocr_chars > best_char_count {
                        best_char_count = ocr_chars;
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "OCR entry {:?} failed for page {}: {}",
                        entry,
                        page.page_number,
                        e
                    );
                }
            }
        }
    }

    // Update page status — search_text is recomputed by update_search_text()
    if any_succeeded {
        updated_page.ocr_status = PageOcrStatus::OcrComplete;
        improved = best_char_count > existing_chars + (existing_chars / 5);
    } else {
        updated_page.ocr_status = PageOcrStatus::Failed;
    }

    handle.block_on(doc_repo.save_page(&updated_page))?;

    // Recompute search_text from all page_ocr_results (picks highest char_count).
    // store_page_ocr_result already does this per-insert, but save_page may have
    // overwritten the value, so we recompute once here to ensure correctness.
    handle.block_on(doc_repo.update_search_text(page.id))?;

    // Check if all pages for this document are now complete
    let mut document_finalized = false;
    if handle
        .block_on(doc_repo.are_all_pages_complete(&page.document_id, page.version_id as i32))?
    {
        handle.block_on(doc_repo.finalize_document(&page.document_id))?;

        // Record completion in document_analysis_results so this document
        // won't be picked up for OCR analysis again
        let _ = handle.block_on(doc_repo.store_analysis_result_for_document(
            &page.document_id,
            page.version_id as i32,
            "ocr",
            "pipeline",
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        document_finalized = true;
        tracing::debug!(
            "Document {} finalized after page {} completed",
            page.document_id,
            page.page_number
        );
    }

    Ok(PageOcrResult {
        improved,
        document_finalized,
    })
}
