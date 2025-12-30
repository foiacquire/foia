-- Service status tracking table for real-time visibility into running services
CREATE TABLE service_status (
    id TEXT PRIMARY KEY,                    -- "scraper:doj", "ocr:worker-1", "server:main"
    service_type TEXT NOT NULL,             -- "scraper", "ocr", "server"
    source_id TEXT,                         -- For scrapers: which source they're scraping
    status TEXT NOT NULL,                   -- "starting", "running", "idle", "error", "stopped"
    last_heartbeat TIMESTAMPTZ NOT NULL,
    last_activity TIMESTAMPTZ,              -- Last time actual work was done
    current_task TEXT,                      -- Human-readable: "crawling /foia/page/5"

    -- Stats as JSONB for flexibility per service type
    stats JSONB NOT NULL DEFAULT '{}',

    -- Metadata
    started_at TIMESTAMPTZ NOT NULL,
    host TEXT,                              -- Container ID / hostname
    version TEXT,                           -- App version for debugging

    -- Error tracking
    last_error TEXT,
    last_error_at TIMESTAMPTZ,
    error_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_service_status_type ON service_status(service_type);
CREATE INDEX idx_service_status_heartbeat ON service_status(last_heartbeat);
CREATE INDEX idx_service_status_source ON service_status(source_id) WHERE source_id IS NOT NULL;
