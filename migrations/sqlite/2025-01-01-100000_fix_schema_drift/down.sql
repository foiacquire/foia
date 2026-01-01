-- Revert schema drift fixes (best effort - SQLite can't drop columns)

-- Recreate service_status with original (incorrect) schema
DROP TABLE IF EXISTS service_status;

CREATE TABLE service_status (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    service_type TEXT NOT NULL,
    hostname TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'starting',
    current_source TEXT,
    started_at TEXT NOT NULL,
    last_heartbeat TEXT NOT NULL,
    stats TEXT,
    last_error TEXT,
    error_count INTEGER NOT NULL DEFAULT 0,
    UNIQUE (service_type, hostname)
);

CREATE INDEX idx_service_status_type ON service_status(service_type);
CREATE INDEX idx_service_status_heartbeat ON service_status(last_heartbeat);
CREATE INDEX idx_service_status_source ON service_status(current_source) WHERE current_source IS NOT NULL;

-- Note: content_hash_blake3 column cannot be removed in SQLite
-- It will remain but be unused after downgrade
