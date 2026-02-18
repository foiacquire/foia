//! Static file serving handlers.

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use super::super::assets;
use super::super::AppState;

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    pub filename: Option<String>,
}

/// Serve a document file.
///
/// When a `filename` query parameter is provided, the response includes a
/// `Content-Disposition` header so browsers use the original filename for
/// downloads instead of the content-addressable storage name.
pub async fn serve_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(params): Query<FileQuery>,
) -> Response {
    let canonical_docs_dir = match state.documents_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Server configuration error",
            )
                .into_response();
        }
    };

    if path.contains("..") || path.starts_with('/') {
        return (StatusCode::NOT_FOUND, "File not found").into_response();
    }

    let file_path = canonical_docs_dir.join(&path);

    let canonical_file = match file_path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::NOT_FOUND, "File not found").into_response();
        }
    };

    if !canonical_file.starts_with(&canonical_docs_dir) {
        return (StatusCode::NOT_FOUND, "File not found").into_response();
    }

    let content = match tokio::fs::read(&canonical_file).await {
        Ok(c) => c,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to read file").into_response();
        }
    };

    let mime = mime_guess::from_path(&canonical_file)
        .first_or_octet_stream()
        .to_string();

    let disposition = match params.filename {
        Some(name) => format!("inline; filename=\"{}\"", name.replace('"', "_")),
        None => "inline".to_string(),
    };

    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        content,
    )
        .into_response()
}

/// Serve CSS.
pub async fn serve_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], assets::CSS)
}

/// Serve JavaScript.
pub async fn serve_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript")],
        assets::JS,
    )
}
