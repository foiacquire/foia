//! Tests for the per-method analysis tracking queries.
//!
//! Verifies that `count_needing_analysis` and `get_needing_analysis` correctly
//! select documents based on their `document_analysis_results` state.

use foia::models::{Document, DocumentVersion};
use foia::repository::diesel_document::DieselDocumentRepository;
use foia::repository::migrations;
use foia::repository::pool::DbPool;

/// Create a temporary SQLite database with all migrations applied.
async fn setup_test_db() -> (DieselDocumentRepository, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let db_path = dir.path().join("test.db");
    let db_url = db_path.display().to_string();

    migrations::run_migrations(&db_url, false)
        .await
        .expect("Failed to run migrations");

    let pool = DbPool::sqlite_from_path(&db_path);
    let repo = DieselDocumentRepository::new(pool);
    (repo, dir)
}

/// Create a test document with a version and save it.
async fn create_test_doc(
    repo: &DieselDocumentRepository,
    id: &str,
    source_id: &str,
    mime_type: &str,
) {
    let version = DocumentVersion::new(
        b"test content",
        mime_type.to_string(),
        Some(format!("https://example.com/{id}")),
    );
    let doc = Document::new(
        id.to_string(),
        source_id.to_string(),
        format!("Test Document {id}"),
        format!("https://example.com/{id}"),
        version,
        serde_json::json!({}),
    );
    repo.save_with_versions(&doc)
        .await
        .expect("Failed to save document");
}

// ============================================================================
// count_needing_analysis
// ============================================================================

#[tokio::test]
async fn count_needing_analysis_finds_documents_without_results() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test-source", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test-source", "application/pdf").await;

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn count_needing_analysis_skips_completed_documents() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test-source", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test-source", "application/pdf").await;

    // Get the version ID for doc-001
    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Mark doc-001 as complete
    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn count_needing_analysis_skips_recent_failures() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test-source", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Mark doc-001 as failed (recent — within the 12h window)
    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        Some("OCR engine crashed"),
        None,
    )
    .await
    .unwrap();

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 0, "Recent failure should be skipped");
}

#[tokio::test]
async fn count_needing_analysis_retries_old_failures() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test-source", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Mark doc-001 as failed
    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        Some("OCR engine crashed"),
        None,
    )
    .await
    .unwrap();

    // Use a retry interval of 0 hours — all failures are eligible for retry
    let count = repo
        .count_needing_analysis("ocr", None, None, 0)
        .await
        .unwrap();
    assert_eq!(count, 1, "Old failure should be retried");
}

#[tokio::test]
async fn count_needing_analysis_filters_by_source() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "doj", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "cia", "application/pdf").await;
    create_test_doc(&repo, "doc-003", "doj", "application/pdf").await;

    let count = repo
        .count_needing_analysis("ocr", Some("doj"), None, 12)
        .await
        .unwrap();
    assert_eq!(count, 2);

    let count = repo
        .count_needing_analysis("ocr", Some("cia"), None, 12)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn count_needing_analysis_filters_by_mime_type() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test", "text/html").await;

    let count = repo
        .count_needing_analysis("ocr", None, Some("application/pdf"), 12)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn count_needing_analysis_skips_failed_status_documents() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    // Mark document as permanently failed
    repo.update_status("doc-001", foia::models::DocumentStatus::Failed)
        .await
        .unwrap();

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 0, "Failed documents should be skipped");
}

#[tokio::test]
async fn count_needing_analysis_includes_indexed_documents() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    // Mark document as indexed (previously these were skipped)
    repo.update_status("doc-001", foia::models::DocumentStatus::Indexed)
        .await
        .unwrap();

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "Indexed documents without analysis results should be eligible"
    );
}

#[tokio::test]
async fn count_needing_analysis_different_types_are_independent() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Complete OCR but not whisper
    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let ocr_count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(ocr_count, 0, "OCR is complete");

    let whisper_count = repo
        .count_needing_analysis("whisper", None, None, 12)
        .await
        .unwrap();
    assert_eq!(whisper_count, 1, "Whisper not done yet");
}

// ============================================================================
// get_needing_analysis
// ============================================================================

#[tokio::test]
async fn get_needing_analysis_returns_eligible_documents() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test", "application/pdf").await;

    let docs = repo
        .get_needing_analysis("ocr", 10, None, None, None, 12)
        .await
        .unwrap();
    assert_eq!(docs.len(), 2);
}

#[tokio::test]
async fn get_needing_analysis_respects_limit() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-003", "test", "application/pdf").await;

    let docs = repo
        .get_needing_analysis("ocr", 2, None, None, None, 12)
        .await
        .unwrap();
    assert_eq!(docs.len(), 2);
}

#[tokio::test]
async fn get_needing_analysis_cursor_pagination() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-003", "test", "application/pdf").await;

    // First page
    let page1 = repo
        .get_needing_analysis("ocr", 2, None, None, None, 12)
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);

    // Second page using cursor
    let last_id = &page1.last().unwrap().id;
    let page2 = repo
        .get_needing_analysis("ocr", 2, None, None, Some(last_id), 12)
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, "doc-003");
}

#[tokio::test]
async fn get_needing_analysis_excludes_completed() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;
    create_test_doc(&repo, "doc-002", "test", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let docs = repo
        .get_needing_analysis("ocr", 10, None, None, None, 12)
        .await
        .unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id, "doc-002");
}

// ============================================================================
// Worker lock (claim_analysis)
// ============================================================================

#[tokio::test]
async fn claim_analysis_creates_pending_row() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    repo.claim_analysis("doc-001", version_id, "ocr")
        .await
        .unwrap();

    // The pending count should include our claim
    let pending = repo.count_pending_analysis("ocr").await.unwrap();
    assert_eq!(pending, 1);
}

#[tokio::test]
async fn claim_analysis_locks_out_other_workers() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Claim the document
    repo.claim_analysis("doc-001", version_id, "ocr")
        .await
        .unwrap();

    // Another worker should not see this document
    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 0, "Claimed document should be locked out");

    let docs = repo
        .get_needing_analysis("ocr", 10, None, None, None, 12)
        .await
        .unwrap();
    assert!(
        docs.is_empty(),
        "Claimed document should not appear in results"
    );
}

#[tokio::test]
async fn completion_overwrites_pending_claim() {
    let (repo, _dir) = setup_test_db().await;
    create_test_doc(&repo, "doc-001", "test", "application/pdf").await;

    let doc = repo.get("doc-001").await.unwrap().unwrap();
    let version_id = doc.current_version().unwrap().id as i32;

    // Claim, then complete
    repo.claim_analysis("doc-001", version_id, "ocr")
        .await
        .unwrap();

    repo.store_analysis_result_for_document(
        "doc-001",
        version_id,
        "ocr",
        "tesseract",
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Should now be complete, not pending
    let pending = repo.count_pending_analysis("ocr").await.unwrap();
    assert_eq!(pending, 0, "Pending should be overwritten by completion");

    let count = repo
        .count_needing_analysis("ocr", None, None, 12)
        .await
        .unwrap();
    assert_eq!(count, 0, "Completed document should not need analysis");
}
