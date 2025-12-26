//! Source management commands.

use console::style;

use crate::config::Settings;
use crate::repository::{
    create_pool, AsyncCrawlRepository, AsyncDocumentRepository, AsyncSourceRepository,
};

use super::helpers::truncate;

/// List configured sources.
pub async fn cmd_source_list(settings: &Settings) -> anyhow::Result<()> {
    let pool = create_pool(&settings.database_path()).await?;
    let source_repo = AsyncSourceRepository::new(pool);
    let sources = source_repo.get_all().await?;

    if sources.is_empty() {
        println!(
            "{} No sources configured. Run 'foiacquire init' first.",
            style("!").yellow()
        );
        return Ok(());
    }

    println!("\n{}", style("FOIA Sources").bold());
    println!("{}", "-".repeat(60));
    println!("{:<15} {:<25} {:<10} Last Scraped", "ID", "Name", "Type");
    println!("{}", "-".repeat(60));

    for source in sources {
        let last_scraped = source
            .last_scraped
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "Never".to_string());

        println!(
            "{:<15} {:<25} {:<10} {}",
            source.id,
            truncate(&source.name, 24),
            source.source_type.as_str(),
            last_scraped
        );
    }

    Ok(())
}

/// Rename a source (updates all associated documents).
pub async fn cmd_source_rename(
    settings: &Settings,
    old_id: &str,
    new_id: &str,
    confirm: bool,
) -> anyhow::Result<()> {
    use std::io::{self, Write};

    let db_path = settings.database_path();
    let pool = create_pool(&db_path).await?;
    let source_repo = AsyncSourceRepository::new(pool.clone());
    let doc_repo = AsyncDocumentRepository::new(pool.clone(), settings.documents_dir.clone());
    let crawl_repo = AsyncCrawlRepository::new(pool.clone());

    // Check old source exists
    let old_source = source_repo.get(old_id).await?;
    if old_source.is_none() {
        println!("{} Source '{}' not found", style("✗").red(), old_id);
        return Ok(());
    }

    // Check new source doesn't exist
    if source_repo.get(new_id).await?.is_some() {
        println!(
            "{} Source '{}' already exists. Use a different name or delete it first.",
            style("✗").red(),
            new_id
        );
        return Ok(());
    }

    // Count affected documents
    let doc_count = doc_repo.count_by_source(old_id).await?;
    let crawl_count = crawl_repo.count_by_source(old_id).await?;

    println!(
        "\n{} Rename source '{}' → '{}'",
        style("→").cyan(),
        style(old_id).yellow(),
        style(new_id).green()
    );
    println!("  Documents to update: {}", doc_count);
    println!("  Crawl URLs to update: {}", crawl_count);

    // Confirm
    if !confirm {
        print!("\nProceed? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{} Cancelled", style("!").yellow());
            return Ok(());
        }
    }

    // Perform the rename using sqlx transaction for atomicity
    let mut tx = pool.begin().await?;

    // Update documents
    let docs_result = sqlx::query!(
        "UPDATE documents SET source_id = ?1 WHERE source_id = ?2",
        new_id,
        old_id
    )
    .execute(&mut *tx)
    .await?;
    let docs_updated = docs_result.rows_affected();

    // Update crawl_urls
    let crawls_result = sqlx::query!(
        "UPDATE crawl_urls SET source_id = ?1 WHERE source_id = ?2",
        new_id,
        old_id
    )
    .execute(&mut *tx)
    .await?;
    let crawls_updated = crawls_result.rows_affected();

    // Update crawl_config
    sqlx::query!(
        "UPDATE crawl_config SET source_id = ?1 WHERE source_id = ?2",
        new_id,
        old_id
    )
    .execute(&mut *tx)
    .await?;

    // Update source itself
    sqlx::query!("UPDATE sources SET id = ?1 WHERE id = ?2", new_id, old_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    println!(
        "\n{} Renamed '{}' → '{}'",
        style("✓").green(),
        old_id,
        new_id
    );
    println!("  Documents updated: {}", docs_updated);
    println!("  Crawl URLs updated: {}", crawls_updated);

    Ok(())
}
