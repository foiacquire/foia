use cetane::prelude::*;

pub fn migration() -> Migration {
    Migration::new("0017_analysis_priority")
        .depends_on(&["0016_normalize_page_text"])
        .operation(AddField::new(
            "documents",
            Field::new("analysis_priority", FieldType::Integer)
                .not_null()
                .default("0"),
        ))
        .operation(AddIndex::new(
            "documents",
            Index::new("idx_documents_analysis_priority")
                .column("analysis_priority")
                .column("id"),
        ))
}
