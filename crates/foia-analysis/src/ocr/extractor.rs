//! Text extraction from documents using pdftotext and Tesseract.

#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use thiserror::Error;

use super::model_utils::check_binary;

/// Handle command output, extracting stdout on success or returning appropriate error.
fn handle_cmd_output(
    result: std::io::Result<std::process::Output>,
    tool_name: &str,
    error_prefix: &str,
) -> Result<String, ExtractionError> {
    match result {
        Ok(output) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(ExtractionError::ExtractionFailed(format!(
                    "{}: {}",
                    error_prefix, stderr
                )))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ExtractionError::ToolNotFound(tool_name.to_string()))
        }
        Err(e) => Err(ExtractionError::Io(e)),
    }
}

/// Check command status, returning appropriate error on failure.
fn check_cmd_status(
    result: std::io::Result<std::process::ExitStatus>,
    tool_name: &str,
    error_msg: &str,
) -> Result<(), ExtractionError> {
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(ExtractionError::ExtractionFailed(error_msg.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ExtractionError::ToolNotFound(tool_name.to_string()))
        }
        Err(e) => Err(ExtractionError::Io(e)),
    }
}

/// Errors that can occur during text extraction.
#[derive(Debug, Error)]
pub enum ExtractionError {
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),

    #[error("External tool not found: {0}")]
    ToolNotFound(String),

    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of text extraction.
#[derive(Debug)]
pub struct ExtractionResult {
    /// Extracted text content.
    pub text: String,
    /// Method used for extraction.
    pub method: ExtractionMethod,
    /// Number of pages processed (for PDFs).
    pub page_count: Option<u32>,
}

/// Method used to extract text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMethod {
    /// Direct text extraction from PDF.
    PdfToText,
    /// OCR using Tesseract.
    TesseractOcr,
    /// Combined: pdftotext with OCR fallback for sparse pages.
    Hybrid,
}

/// Text extractor that uses external tools.
pub struct TextExtractor {
    /// Minimum characters per page to consider text extraction successful.
    min_chars_per_page: usize,
    /// Tesseract language setting.
    tesseract_lang: String,
}

impl Default for TextExtractor {
    fn default() -> Self {
        Self {
            min_chars_per_page: 100,
            tesseract_lang: "eng".to_string(),
        }
    }
}

impl TextExtractor {
    /// Create a new text extractor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set minimum characters per page threshold.
    pub fn with_min_chars(mut self, min_chars: usize) -> Self {
        self.min_chars_per_page = min_chars;
        self
    }

    /// Set Tesseract language.
    pub fn with_language(mut self, lang: &str) -> Self {
        self.tesseract_lang = lang.to_string();
        self
    }

    /// Extract text from a file based on its MIME type.
    pub fn extract(
        &self,
        file_path: &Path,
        mime_type: &str,
    ) -> Result<ExtractionResult, ExtractionError> {
        match mime_type {
            "application/pdf" => self.extract_pdf(file_path),
            "image/png" | "image/jpeg" | "image/tiff" | "image/gif" | "image/bmp" => {
                self.extract_image(file_path)
            }
            "text/plain" | "text/html" => {
                // Read directly
                let text = std::fs::read_to_string(file_path)?;
                Ok(ExtractionResult {
                    text,
                    method: ExtractionMethod::PdfToText, // Not really, but direct read
                    page_count: None,
                })
            }
            _ => Err(ExtractionError::UnsupportedFileType(mime_type.to_string())),
        }
    }

    /// Extract text from a PDF file using per-page analysis.
    /// Both pdftotext and OCR are run on each page, keeping whichever has more content.
    fn extract_pdf(&self, file_path: &Path) -> Result<ExtractionResult, ExtractionError> {
        let page_count = self.get_pdf_page_count(file_path).unwrap_or(1);

        // For single-page PDFs or if we can't get page count, use simple approach
        if page_count <= 1 {
            return self.extract_pdf_simple(file_path, page_count);
        }

        // Convert entire PDF to images for OCR
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();

        let pdftoppm_status = Command::new("pdftoppm")
            .args(["-png", "-r", "300"])
            .arg(file_path)
            .arg(temp_path.join("page"))
            .status();

        let ocr_available = match pdftoppm_status {
            Ok(s) if s.success() => true,
            _ => {
                tracing::debug!("pdftoppm failed, falling back to pdftotext only");
                false
            }
        };

        // Process each page
        let mut page_texts: Vec<String> = Vec::with_capacity(page_count as usize);
        let mut used_ocr = false;

        for page_num in 1..=page_count {
            // Get pdftotext result for this page
            let pdf_text = self
                .extract_pdf_page_text(file_path, page_num)
                .unwrap_or_default();
            let pdf_chars: usize = pdf_text.chars().filter(|c| !c.is_whitespace()).count();

            // Try OCR for this page
            let mut final_text = pdf_text.clone();

            if ocr_available {
                // Find the image file for this page (pdftoppm names them page-01.png, page-02.png, etc.)
                let image_path = self.find_page_image(temp_path, page_num);

                if let Some(img_path) = image_path {
                    if let Ok(ocr_text) = self.run_tesseract(&img_path) {
                        let ocr_chars: usize =
                            ocr_text.chars().filter(|c| !c.is_whitespace()).count();

                        // Use OCR if it has significantly more content (>20% more chars)
                        if ocr_chars > pdf_chars + (pdf_chars / 5) {
                            final_text = ocr_text;
                            used_ocr = true;
                        }
                    }
                }
            }

            page_texts.push(final_text);
        }

        let combined_text = page_texts.join("\n\n");
        let method = if used_ocr {
            ExtractionMethod::Hybrid
        } else {
            ExtractionMethod::PdfToText
        };

        Ok(ExtractionResult {
            text: combined_text,
            method,
            page_count: Some(page_count),
        })
    }

    /// Find the image file for a specific page number.
    fn find_page_image(&self, temp_path: &Path, page_num: u32) -> Option<std::path::PathBuf> {
        // pdftoppm names files like page-01.png, page-02.png, etc.
        // For documents with many pages, it may use more digits: page-001.png
        for digits in [2, 3, 4] {
            let filename = format!("page-{:0width$}.png", page_num, width = digits);
            let path = temp_path.join(&filename);
            if path.exists() {
                return Some(path);
            }
        }
        None
    }

    /// Simple PDF extraction for single-page PDFs or fallback.
    fn extract_pdf_simple(
        &self,
        file_path: &Path,
        page_count: u32,
    ) -> Result<ExtractionResult, ExtractionError> {
        let pdftotext_result = self.run_pdftotext(file_path)?;
        let pdf_chars: usize = pdftotext_result
            .chars()
            .filter(|c| !c.is_whitespace())
            .count();

        // Always try OCR and compare results
        match self.ocr_pdf(file_path) {
            Ok(ocr_text) => {
                let ocr_chars: usize = ocr_text.chars().filter(|c| !c.is_whitespace()).count();

                // Use OCR if it has significantly more content (>20% more chars)
                if ocr_chars > pdf_chars + (pdf_chars / 5) {
                    Ok(ExtractionResult {
                        text: ocr_text,
                        method: ExtractionMethod::TesseractOcr,
                        page_count: Some(page_count),
                    })
                } else {
                    Ok(ExtractionResult {
                        text: pdftotext_result,
                        method: ExtractionMethod::PdfToText,
                        page_count: Some(page_count),
                    })
                }
            }
            Err(e) => {
                tracing::debug!("OCR failed: {}, using pdftotext result", e);
                Ok(ExtractionResult {
                    text: pdftotext_result,
                    method: ExtractionMethod::PdfToText,
                    page_count: Some(page_count),
                })
            }
        }
    }

    /// Run pdftotext on a PDF file.
    fn run_pdftotext(&self, file_path: &Path) -> Result<String, ExtractionError> {
        let output = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8"])
            .arg(file_path)
            .arg("-") // Output to stdout
            .output();

        handle_cmd_output(
            output,
            "pdftotext (install poppler-utils)",
            "pdftotext failed",
        )
    }

    /// Extract text from all pages of a PDF in a single pdftotext call.
    /// Splits output on form-feed characters to get per-page text.
    /// Returns a Vec where index 0 = page 1, index 1 = page 2, etc.
    ///
    /// Falls back to per-page extraction if the PDF doesn't produce
    /// form-feed delimiters (malformed PDFs).
    pub fn extract_all_pdf_page_texts(
        &self,
        file_path: &Path,
        expected_pages: u32,
    ) -> Result<Vec<String>, ExtractionError> {
        let output = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8"])
            .arg(file_path)
            .arg("-")
            .output();

        let full_text = handle_cmd_output(
            output,
            "pdftotext (install poppler-utils)",
            "pdftotext failed",
        )?;

        let mut pages: Vec<String> = full_text.split('\x0C').map(|s| s.to_string()).collect();

        // pdftotext appends a trailing form-feed, producing an empty final element
        if pages.last().is_some_and(|s| s.trim().is_empty()) {
            pages.pop();
        }

        // Fallback: if split produced fewer pages than expected, extract per-page
        if pages.len() < expected_pages as usize && expected_pages > 1 {
            tracing::debug!(
                "Bulk pdftotext produced {} pages but expected {}, falling back to per-page",
                pages.len(),
                expected_pages
            );
            let mut fallback_pages = Vec::with_capacity(expected_pages as usize);
            for page_num in 1..=expected_pages {
                let text = self
                    .extract_pdf_page_text(file_path, page_num)
                    .unwrap_or_default();
                fallback_pages.push(text);
            }
            return Ok(fallback_pages);
        }

        Ok(pages)
    }

    /// Run pdftotext on a single page of a PDF file.
    pub fn extract_pdf_page_text(
        &self,
        file_path: &Path,
        page: u32,
    ) -> Result<String, ExtractionError> {
        let page_str = page.to_string();
        let output = Command::new("pdftotext")
            .args(["-layout", "-enc", "UTF-8", "-f", &page_str, "-l", &page_str])
            .arg(file_path)
            .arg("-") // Output to stdout
            .output();

        handle_cmd_output(
            output,
            "pdftotext (install poppler-utils)",
            &format!("pdftotext failed on page {}", page),
        )
    }

    /// Get the page count of a PDF.
    pub fn get_pdf_page_count(&self, file_path: &Path) -> Option<u32> {
        let output = Command::new("pdfinfo").arg(file_path).output().ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.starts_with("Pages:") {
                return line.split_whitespace().nth(1).and_then(|s| s.parse().ok());
            }
        }
        None
    }

    /// OCR a PDF by converting pages to images and running Tesseract.
    fn ocr_pdf(&self, file_path: &Path) -> Result<String, ExtractionError> {
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();

        // Convert PDF to images using pdftoppm
        let status = Command::new("pdftoppm")
            .args(["-png", "-r", "300"]) // 300 DPI
            .arg(file_path)
            .arg(temp_path.join("page"))
            .status();

        check_cmd_status(
            status,
            "pdftoppm (install poppler-utils)",
            "pdftoppm failed to convert PDF",
        )?;

        // Find all generated images
        let mut images: Vec<_> = std::fs::read_dir(temp_path)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "png")
                    .unwrap_or(false)
            })
            .map(|e| e.path())
            .collect();

        images.sort();

        if images.is_empty() {
            return Err(ExtractionError::ExtractionFailed(
                "No images generated from PDF".to_string(),
            ));
        }

        // OCR each image
        let mut all_text = String::new();
        for (i, image_path) in images.iter().enumerate() {
            match self.run_tesseract(image_path) {
                Ok(text) => {
                    if !all_text.is_empty() {
                        all_text.push_str("\n\n--- Page ");
                        all_text.push_str(&(i + 1).to_string());
                        all_text.push_str(" ---\n\n");
                    }
                    all_text.push_str(&text);
                }
                Err(e) => {
                    tracing::warn!("OCR failed for page {}: {}", i + 1, e);
                }
            }
        }

        Ok(all_text)
    }

    /// Extract text from an image file using Tesseract.
    fn extract_image(&self, file_path: &Path) -> Result<ExtractionResult, ExtractionError> {
        let text = self.run_tesseract(file_path)?;
        Ok(ExtractionResult {
            text,
            method: ExtractionMethod::TesseractOcr,
            page_count: Some(1),
        })
    }

    /// Run Tesseract OCR on an image.
    fn run_tesseract(&self, image_path: &Path) -> Result<String, ExtractionError> {
        let output = Command::new("tesseract")
            .arg(image_path)
            .arg("stdout")
            .args(["-l", &self.tesseract_lang])
            .output();

        handle_cmd_output(
            output,
            "tesseract (install tesseract-ocr)",
            "tesseract failed",
        )
    }

    /// OCR a single page of a PDF file.
    /// Converts the specified page to an image and runs Tesseract on it.
    pub fn ocr_pdf_page(&self, file_path: &Path, page: u32) -> Result<String, ExtractionError> {
        self.ocr_pdf_page_with_hash(file_path, page)
            .map(|(text, _hash)| text)
    }

    /// OCR a single page of a PDF file, returning both text and image hash.
    /// The image hash can be used for deduplication - pages with identical
    /// images will have the same hash.
    pub fn ocr_pdf_page_with_hash(
        &self,
        file_path: &Path,
        page: u32,
    ) -> Result<(String, String), ExtractionError> {
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();
        let output_prefix = temp_path.join("page");

        // Convert just this page to an image using pdftoppm
        let page_str = page.to_string();
        let status = Command::new("pdftoppm")
            .args(["-png", "-r", "300", "-f", &page_str, "-l", &page_str])
            .arg(file_path)
            .arg(&output_prefix)
            .status();

        check_cmd_status(
            status,
            "pdftoppm (install poppler-utils)",
            &format!("pdftoppm failed to convert page {}", page),
        )?;

        // Find the generated image
        if let Some(image_path) = self.find_page_image(temp_path, page) {
            // Compute hash before running OCR
            let image_hash = super::pdf_utils::compute_file_hash(&image_path)
                .map_err(|e| ExtractionError::ExtractionFailed(e.to_string()))?;
            let text = self.run_tesseract(&image_path)?;
            Ok((text, image_hash))
        } else {
            Err(ExtractionError::ExtractionFailed(format!(
                "No image generated for page {}",
                page
            )))
        }
    }

    /// Get the image hash for a PDF page without running OCR.
    /// Useful for checking deduplication before processing.
    pub fn get_pdf_page_hash(
        &self,
        file_path: &Path,
        page: u32,
    ) -> Result<String, ExtractionError> {
        let temp_dir = TempDir::new()?;
        let temp_path = temp_dir.path();
        let output_prefix = temp_path.join("page");

        // Convert just this page to an image using pdftoppm
        let page_str = page.to_string();
        let status = Command::new("pdftoppm")
            .args(["-png", "-r", "300", "-f", &page_str, "-l", &page_str])
            .arg(file_path)
            .arg(&output_prefix)
            .status();

        check_cmd_status(
            status,
            "pdftoppm (install poppler-utils)",
            &format!("pdftoppm failed to convert page {}", page),
        )?;

        // Find the generated image
        if let Some(image_path) = self.find_page_image(temp_path, page) {
            super::pdf_utils::compute_file_hash(&image_path)
                .map_err(|e| ExtractionError::ExtractionFailed(e.to_string()))
        } else {
            Err(ExtractionError::ExtractionFailed(format!(
                "No image generated for page {}",
                page
            )))
        }
    }

    /// OCR an image file directly.
    pub fn ocr_image(&self, file_path: &Path) -> Result<String, ExtractionError> {
        self.run_tesseract(file_path)
    }

    /// Check if required tools are available.
    pub fn check_tools() -> Vec<(String, bool)> {
        ["pdftotext", "pdftoppm", "pdfinfo", "tesseract"]
            .iter()
            .map(|tool| (tool.to_string(), check_binary(tool)))
            .collect()
    }

    /// Check only PDF processing tools (Poppler utilities).
    /// These are always required regardless of OCR backend.
    pub fn check_pdf_tools() -> Vec<(String, bool)> {
        ["pdftotext", "pdftoppm", "pdfinfo"]
            .iter()
            .map(|tool| (tool.to_string(), check_binary(tool)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_tools() {
        let tools = TextExtractor::check_tools();
        assert!(!tools.is_empty());

        let missing: Vec<&str> = tools
            .iter()
            .filter(|(_, available)| !available)
            .map(|(name, _)| name.as_str())
            .collect();

        if !missing.is_empty() {
            eprintln!(
                "Skipping tool-presence assertion: {} not found (CI or minimal env)",
                missing.join(", ")
            );
            return;
        }
    }

    /// Create a multi-page PDF with known text content for testing.
    /// Returns the path to the temporary PDF file.
    fn create_test_pdf(dir: &Path, pages: &[&str]) -> std::path::PathBuf {
        let tex_path = dir.join("test.tex");
        let mut tex = String::from("\\documentclass{article}\n\\begin{document}\n");
        for (i, text) in pages.iter().enumerate() {
            if i > 0 {
                tex.push_str("\\newpage\n");
            }
            tex.push_str(text);
            tex.push('\n');
        }
        tex.push_str("\\end{document}\n");
        std::fs::write(&tex_path, &tex).unwrap();

        let status = Command::new("pdflatex")
            .args(["-interaction=nonstopmode", "-output-directory"])
            .arg(dir)
            .arg(&tex_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => dir.join("test.pdf"),
            _ => panic!("pdflatex not available or failed"),
        }
    }

    #[test]
    fn test_extract_all_pdf_page_texts_splits_correctly() {
        if !check_binary("pdflatex") || !check_binary("pdftotext") {
            eprintln!("Skipping: pdflatex or pdftotext not available");
            return;
        }

        let dir = tempfile::TempDir::new().unwrap();
        let page_contents = ["Page one content here", "Page two content here", "Page three final"];
        let pdf_path = create_test_pdf(dir.path(), &page_contents);

        let extractor = TextExtractor::new();
        let pages = extractor
            .extract_all_pdf_page_texts(&pdf_path, 3)
            .expect("extraction should succeed");

        assert_eq!(pages.len(), 3, "should produce exactly 3 pages");

        for (i, expected) in page_contents.iter().enumerate() {
            assert!(
                pages[i].contains(expected),
                "page {} should contain {:?}, got {:?}",
                i + 1,
                expected,
                &pages[i][..pages[i].len().min(100)]
            );
        }
    }

    #[test]
    fn test_extract_all_matches_per_page() {
        if !check_binary("pdflatex") || !check_binary("pdftotext") {
            eprintln!("Skipping: pdflatex or pdftotext not available");
            return;
        }

        let dir = tempfile::TempDir::new().unwrap();
        let page_contents = ["Alpha text", "Beta text"];
        let pdf_path = create_test_pdf(dir.path(), &page_contents);

        let extractor = TextExtractor::new();

        // Bulk extraction
        let bulk_pages = extractor
            .extract_all_pdf_page_texts(&pdf_path, 2)
            .expect("bulk extraction should succeed");

        // Per-page extraction
        let page1 = extractor.extract_pdf_page_text(&pdf_path, 1).unwrap();
        let page2 = extractor.extract_pdf_page_text(&pdf_path, 2).unwrap();

        assert_eq!(bulk_pages.len(), 2);
        assert_eq!(bulk_pages[0].trim(), page1.trim(), "page 1 should match");
        assert_eq!(bulk_pages[1].trim(), page2.trim(), "page 2 should match");
    }

    #[test]
    fn test_extract_all_single_page() {
        if !check_binary("pdflatex") || !check_binary("pdftotext") {
            eprintln!("Skipping: pdflatex or pdftotext not available");
            return;
        }

        let dir = tempfile::TempDir::new().unwrap();
        let pdf_path = create_test_pdf(dir.path(), &["Only one page"]);

        let extractor = TextExtractor::new();
        let pages = extractor
            .extract_all_pdf_page_texts(&pdf_path, 1)
            .expect("extraction should succeed");

        assert_eq!(pages.len(), 1);
        assert!(pages[0].contains("Only one page"));
    }
}
