use cetane::prelude::*;

pub fn migration() -> Migration {
    Migration::new("0015_analysis_covering_index")
        .depends_on(&["0013_analysis_lookup_index"])
        // Drop the old 4-column index, replaced by the 5-column covering index
        .operation(
            RunSql::new("DROP INDEX IF EXISTS idx_dar_doc_version_type_status"),
        )
        // Covering index: includes created_at so the NOT EXISTS subqueries
        // with timestamp filters (failed retry window, pending lock window)
        // resolve entirely from the index without heap lookups.
        .operation(AddIndex::new(
            "document_analysis_results",
            Index::new("idx_dar_analysis_lookup")
                .column("document_id")
                .column("version_id")
                .column("analysis_type")
                .column("status")
                .column("created_at"),
        ))
}
