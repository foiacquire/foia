//! Status command for showing system state.

use console::style;

use crate::config::Settings;
use crate::models::DocumentStatus;

/// Show overall system status.
pub async fn cmd_status(settings: &Settings) -> anyhow::Result<()> {
    if !settings.database_exists() {
        println!(
            "{} System not initialized. Run 'foiacquire init' first.",
            style("!").yellow()
        );
        return Ok(());
    }

    let ctx = settings.create_db_context()?;
    let doc_repo = ctx.documents();
    let source_repo = ctx.sources();

    println!("\n{}", style("FOIAcquire Status").bold());
    println!("{}", "-".repeat(40));
    println!("{:<20} {}", "Data Directory:", settings.data_dir.display());
    println!("{:<20} {}", "Sources:", source_repo.get_all().await?.len());
    println!("{:<20} {}", "Total Documents:", doc_repo.count().await?);

    // Count by status (single bulk query instead of N+1)
    let status_counts = doc_repo.count_all_by_status().await?;
    for status in [
        DocumentStatus::Pending,
        DocumentStatus::Downloaded,
        DocumentStatus::OcrComplete,
        DocumentStatus::Indexed,
        DocumentStatus::Failed,
    ] {
        if let Some(&count) = status_counts.get(status.as_str()) {
            if count > 0 {
                println!("{:<20} {}", format!("  {}:", status.as_str()), count);
            }
        }
    }

    Ok(())
}
