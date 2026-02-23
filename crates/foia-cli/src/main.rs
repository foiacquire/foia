//! foia - FOIA document acquisition and research system.
//!
//! A tool for acquiring, storing, and researching Freedom of Information Act
//! documents from various government sources.

mod cli;

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present (before anything else)
    let _ = dotenvy::dotenv();

    // Initialize logging based on verbosity.
    // Always suppress noisy tokio_postgres NOTICE messages (promoted to INFO),
    // even when RUST_LOG is set explicitly.
    let base_filter = if cli::is_verbose() {
        "foia=info"
    } else {
        "foia=warn"
    };

    let filter_str = match std::env::var("RUST_LOG") {
        Ok(user_filter) => format!("{user_filter},tokio_postgres=warn"),
        Err(_) => format!("{base_filter},tokio_postgres=warn"),
    };

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter_str))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Run CLI
    cli::run().await
}
