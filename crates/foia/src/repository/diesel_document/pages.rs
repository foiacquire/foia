//! Document page and OCR operations.

use std::collections::HashMap;

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::{DieselDocumentRepository, OcrResult, ReturningId};
use crate::models::{DocumentPage, PageOcrStatus};
use crate::repository::models::{DocumentPageRecord, PageOcrResultRecord};
use crate::repository::parse_datetime;
use crate::repository::pool::DieselError;
use crate::schema::{document_pages, page_ocr_results};
use crate::{with_conn, with_conn_split};

#[derive(diesel::QueryableByName, Debug)]
pub struct PageSearchRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub document_id: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub title: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub source_id: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub page_number: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub headline: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub content_hash: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub version_mime_type: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    pub original_filename: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    pub dedup_index: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub source_url: String,
}

impl From<DocumentPageRecord> for DocumentPage {
    fn from(r: DocumentPageRecord) -> Self {
        Self {
            id: r.id as i64,
            document_id: r.document_id,
            version_id: r.version_id as i64,
            page_number: r.page_number as u32,
            search_text: r.search_text,
            ocr_status: PageOcrStatus::from_str(&r.ocr_status).unwrap_or(PageOcrStatus::Pending),
            created_at: parse_datetime(&r.created_at),
            updated_at: parse_datetime(&r.updated_at),
        }
    }
}

impl DieselDocumentRepository {
    /// Count pages for a document.
    pub async fn count_pages(&self, document_id: &str, version: i32) -> Result<u32, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let count: i64 = document_pages::table
                .filter(document_pages::document_id.eq(document_id))
                .filter(document_pages::version_id.eq(version))
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(count as u32)
        })
    }

    /// Save a document page. Returns the page ID.
    pub async fn save_page(&self, page: &DocumentPage) -> Result<i64, DieselError> {
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::DocumentPages;
        use sea_query::{OnConflict, Query};

        let now = Utc::now().to_rfc3339();
        let version_id = page.version_id as i32;
        let page_number = page.page_number as i32;
        let ocr_status = page.ocr_status.as_str().to_string();

        let stmt = Query::insert()
            .into_table(DocumentPages::Table)
            .columns([
                DocumentPages::DocumentId,
                DocumentPages::VersionId,
                DocumentPages::PageNumber,
                DocumentPages::SearchText,
                DocumentPages::OcrStatus,
                DocumentPages::CreatedAt,
                DocumentPages::UpdatedAt,
            ])
            .values_panic([
                page.document_id.clone().into(),
                version_id.into(),
                page_number.into(),
                page.search_text.clone().into(),
                ocr_status.clone().into(),
                now.clone().into(),
                now.clone().into(),
            ])
            .on_conflict(
                OnConflict::columns([
                    DocumentPages::DocumentId,
                    DocumentPages::VersionId,
                    DocumentPages::PageNumber,
                ])
                .update_columns([
                    DocumentPages::SearchText,
                    DocumentPages::OcrStatus,
                    DocumentPages::UpdatedAt,
                ])
                .to_owned(),
            )
            .returning_col(DocumentPages::Id)
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            let result: ReturningId = diesel::sql_query(&sql)
                .bind::<diesel::sql_types::Text, _>(&page.document_id)
                .bind::<diesel::sql_types::Integer, _>(version_id)
                .bind::<diesel::sql_types::Integer, _>(page_number)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(&page.search_text)
                .bind::<diesel::sql_types::Text, _>(&ocr_status)
                .bind::<diesel::sql_types::Text, _>(&now)
                .bind::<diesel::sql_types::Text, _>(&now)
                .get_result(&mut conn)
                .await?;
            Ok(result.id as i64)
        })
    }

    /// Save multiple document pages in a single bulk insert.
    /// Much faster than calling save_page() in a loop.
    pub async fn save_pages_batch(&self, pages: &[DocumentPage]) -> Result<(), DieselError> {
        if pages.is_empty() {
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();

        with_conn_split!(self.pool,
            sqlite: conn => {
                for page in pages {
                    let version_id = page.version_id as i32;
                    let page_number = page.page_number as i32;
                    let ocr_status = page.ocr_status.as_str().to_string();

                    diesel::insert_into(document_pages::table)
                        .values((
                            document_pages::document_id.eq(&page.document_id),
                            document_pages::version_id.eq(version_id),
                            document_pages::page_number.eq(page_number),
                            document_pages::search_text.eq(&page.search_text),
                            document_pages::ocr_status.eq(&ocr_status),
                            document_pages::created_at.eq(&now),
                            document_pages::updated_at.eq(&now),
                        ))
                        .on_conflict((
                            document_pages::document_id,
                            document_pages::version_id,
                            document_pages::page_number,
                        ))
                        .do_update()
                        .set((
                            document_pages::search_text.eq(&page.search_text),
                            document_pages::ocr_status.eq(&ocr_status),
                            document_pages::updated_at.eq(&now),
                        ))
                        .execute(&mut conn)
                        .await?;
                }
                Ok::<_, DieselError>(())
            },
            postgres: conn => {
                use crate::repository::sea_tables::DocumentPages;
                use sea_query::{OnConflict, Query, PostgresQueryBuilder};

                for chunk in pages.chunks(50) {
                    let mut insert = Query::insert()
                        .into_table(DocumentPages::Table)
                        .columns([
                            DocumentPages::DocumentId,
                            DocumentPages::VersionId,
                            DocumentPages::PageNumber,
                            DocumentPages::SearchText,
                            DocumentPages::OcrStatus,
                            DocumentPages::CreatedAt,
                            DocumentPages::UpdatedAt,
                        ])
                        .to_owned();

                    for page in chunk {
                        insert = insert.values_panic([
                            page.document_id.clone().into(),
                            (page.version_id as i32).into(),
                            (page.page_number as i32).into(),
                            page.search_text.clone().into(),
                            page.ocr_status.as_str().to_string().into(),
                            now.clone().into(),
                            now.clone().into(),
                        ]).to_owned();
                    }

                    let stmt = insert.on_conflict(
                        OnConflict::columns([
                            DocumentPages::DocumentId,
                            DocumentPages::VersionId,
                            DocumentPages::PageNumber,
                        ])
                        .update_columns([
                            DocumentPages::SearchText,
                            DocumentPages::OcrStatus,
                            DocumentPages::UpdatedAt,
                        ])
                        .to_owned(),
                    ).to_owned();

                    let (sql, _) = stmt.build(PostgresQueryBuilder);

                    let mut query = diesel::sql_query(sql).into_boxed::<diesel::pg::Pg>();
                    for page in chunk {
                        let version_id = page.version_id as i32;
                        let page_number = page.page_number as i32;
                        let ocr_status = page.ocr_status.as_str().to_string();

                        query = query
                            .bind::<diesel::sql_types::Text, _>(page.document_id.clone())
                            .bind::<diesel::sql_types::Integer, _>(version_id)
                            .bind::<diesel::sql_types::Integer, _>(page_number)
                            .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(page.search_text.clone())
                            .bind::<diesel::sql_types::Text, _>(ocr_status)
                            .bind::<diesel::sql_types::Text, _>(now.clone())
                            .bind::<diesel::sql_types::Text, _>(now.clone());
                    }

                    query.execute(&mut conn).await?;
                }
                Ok::<_, DieselError>(())
            }
        )?;

        Ok(())
    }

    /// Get document pages.
    pub async fn get_pages(
        &self,
        document_id: &str,
        version: i32,
    ) -> Result<Vec<DocumentPage>, DieselError> {
        let records: Vec<DocumentPageRecord> = with_conn!(self.pool, conn, {
            document_pages::table
                .filter(document_pages::document_id.eq(document_id))
                .filter(document_pages::version_id.eq(version))
                .order(document_pages::page_number.asc())
                .load(&mut conn)
                .await
        })?;

        Ok(records.into_iter().map(DocumentPage::from).collect())
    }

    /// Get pages needing OCR.
    #[allow(dead_code)]
    pub async fn get_pages_needing_ocr(
        &self,
        document_id: &str,
        version_id: i32,
        limit: usize,
    ) -> Result<Vec<DocumentPage>, DieselError> {
        let records: Vec<DocumentPageRecord> = with_conn!(self.pool, conn, {
            document_pages::table
                .filter(document_pages::document_id.eq(document_id))
                .filter(document_pages::version_id.eq(version_id))
                .filter(
                    document_pages::ocr_status
                        .eq("pending")
                        .or(document_pages::ocr_status.eq("text_extracted")),
                )
                .order(document_pages::page_number.asc())
                .limit(limit as i64)
                .load(&mut conn)
                .await
        })?;

        Ok(records.into_iter().map(DocumentPage::from).collect())
    }

    /// Store OCR result for a page from a specific backend.
    /// Stores in page_ocr_results table and recomputes search_text.
    #[allow(clippy::too_many_arguments)]
    pub async fn store_page_ocr_result(
        &self,
        page_id: i64,
        backend: &str,
        model: Option<&str>,
        text: Option<&str>,
        confidence: Option<f32>,
        processing_time_ms: Option<i32>,
        image_hash: Option<&str>,
    ) -> Result<(), DieselError> {
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::PageOcrResults;
        use sea_query::{Expr, OnConflict, Query};

        let now = Utc::now().to_rfc3339();
        let char_count = text.map(|t| t.chars().count() as i32);
        let word_count = text.map(|t| t.split_whitespace().count() as i32);
        let page_id_i32 = page_id as i32;

        let stmt = Query::insert()
            .into_table(PageOcrResults::Table)
            .columns([
                PageOcrResults::PageId,
                PageOcrResults::Backend,
                PageOcrResults::Text,
                PageOcrResults::Confidence,
                PageOcrResults::QualityScore,
                PageOcrResults::CharCount,
                PageOcrResults::WordCount,
                PageOcrResults::ProcessingTimeMs,
                PageOcrResults::ErrorMessage,
                PageOcrResults::CreatedAt,
                PageOcrResults::Model,
                PageOcrResults::ImageHash,
            ])
            .values_panic([
                page_id_i32.into(),
                backend.to_string().into(),
                text.map(|s| s.to_string()).into(),
                confidence.into(),
                Option::<i32>::None.into(),
                char_count.into(),
                word_count.into(),
                processing_time_ms.into(),
                Option::<String>::None.into(),
                now.clone().into(),
                model.map(|s| s.to_string()).into(),
                image_hash.map(|s| s.to_string()).into(),
            ])
            .on_conflict(
                OnConflict::new()
                    .expr(Expr::col(PageOcrResults::PageId))
                    .expr(Expr::col(PageOcrResults::Backend))
                    .expr(Expr::cust("COALESCE(\"model\", '')"))
                    .update_columns([
                        PageOcrResults::Text,
                        PageOcrResults::Confidence,
                        PageOcrResults::CharCount,
                        PageOcrResults::WordCount,
                        PageOcrResults::ProcessingTimeMs,
                        PageOcrResults::CreatedAt,
                        PageOcrResults::ImageHash,
                    ])
                    .to_owned(),
            )
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            diesel::sql_query(&sql)
                .bind::<diesel::sql_types::Integer, _>(page_id_i32)
                .bind::<diesel::sql_types::Text, _>(backend)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(text)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Float>, _>(confidence)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(None::<i32>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(char_count)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(word_count)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(
                    processing_time_ms,
                )
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(None::<&str>)
                .bind::<diesel::sql_types::Text, _>(&now)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(model)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(image_hash)
                .execute(&mut conn)
                .await?;

            Ok::<_, DieselError>(())
        })?;

        self.update_search_text(page_id).await
    }

    /// Recompute search_text from the best page_ocr_results entry (highest char_count).
    pub async fn update_search_text(&self, page_id: i64) -> Result<(), DieselError> {
        let page_id_i32 = page_id as i32;

        // Get the text with the highest char_count for this page
        let best_text: Option<Option<String>> = with_conn!(self.pool, conn, {
            page_ocr_results::table
                .filter(page_ocr_results::page_id.eq(page_id_i32))
                .filter(page_ocr_results::text.is_not_null())
                .order(page_ocr_results::char_count.desc())
                .select(page_ocr_results::text)
                .first(&mut conn)
                .await
                .optional()
        })?;

        let search_text = best_text.flatten();

        with_conn!(self.pool, conn, {
            diesel::update(document_pages::table.find(page_id_i32))
                .set((
                    document_pages::search_text.eq(&search_text),
                    document_pages::updated_at.eq(Utc::now().to_rfc3339()),
                ))
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Bulk-insert pdftotext results into page_ocr_results for all pages
    /// of a document/version that have search_text but no pdftotext row yet.
    pub async fn store_pdftotext_results_batch(
        &self,
        document_id: &str,
        version_id: i32,
    ) -> Result<(), DieselError> {
        use crate::repository::sea_tables::{DocumentPages, PageOcrResults};
        use sea_query::{Alias, Expr, Query};

        let now = Utc::now().to_rfc3339();
        let dp = Alias::new("dp");
        let por = Alias::new("por");

        let not_exists = Query::select()
            .expr(Expr::cust("1"))
            .from_as(PageOcrResults::Table, por.clone())
            .and_where(
                Expr::col((por.clone(), PageOcrResults::PageId))
                    .equals((dp.clone(), DocumentPages::Id)),
            )
            .and_where(Expr::col((por, PageOcrResults::Backend)).eq("pdftotext"))
            .to_owned();

        with_conn_split!(self.pool,
            sqlite: conn => {
                // SQLite: word count via LENGTH trick, INSERT OR IGNORE
                // NOTE: Use Func::cust / Expr::cust with inline column refs instead of
                // Expr::cust_with_expr — the latter doesn't replace $N on SqliteQueryBuilder.
                let st_col = Expr::col((dp.clone(), DocumentPages::SearchText));
                let char_count = sea_query::Func::cust(Alias::new("LENGTH")).arg(st_col.clone());
                let word_count = Expr::cust(
                    "LENGTH(\"dp\".\"search_text\") - LENGTH(REPLACE(\"dp\".\"search_text\", ' ', '')) + 1",
                );

                let select = Query::select()
                    .column((dp.clone(), DocumentPages::Id))
                    .expr(Expr::cust("'pdftotext'"))
                    .column((dp.clone(), DocumentPages::SearchText))
                    .expr(char_count)
                    .expr(word_count)
                    .expr(Expr::val(&now as &str))
                    .from_as(DocumentPages::Table, dp.clone())
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::DocumentId)).eq(document_id),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::VersionId)).eq(version_id),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::SearchText)).is_not_null(),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::SearchText))
                            .ne(Expr::cust("''")),
                    )
                    .and_where(Expr::exists(not_exists.clone()).not())
                    .to_owned();

                let stmt = Query::insert()
                    .into_table(PageOcrResults::Table)
                    .columns([
                        PageOcrResults::PageId,
                        PageOcrResults::Backend,
                        PageOcrResults::Text,
                        PageOcrResults::CharCount,
                        PageOcrResults::WordCount,
                        PageOcrResults::CreatedAt,
                    ])
                    .select_from(select)
                    .expect("valid INSERT...SELECT")
                    .on_conflict(sea_query::OnConflict::new().do_nothing().to_owned())
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);

                // Bind order: SELECT vals (now) → WHERE (document_id, version_id)
                diesel::sql_query(&sql)
                    .bind::<diesel::sql_types::Text, _>(&now)
                    .bind::<diesel::sql_types::Text, _>(document_id)
                    .bind::<diesel::sql_types::Integer, _>(version_id)
                    .execute(&mut conn)
                    .await?;
                Ok::<_, DieselError>(())
            },
            postgres: conn => {
                // Postgres: word count via regexp_split_to_array, ON CONFLICT DO NOTHING
                let select = Query::select()
                    .column((dp.clone(), DocumentPages::Id))
                    .expr(Expr::cust("'pdftotext'"))
                    .column((dp.clone(), DocumentPages::SearchText))
                    .expr(Expr::cust_with_expr(
                        "LENGTH($1)",
                        Expr::col((dp.clone(), DocumentPages::SearchText)),
                    ))
                    .expr(Expr::cust_with_expr(
                        "array_length(regexp_split_to_array($1, '\\s+'), 1)",
                        Expr::col((dp.clone(), DocumentPages::SearchText)),
                    ))
                    .expr(Expr::val(&now as &str))
                    .from_as(DocumentPages::Table, dp.clone())
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::DocumentId)).eq(document_id),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::VersionId)).eq(version_id),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::SearchText)).is_not_null(),
                    )
                    .and_where(
                        Expr::col((dp.clone(), DocumentPages::SearchText)).ne(""),
                    )
                    .and_where(Expr::exists(not_exists).not())
                    .to_owned();

                let stmt = Query::insert()
                    .into_table(PageOcrResults::Table)
                    .columns([
                        PageOcrResults::PageId,
                        PageOcrResults::Backend,
                        PageOcrResults::Text,
                        PageOcrResults::CharCount,
                        PageOcrResults::WordCount,
                        PageOcrResults::CreatedAt,
                    ])
                    .select_from(select)
                    .expect("valid INSERT...SELECT")
                    .on_conflict(sea_query::OnConflict::new().do_nothing().to_owned())
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);

                // Bind order: SELECT vals (now) → WHERE (document_id, version_id)
                diesel::sql_query(&sql)
                    .bind::<diesel::sql_types::Text, _>(&now)
                    .bind::<diesel::sql_types::Text, _>(document_id)
                    .bind::<diesel::sql_types::Integer, _>(version_id)
                    .execute(&mut conn)
                    .await?;
                Ok::<_, DieselError>(())
            }
        )?;

        Ok(())
    }

    /// Store OCR error for a page from a specific backend.
    #[allow(dead_code)]
    pub async fn store_page_ocr_error(
        &self,
        page_id: i64,
        backend: &str,
        model: Option<&str>,
        error_message: &str,
    ) -> Result<(), DieselError> {
        use crate::repository::pool::build_sql;
        use crate::repository::sea_tables::PageOcrResults;
        use sea_query::{Expr, OnConflict, Query};

        let now = Utc::now().to_rfc3339();
        let page_id_i32 = page_id as i32;

        let stmt = Query::insert()
            .into_table(PageOcrResults::Table)
            .columns([
                PageOcrResults::PageId,
                PageOcrResults::Backend,
                PageOcrResults::Text,
                PageOcrResults::Confidence,
                PageOcrResults::QualityScore,
                PageOcrResults::CharCount,
                PageOcrResults::WordCount,
                PageOcrResults::ProcessingTimeMs,
                PageOcrResults::ErrorMessage,
                PageOcrResults::CreatedAt,
                PageOcrResults::Model,
                PageOcrResults::ImageHash,
            ])
            .values_panic([
                page_id_i32.into(),
                backend.to_string().into(),
                Option::<String>::None.into(),
                Option::<f32>::None.into(),
                Option::<i32>::None.into(),
                Option::<i32>::None.into(),
                Option::<i32>::None.into(),
                Option::<i32>::None.into(),
                error_message.to_string().into(),
                now.clone().into(),
                model.map(|s| s.to_string()).into(),
                Option::<String>::None.into(),
            ])
            .on_conflict(
                OnConflict::new()
                    .expr(Expr::col(PageOcrResults::PageId))
                    .expr(Expr::col(PageOcrResults::Backend))
                    .expr(Expr::cust("COALESCE(\"model\", '')"))
                    .update_columns([
                        PageOcrResults::Text,
                        PageOcrResults::ErrorMessage,
                        PageOcrResults::CreatedAt,
                    ])
                    .to_owned(),
            )
            .to_owned();

        let sql = build_sql(&self.pool, &stmt);

        with_conn!(self.pool, conn, {
            diesel::sql_query(&sql)
                .bind::<diesel::sql_types::Integer, _>(page_id_i32)
                .bind::<diesel::sql_types::Text, _>(backend)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(None::<&str>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Float>, _>(None::<f32>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(None::<i32>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(None::<i32>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(None::<i32>)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Integer>, _>(None::<i32>)
                .bind::<diesel::sql_types::Text, _>(error_message)
                .bind::<diesel::sql_types::Text, _>(&now)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(model)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(None::<&str>)
                .execute(&mut conn)
                .await?;
            Ok(())
        })
    }

    /// Get all OCR results for a page from different backends.
    #[allow(dead_code)]
    pub async fn get_page_ocr_results(
        &self,
        page_id: i64,
    ) -> Result<Vec<PageOcrResultRecord>, DieselError> {
        with_conn!(self.pool, conn, {
            page_ocr_results::table
                .filter(page_ocr_results::page_id.eq(page_id as i32))
                .order(page_ocr_results::created_at.desc())
                .load(&mut conn)
                .await
        })
    }

    /// Find an existing OCR result by image hash and backend.
    /// Used for deduplication - if we've already OCR'd this exact image, reuse the result.
    pub async fn find_ocr_result_by_image_hash(
        &self,
        image_hash: &str,
        backend: &str,
    ) -> Result<Option<PageOcrResultRecord>, DieselError> {
        with_conn!(self.pool, conn, {
            page_ocr_results::table
                .filter(page_ocr_results::image_hash.eq(image_hash))
                .filter(page_ocr_results::backend.eq(backend))
                .filter(page_ocr_results::text.is_not_null())
                .first(&mut conn)
                .await
                .optional()
        })
    }

    /// Delete pages for a document version.
    pub async fn delete_pages(
        &self,
        document_id: &str,
        version_id: i32,
    ) -> Result<(), DieselError> {
        with_conn!(self.pool, conn, {
            diesel::delete(
                document_pages::table
                    .filter(document_pages::document_id.eq(document_id))
                    .filter(document_pages::version_id.eq(version_id)),
            )
            .execute(&mut conn)
            .await?;
            Ok(())
        })
    }

    /// Check if all pages are complete.
    pub async fn are_all_pages_complete(
        &self,
        document_id: &str,
        version_id: i32,
    ) -> Result<bool, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let pending_count: i64 = document_pages::table
                .filter(document_pages::document_id.eq(document_id))
                .filter(document_pages::version_id.eq(version_id))
                .filter(
                    document_pages::ocr_status
                        .eq("pending")
                        .or(document_pages::ocr_status.eq("text_extracted")),
                )
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(pending_count == 0)
        })
    }

    /// Count pages needing OCR across all documents.
    pub async fn count_pages_needing_ocr(&self) -> Result<u64, DieselError> {
        use diesel::dsl::count_star;
        with_conn!(self.pool, conn, {
            let count: i64 = document_pages::table
                .filter(
                    document_pages::ocr_status
                        .eq("pending")
                        .or(document_pages::ocr_status.eq("text_extracted")),
                )
                .select(count_star())
                .first(&mut conn)
                .await?;
            Ok(count as u64)
        })
    }

    /// Get pages needing OCR across all documents.
    ///
    /// Returns pages ordered by: manually prioritized documents first,
    /// then pages with no text, then everything else.
    pub async fn get_all_pages_needing_ocr(
        &self,
        limit: usize,
    ) -> Result<Vec<DocumentPage>, DieselError> {
        use crate::schema::documents;

        let records: Vec<DocumentPageRecord> = with_conn!(self.pool, conn, {
            document_pages::table
                .inner_join(
                    documents::table.on(documents::id.eq(document_pages::document_id)),
                )
                .filter(
                    document_pages::ocr_status
                        .eq("pending")
                        .or(document_pages::ocr_status.eq("text_extracted")),
                )
                .order((
                    documents::analysis_priority.desc(),
                    diesel::dsl::sql::<diesel::sql_types::Integer>(
                        "CASE WHEN document_pages.search_text IS NULL THEN 0 ELSE 1 END",
                    )
                    .asc(),
                    document_pages::document_id.asc(),
                    document_pages::page_number.asc(),
                ))
                .select(DocumentPageRecord::as_select())
                .limit(limit as i64)
                .load(&mut conn)
                .await
        })?;

        Ok(records.into_iter().map(DocumentPage::from).collect())
    }

    /// Get combined page text for a document.
    pub async fn get_combined_page_text(
        &self,
        document_id: &str,
        version: i32,
    ) -> Result<Option<String>, DieselError> {
        let texts: Vec<Option<String>> = with_conn!(self.pool, conn, {
            document_pages::table
                .filter(document_pages::document_id.eq(document_id))
                .filter(document_pages::version_id.eq(version))
                .order(document_pages::page_number.asc())
                .select(document_pages::search_text)
                .load(&mut conn)
                .await
        })?;

        let combined: String = texts.into_iter().flatten().collect::<Vec<_>>().join("\n\n");

        if combined.is_empty() {
            Ok(None)
        } else {
            Ok(Some(combined))
        }
    }

    /// Full-text search on page content.
    ///
    /// Postgres: uses `tsvector`/`tsquery` for ranked full-text search with headline snippets.
    /// SQLite: falls back to LIKE matching (no headlines).
    pub async fn search_page_content(
        &self,
        query: &str,
        source_id: Option<&str>,
        document_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PageSearchRow>, DieselError> {
        use crate::repository::sea_tables::{DocumentPages, Documents, DocumentVersions};
        use sea_query::{Alias, Expr, Query};

        let dp = Alias::new("dp");
        let d = Alias::new("d");
        let dv = Alias::new("dv");

        with_conn_split!(self.pool,
            sqlite: conn => {
                let like_pattern = format!("%{query}%");

                let mut stmt = Query::select()
                    .column((dp.clone(), DocumentPages::DocumentId))
                    .column((d.clone(), Documents::Title))
                    .column((d.clone(), Documents::SourceId))
                    .column((dp.clone(), DocumentPages::PageNumber))
                    .expr_as(Expr::cust("''"), Alias::new("headline"))
                    .column((dv.clone(), DocumentVersions::ContentHash))
                    .expr_as(
                        Expr::col((dv.clone(), DocumentVersions::MimeType)),
                        Alias::new("version_mime_type"),
                    )
                    .column((dv.clone(), DocumentVersions::OriginalFilename))
                    .column((dv.clone(), DocumentVersions::DedupIndex))
                    .column((d.clone(), Documents::SourceUrl))
                    .from_as(DocumentPages::Table, dp.clone())
                    .join_as(
                        sea_query::JoinType::Join,
                        Documents::Table,
                        d.clone(),
                        Expr::col((d.clone(), Documents::Id))
                            .equals((dp.clone(), DocumentPages::DocumentId)),
                    )
                    .join_as(
                        sea_query::JoinType::Join,
                        DocumentVersions::Table,
                        dv.clone(),
                        Expr::col((dv.clone(), DocumentVersions::Id))
                            .equals((dp.clone(), DocumentPages::VersionId)),
                    )
                    .and_where(Expr::cust_with_exprs(
                        "COALESCE($1, '') LIKE $2",
                        [
                            Expr::col((dp.clone(), DocumentPages::SearchText)).into(),
                            Expr::val(&like_pattern as &str).into(),
                        ],
                    ))
                    .to_owned();

                if let Some(sid) = source_id {
                    stmt = stmt
                        .and_where(Expr::col((d.clone(), Documents::SourceId)).eq(sid))
                        .to_owned();
                }
                if let Some(did) = document_id {
                    stmt = stmt
                        .and_where(Expr::col((dp.clone(), DocumentPages::DocumentId)).eq(did))
                        .to_owned();
                }

                stmt = stmt
                    .order_by((dp.clone(), DocumentPages::DocumentId), sea_query::Order::Asc)
                    .order_by((dp.clone(), DocumentPages::PageNumber), sea_query::Order::Asc)
                    .limit(limit as u64)
                    .offset(offset as u64)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::SqliteQueryBuilder);

                // Bind order: like_pattern, [source_id], [document_id]
                let mut q = diesel::sql_query(sql).into_boxed::<diesel::sqlite::Sqlite>();
                q = q.bind::<diesel::sql_types::Text, _>(&like_pattern);
                if let Some(sid) = source_id {
                    q = q.bind::<diesel::sql_types::Text, _>(sid);
                }
                if let Some(did) = document_id {
                    q = q.bind::<diesel::sql_types::Text, _>(did);
                }
                q.load::<PageSearchRow>(&mut conn).await
            },
            postgres: conn => {
                let tsquery = Expr::cust_with_expr(
                    "plainto_tsquery('english', $1)",
                    Expr::val(query),
                );
                let tsvec = Expr::cust_with_expr(
                    "to_tsvector('english', COALESCE($1, ''))",
                    Expr::col((dp.clone(), DocumentPages::SearchText)),
                );
                let headline = Expr::cust_with_exprs(
                    "ts_headline('english', COALESCE($1, ''), $2, 'MaxFragments=3, MaxWords=30, MinWords=10')",
                    [
                        Expr::col((dp.clone(), DocumentPages::SearchText)).into(),
                        tsquery.clone().into(),
                    ],
                );
                let fts_match = Expr::cust_with_exprs(
                    "$1 @@ $2",
                    [tsvec.clone().into(), tsquery.clone().into()],
                );
                let rank = Expr::cust_with_exprs(
                    "ts_rank($1, $2)",
                    [tsvec.into(), tsquery.into()],
                );

                let mut stmt = Query::select()
                    .column((dp.clone(), DocumentPages::DocumentId))
                    .column((d.clone(), Documents::Title))
                    .column((d.clone(), Documents::SourceId))
                    .column((dp.clone(), DocumentPages::PageNumber))
                    .expr_as(headline, Alias::new("headline"))
                    .column((dv.clone(), DocumentVersions::ContentHash))
                    .expr_as(
                        Expr::col((dv.clone(), DocumentVersions::MimeType)),
                        Alias::new("version_mime_type"),
                    )
                    .column((dv.clone(), DocumentVersions::OriginalFilename))
                    .column((dv.clone(), DocumentVersions::DedupIndex))
                    .column((d.clone(), Documents::SourceUrl))
                    .from_as(DocumentPages::Table, dp.clone())
                    .join_as(
                        sea_query::JoinType::Join,
                        Documents::Table,
                        d.clone(),
                        Expr::col((d.clone(), Documents::Id))
                            .equals((dp.clone(), DocumentPages::DocumentId)),
                    )
                    .join_as(
                        sea_query::JoinType::Join,
                        DocumentVersions::Table,
                        dv.clone(),
                        Expr::col((dv.clone(), DocumentVersions::Id))
                            .equals((dp.clone(), DocumentPages::VersionId)),
                    )
                    .and_where(fts_match)
                    .to_owned();

                if let Some(sid) = source_id {
                    stmt = stmt
                        .and_where(Expr::col((d.clone(), Documents::SourceId)).eq(sid))
                        .to_owned();
                }
                if let Some(did) = document_id {
                    stmt = stmt
                        .and_where(Expr::col((dp.clone(), DocumentPages::DocumentId)).eq(did))
                        .to_owned();
                }

                stmt = stmt
                    .order_by_expr(rank, sea_query::Order::Desc)
                    .order_by((dp.clone(), DocumentPages::DocumentId), sea_query::Order::Asc)
                    .order_by((dp.clone(), DocumentPages::PageNumber), sea_query::Order::Asc)
                    .limit(limit as u64)
                    .offset(offset as u64)
                    .to_owned();

                let (sql, _) = stmt.build(sea_query::PostgresQueryBuilder);

                // Bind order: query (headline), query (WHERE fts), [source_id],
                // [document_id], query (ORDER BY rank)
                let mut q = diesel::sql_query(sql).into_boxed::<diesel::pg::Pg>();
                q = q
                    .bind::<diesel::sql_types::Text, _>(query)
                    .bind::<diesel::sql_types::Text, _>(query);
                if let Some(sid) = source_id {
                    q = q.bind::<diesel::sql_types::Text, _>(sid);
                }
                if let Some(did) = document_id {
                    q = q.bind::<diesel::sql_types::Text, _>(did);
                }
                q = q.bind::<diesel::sql_types::Text, _>(query);
                q.load::<PageSearchRow>(&mut conn).await
            }
        )
    }

    /// Count full-text search matches on page content.
    pub async fn count_page_content_matches(
        &self,
        query: &str,
        source_id: Option<&str>,
        document_id: Option<&str>,
    ) -> Result<u64, DieselError> {
        use crate::schema::documents;
        use diesel::dsl::count_star;

        with_conn_split!(self.pool,
            sqlite: conn => {
                let like_pattern = format!("%{query}%");
                let search_filter = diesel::dsl::sql::<diesel::sql_types::Bool>(
                    "COALESCE(document_pages.search_text, '') LIKE ",
                )
                .bind::<diesel::sql_types::Text, _>(&like_pattern);

                let mut query = document_pages::table
                    .inner_join(
                        documents::table.on(documents::id.eq(document_pages::document_id)),
                    )
                    .filter(search_filter)
                    .select(count_star())
                    .into_boxed();

                if let Some(sid) = source_id {
                    query = query.filter(documents::source_id.eq(sid));
                }
                if let Some(did) = document_id {
                    query = query.filter(document_pages::document_id.eq(did));
                }

                let count: i64 = query.first(&mut conn).await?;
                Ok(count as u64)
            },
            postgres: conn => {
                let fts_filter = diesel::dsl::sql::<diesel::sql_types::Bool>(
                    "to_tsvector('english', COALESCE(document_pages.search_text, '')) @@ plainto_tsquery('english', ",
                )
                .bind::<diesel::sql_types::Text, _>(query)
                .sql(")");

                let mut pg_query = document_pages::table
                    .inner_join(
                        documents::table.on(documents::id.eq(document_pages::document_id)),
                    )
                    .filter(fts_filter)
                    .select(count_star())
                    .into_boxed();

                if let Some(sid) = source_id {
                    pg_query = pg_query.filter(documents::source_id.eq(sid));
                }
                if let Some(did) = document_id {
                    pg_query = pg_query.filter(document_pages::document_id.eq(did));
                }

                let count: i64 = pg_query.first(&mut conn).await?;
                Ok(count as u64)
            }
        )
    }

    /// Get OCR results for pages in bulk (stub).
    pub async fn get_pages_ocr_results_bulk(
        &self,
        _page_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<OcrResult>>, DieselError> {
        Ok(HashMap::new())
    }

    /// Get pages without a specific OCR backend (stub).
    pub async fn get_pages_without_backend(
        &self,
        _document_id: &str,
        _backend: &str,
    ) -> Result<Vec<DocumentPage>, DieselError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, DocumentStatus, DocumentVersion, PageOcrStatus};
    use crate::repository::diesel_document::tests::setup_test_db;
    use crate::repository::diesel_document::DieselDocumentRepository;
    use chrono::Utc;

    async fn setup_doc_with_version(
        repo: &DieselDocumentRepository,
        doc_id: &str,
        source: &str,
    ) -> i64 {
        let doc = Document {
            id: doc_id.to_string(),
            source_id: source.to_string(),
            title: format!("Doc {doc_id}"),
            source_url: format!("https://example.com/{doc_id}"),
            extracted_text: None,
            synopsis: None,
            tags: vec![],
            status: DocumentStatus::Pending,
            metadata: serde_json::Value::Object(Default::default()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            discovery_method: "seed".to_string(),
            versions: vec![],
        };
        repo.save(&doc).await.unwrap();
        let version = DocumentVersion {
            id: 0,
            content_hash: format!("hash-{doc_id}"),
            content_hash_blake3: None,
            file_path: None,
            file_size: 1024,
            mime_type: "application/pdf".to_string(),
            acquired_at: Utc::now(),
            source_url: None,
            original_filename: None,
            server_date: None,
            page_count: None,
            archive_snapshot_id: None,
            earliest_archived_at: None,
            dedup_index: None,
        };
        repo.add_version(doc_id, &version).await.unwrap()
    }

    fn make_page(doc_id: &str, version_id: i64, page_num: u32) -> DocumentPage {
        DocumentPage::new(doc_id.to_string(), version_id, page_num)
    }

    #[tokio::test]
    async fn test_count_pages() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        for i in 1..=3 {
            repo.save_page(&make_page("d1", vid, i)).await.unwrap();
        }

        let count = repo.count_pages("d1", vid as i32).await.unwrap();
        assert_eq!(count, 3);

        let zero = repo.count_pages("d1", 999).await.unwrap();
        assert_eq!(zero, 0);
    }

    #[tokio::test]
    async fn test_save_and_get_pages() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let mut p2 = make_page("d1", vid, 2);
        p2.search_text = Some("page two text".to_string());
        let mut p1 = make_page("d1", vid, 1);
        p1.search_text = Some("page one text".to_string());
        let p3 = make_page("d1", vid, 3);

        repo.save_page(&p2).await.unwrap();
        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p3).await.unwrap();

        let pages = repo.get_pages("d1", vid as i32).await.unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].page_number, 1);
        assert_eq!(pages[1].page_number, 2);
        assert_eq!(pages[2].page_number, 3);
        assert_eq!(pages[0].search_text.as_deref(), Some("page one text"));
        assert_eq!(pages[1].search_text.as_deref(), Some("page two text"));
        assert!(pages[2].search_text.is_none());
    }

    #[tokio::test]
    async fn test_save_pages_batch() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let pages: Vec<DocumentPage> = (1..=5).map(|i| make_page("d1", vid, i)).collect();
        repo.save_pages_batch(&pages).await.unwrap();

        let count = repo.count_pages("d1", vid as i32).await.unwrap();
        assert_eq!(count, 5);

        repo.save_pages_batch(&[]).await.unwrap();
    }

    #[tokio::test]
    async fn test_delete_pages() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        for i in 1..=3 {
            repo.save_page(&make_page("d1", vid, i)).await.unwrap();
        }
        assert_eq!(repo.count_pages("d1", vid as i32).await.unwrap(), 3);

        repo.delete_pages("d1", vid as i32).await.unwrap();
        assert_eq!(repo.count_pages("d1", vid as i32).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_are_all_pages_complete() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let mut p1 = make_page("d1", vid, 1);
        p1.ocr_status = PageOcrStatus::OcrComplete;
        let p2 = make_page("d1", vid, 2); // Pending

        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p2).await.unwrap();

        let complete = repo.are_all_pages_complete("d1", vid as i32).await.unwrap();
        assert!(!complete);

        // Update p2 to complete
        let mut p2_updated = make_page("d1", vid, 2);
        p2_updated.ocr_status = PageOcrStatus::OcrComplete;
        repo.save_page(&p2_updated).await.unwrap();

        let complete = repo.are_all_pages_complete("d1", vid as i32).await.unwrap();
        assert!(complete);
    }

    #[tokio::test]
    async fn test_get_combined_page_text() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let mut p1 = make_page("d1", vid, 1);
        p1.search_text = Some("First page".to_string());
        let mut p2 = make_page("d1", vid, 2);
        p2.search_text = Some("Second page".to_string());
        let p3 = make_page("d1", vid, 3); // No text

        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p2).await.unwrap();
        repo.save_page(&p3).await.unwrap();

        let combined = repo
            .get_combined_page_text("d1", vid as i32)
            .await
            .unwrap();
        assert_eq!(combined.as_deref(), Some("First page\n\nSecond page"));

        // All None → None
        let vid2 = setup_doc_with_version(&repo, "d2", "src").await;
        repo.save_page(&make_page("d2", vid2, 1)).await.unwrap();
        let none = repo
            .get_combined_page_text("d2", vid2 as i32)
            .await
            .unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn test_count_pages_needing_ocr() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let p1 = make_page("d1", vid, 1); // Pending
        let mut p2 = make_page("d1", vid, 2);
        p2.ocr_status = PageOcrStatus::TextExtracted;
        let mut p3 = make_page("d1", vid, 3);
        p3.ocr_status = PageOcrStatus::OcrComplete;

        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p2).await.unwrap();
        repo.save_page(&p3).await.unwrap();

        let count = repo.count_pages_needing_ocr().await.unwrap();
        assert_eq!(count, 2); // pending + text_extracted
    }

    #[tokio::test]
    async fn test_get_pages_needing_ocr() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let p1 = make_page("d1", vid, 1); // Pending
        let mut p2 = make_page("d1", vid, 2);
        p2.ocr_status = PageOcrStatus::TextExtracted;
        let mut p3 = make_page("d1", vid, 3);
        p3.ocr_status = PageOcrStatus::OcrComplete;

        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p2).await.unwrap();
        repo.save_page(&p3).await.unwrap();

        let pages = repo
            .get_pages_needing_ocr("d1", vid as i32, 10)
            .await
            .unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].page_number, 1);
        assert_eq!(pages[1].page_number, 2);
    }

    #[tokio::test]
    async fn test_store_page_ocr_result() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let page_id = repo.save_page(&make_page("d1", vid, 1)).await.unwrap();

        repo.store_page_ocr_result(
            page_id,
            "tesseract",
            None,
            Some("OCR text from tesseract"),
            Some(0.95),
            Some(1500),
            Some("abc123"),
        )
        .await
        .unwrap();

        // search_text should be updated via update_search_text
        let pages = repo.get_pages("d1", vid as i32).await.unwrap();
        assert_eq!(
            pages[0].search_text.as_deref(),
            Some("OCR text from tesseract")
        );

        // Verify OCR result stored
        let results = repo.get_page_ocr_results(page_id).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].backend, "tesseract");
        assert_eq!(results[0].text.as_deref(), Some("OCR text from tesseract"));
        assert_eq!(results[0].image_hash.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn test_store_page_ocr_error() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let page_id = repo.save_page(&make_page("d1", vid, 1)).await.unwrap();

        repo.store_page_ocr_error(page_id, "groq", Some("llama-v3"), "rate limited")
            .await
            .unwrap();

        let results = repo.get_page_ocr_results(page_id).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].backend, "groq");
        assert_eq!(results[0].model.as_deref(), Some("llama-v3"));
        assert_eq!(results[0].error_message.as_deref(), Some("rate limited"));
        assert!(results[0].text.is_none());

        // search_text should remain None (no successful OCR)
        let pages = repo.get_pages("d1", vid as i32).await.unwrap();
        assert!(pages[0].search_text.is_none());
    }

    #[tokio::test]
    async fn test_get_page_ocr_results_multiple_backends() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let page_id = repo.save_page(&make_page("d1", vid, 1)).await.unwrap();

        repo.store_page_ocr_result(
            page_id,
            "tesseract",
            None,
            Some("short text"),
            Some(0.8),
            None,
            None,
        )
        .await
        .unwrap();

        repo.store_page_ocr_result(
            page_id,
            "groq",
            Some("llama-v3"),
            Some("much longer text with better quality"),
            Some(0.95),
            None,
            None,
        )
        .await
        .unwrap();

        let results = repo.get_page_ocr_results(page_id).await.unwrap();
        assert_eq!(results.len(), 2);

        // search_text should be the longest (best) text
        let pages = repo.get_pages("d1", vid as i32).await.unwrap();
        assert_eq!(
            pages[0].search_text.as_deref(),
            Some("much longer text with better quality")
        );
    }

    #[tokio::test]
    async fn test_find_ocr_result_by_image_hash() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        let page_id = repo.save_page(&make_page("d1", vid, 1)).await.unwrap();

        repo.store_page_ocr_result(
            page_id,
            "tesseract",
            None,
            Some("cached OCR text"),
            None,
            None,
            Some("img-hash-abc"),
        )
        .await
        .unwrap();

        let found = repo
            .find_ocr_result_by_image_hash("img-hash-abc", "tesseract")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().text.as_deref(), Some("cached OCR text"));

        let not_found = repo
            .find_ocr_result_by_image_hash("nonexistent", "tesseract")
            .await
            .unwrap();
        assert!(not_found.is_none());

        let wrong_backend = repo
            .find_ocr_result_by_image_hash("img-hash-abc", "groq")
            .await
            .unwrap();
        assert!(wrong_backend.is_none());
    }

    #[tokio::test]
    async fn test_get_all_pages_needing_ocr_priority() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);

        let vid1 = setup_doc_with_version(&repo, "d1", "src").await;
        let vid2 = setup_doc_with_version(&repo, "d2", "src").await;

        // Set d2 to higher priority
        match &repo.pool {
            crate::repository::pool::DbPool::Sqlite(pool) => {
                let mut conn = pool.get().await.unwrap();
                use crate::schema::documents;
                use diesel::prelude::*;
                diesel_async::RunQueryDsl::execute(
                    diesel::update(documents::table.find("d2"))
                        .set(documents::analysis_priority.eq(10)),
                    &mut conn,
                )
                .await
                .unwrap();
            }
            #[cfg(feature = "postgres")]
            _ => unreachable!("tests use SQLite"),
        }

        // d1 pages: pending
        repo.save_page(&make_page("d1", vid1, 1)).await.unwrap();
        // d2 pages: pending (but higher priority)
        repo.save_page(&make_page("d2", vid2, 1)).await.unwrap();

        let pages = repo.get_all_pages_needing_ocr(10).await.unwrap();
        assert_eq!(pages.len(), 2);
        // d2 should come first (higher priority)
        assert_eq!(pages[0].document_id, "d2");
        assert_eq!(pages[1].document_id, "d1");
    }

    #[tokio::test]
    async fn test_store_pdftotext_results_batch() {
        let (pool, _dir) = setup_test_db().await;
        let repo = DieselDocumentRepository::new(pool);
        let vid = setup_doc_with_version(&repo, "d1", "src").await;

        // Save pages with search_text (simulating pdftotext extraction)
        let mut p1 = make_page("d1", vid, 1);
        p1.search_text = Some("First page extracted text".to_string());
        let mut p2 = make_page("d1", vid, 2);
        p2.search_text = Some("Second page extracted text".to_string());
        let p3 = make_page("d1", vid, 3); // No text

        repo.save_page(&p1).await.unwrap();
        repo.save_page(&p2).await.unwrap();
        repo.save_page(&p3).await.unwrap();

        // Store pdftotext results in batch
        repo.store_pdftotext_results_batch("d1", vid as i32)
            .await
            .unwrap();

        // Verify page_ocr_results were created for pages with text
        let pages = repo.get_pages("d1", vid as i32).await.unwrap();
        let p1_results = repo.get_page_ocr_results(pages[0].id).await.unwrap();
        assert_eq!(p1_results.len(), 1);
        assert_eq!(p1_results[0].backend, "pdftotext");
        assert_eq!(
            p1_results[0].text.as_deref(),
            Some("First page extracted text")
        );

        let p2_results = repo.get_page_ocr_results(pages[1].id).await.unwrap();
        assert_eq!(p2_results.len(), 1);
        assert_eq!(p2_results[0].backend, "pdftotext");

        // Page 3 has no text, so no OCR result
        let p3_results = repo.get_page_ocr_results(pages[2].id).await.unwrap();
        assert!(p3_results.is_empty());

        // Running again should be idempotent (ON CONFLICT DO NOTHING)
        repo.store_pdftotext_results_batch("d1", vid as i32)
            .await
            .unwrap();
        let p1_results_again = repo.get_page_ocr_results(pages[0].id).await.unwrap();
        assert_eq!(p1_results_again.len(), 1);
    }
}
