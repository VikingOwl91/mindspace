CREATE UNIQUE INDEX IF NOT EXISTS idx_logical_sources_manual
    ON logical_sources(current_location) WHERE kind = 'manual';
