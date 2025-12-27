// @generated automatically by Diesel CLI.
// Manually corrected: PRIMARY KEY columns are not nullable

diesel::table! {
    config_history (id) {
        id -> Integer,
        data -> Text,
        format -> Text,
        hash -> Text,
        created_at -> Text,
    }
}

diesel::table! {
    crawl_config (source_id) {
        source_id -> Text,
        config_hash -> Text,
        updated_at -> Text,
    }
}

diesel::table! {
    crawl_requests (id) {
        id -> Integer,
        source_id -> Text,
        url -> Text,
        method -> Text,
        request_headers -> Text,
        request_at -> Text,
        response_status -> Nullable<Integer>,
        response_headers -> Text,
        response_at -> Nullable<Text>,
        response_size -> Nullable<Integer>,
        duration_ms -> Nullable<Integer>,
        error -> Nullable<Text>,
        was_conditional -> Integer,
        was_not_modified -> Integer,
    }
}

diesel::table! {
    crawl_urls (id) {
        id -> Integer,
        url -> Text,
        source_id -> Text,
        status -> Text,
        discovery_method -> Text,
        parent_url -> Nullable<Text>,
        discovery_context -> Text,
        depth -> Integer,
        discovered_at -> Text,
        fetched_at -> Nullable<Text>,
        retry_count -> Integer,
        last_error -> Nullable<Text>,
        next_retry_at -> Nullable<Text>,
        etag -> Nullable<Text>,
        last_modified -> Nullable<Text>,
        content_hash -> Nullable<Text>,
        document_id -> Nullable<Text>,
    }
}

diesel::table! {
    document_pages (id) {
        id -> Integer,
        document_id -> Text,
        version -> Integer,
        page_number -> Integer,
        text_content -> Nullable<Text>,
        ocr_text -> Nullable<Text>,
        has_images -> Integer,
        status -> Text,
    }
}

diesel::table! {
    document_versions (id) {
        id -> Integer,
        document_id -> Text,
        version -> Integer,
        file_path -> Nullable<Text>,
        content_hash -> Nullable<Text>,
        mime_type -> Nullable<Text>,
        file_size -> Nullable<Integer>,
        fetched_at -> Text,
    }
}

diesel::table! {
    documents (id) {
        id -> Text,
        source_id -> Text,
        url -> Text,
        title -> Nullable<Text>,
        status -> Text,
        metadata -> Text,
        created_at -> Text,
        updated_at -> Text,
    }
}

diesel::table! {
    rate_limit_state (domain) {
        domain -> Text,
        current_delay_ms -> Integer,
        in_backoff -> Integer,
        total_requests -> Integer,
        rate_limit_hits -> Integer,
        updated_at -> Text,
    }
}

diesel::table! {
    sources (id) {
        id -> Text,
        source_type -> Text,
        name -> Text,
        base_url -> Text,
        metadata -> Text,
        created_at -> Text,
        last_scraped -> Nullable<Text>,
    }
}

diesel::table! {
    virtual_files (id) {
        id -> Integer,
        document_id -> Text,
        version -> Integer,
        path -> Text,
        mime_type -> Nullable<Text>,
        file_size -> Nullable<Integer>,
        status -> Text,
        ocr_text -> Nullable<Text>,
    }
}

diesel::joinable!(document_pages -> documents (document_id));
diesel::joinable!(document_versions -> documents (document_id));
diesel::joinable!(documents -> sources (source_id));
diesel::joinable!(virtual_files -> documents (document_id));

diesel::allow_tables_to_appear_in_same_query!(
    config_history,
    crawl_config,
    crawl_requests,
    crawl_urls,
    document_pages,
    document_versions,
    documents,
    rate_limit_state,
    sources,
    virtual_files,
);
