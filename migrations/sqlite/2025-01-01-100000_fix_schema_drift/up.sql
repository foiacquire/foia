-- Fix schema drift between SQLite and PostgreSQL migrations

-- Add missing content_hash_blake3 column to document_versions
ALTER TABLE document_versions ADD COLUMN content_hash_blake3 TEXT;

-- Recreate service_status table with correct schema
-- SQLite doesn't support most ALTER TABLE operations, so we recreate

DROP TABLE IF EXISTS service_status;

CREATE TABLE service_status (
    id TEXT PRIMARY KEY NOT NULL,
    service_type TEXT NOT NULL,
    source_id TEXT,
    status TEXT NOT NULL DEFAULT 'starting',
    last_heartbeat TEXT NOT NULL,
    last_activity TEXT,
    current_task TEXT,
    stats TEXT NOT NULL DEFAULT '{}',
    started_at TEXT NOT NULL,
    host TEXT,
    version TEXT,
    last_error TEXT,
    last_error_at TEXT,
    error_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_service_status_type ON service_status(service_type);
CREATE INDEX idx_service_status_heartbeat ON service_status(last_heartbeat);
CREATE INDEX idx_service_status_source ON service_status(source_id) WHERE source_id IS NOT NULL;
