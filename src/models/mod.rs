//! Data models for FOIAcquire.

mod crawl;
mod document;
mod document_page;
mod service_status;
mod source;
mod virtual_file;

pub use crawl::{CrawlRequest, CrawlUrl, DiscoveryMethod, UrlStatus};
pub use document::{Document, DocumentDisplay, DocumentStatus, DocumentVersion};
pub use document_page::{DocumentPage, PageOcrStatus};
pub use service_status::{ScraperStats, ServiceState, ServiceStatus, ServiceType};
pub use source::{Source, SourceType};
pub use virtual_file::{VirtualFile, VirtualFileStatus};
