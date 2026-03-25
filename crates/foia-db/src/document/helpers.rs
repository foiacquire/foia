/// Navigation context for a document within a filtered list.
/// Uses window functions to efficiently find prev/next documents.
#[derive(Debug, Clone)]
pub struct DocumentNavigation {
    pub prev_id: Option<String>,
    pub prev_title: Option<String>,
    pub next_id: Option<String>,
    pub next_title: Option<String>,
    pub position: u64,
    pub total: u64,
}

pub use foia_models::{extract_filename_parts, sanitize_filename};
