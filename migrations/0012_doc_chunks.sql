-- M8 S3: immutable versioned derivations, deliberately outside the retrieval/memory spine.
-- A set exists even when it has no chunks; absence and a completed empty result differ.
CREATE TABLE doc_chunk_sets (
    extraction_id TEXT NOT NULL REFERENCES doc_extractions(id),
    version TEXT NOT NULL CHECK(length(version) BETWEEN 1 AND 64),
    budget_identity TEXT NOT NULL CHECK(length(budget_identity) BETWEEN 1 AND 2048),
    plan_hash TEXT NOT NULL CHECK(length(plan_hash) = 64 AND plan_hash NOT GLOB '*[^0-9a-f]*'),
    chunk_count INTEGER NOT NULL CHECK(chunk_count >= 0),
    PRIMARY KEY(extraction_id, version, budget_identity)
);
CREATE TABLE doc_chunks (
    id TEXT PRIMARY KEY CHECK(length(id) = 64 AND id NOT GLOB '*[^0-9a-f]*'),
    extraction_id TEXT NOT NULL,
    version TEXT NOT NULL,
    budget_identity TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    text_start INTEGER NOT NULL CHECK(text_start >= 0),
    text_end INTEGER NOT NULL CHECK(text_end > text_start),
    token_count INTEGER NOT NULL CHECK(token_count > 0),
    context_json TEXT NOT NULL,
    anchor TEXT NOT NULL,
    UNIQUE(extraction_id, version, budget_identity, ordinal),
    FOREIGN KEY(extraction_id, version, budget_identity)
        REFERENCES doc_chunk_sets(extraction_id, version, budget_identity)
);
CREATE TRIGGER doc_chunk_sets_no_update BEFORE UPDATE ON doc_chunk_sets
BEGIN SELECT RAISE(ABORT, 'document chunk sets are immutable'); END;
CREATE TRIGGER doc_chunk_sets_no_delete BEFORE DELETE ON doc_chunk_sets
BEGIN SELECT RAISE(ABORT, 'document chunk sets are immutable'); END;
CREATE TRIGGER doc_chunks_no_update BEFORE UPDATE ON doc_chunks
BEGIN SELECT RAISE(ABORT, 'document chunks are immutable'); END;
CREATE TRIGGER doc_chunks_no_delete BEFORE DELETE ON doc_chunks
BEGIN SELECT RAISE(ABORT, 'document chunks are immutable'); END;
-- octet_length uses SQLite byte-length metadata; CAST AS BLOB would fetch the full text per chunk.
CREATE TRIGGER doc_chunks_range BEFORE INSERT ON doc_chunks
BEGIN
    SELECT CASE WHEN NEW.text_end > (
        SELECT octet_length(s.content) FROM doc_extractions e
        JOIN sources s ON s.id = e.source_id WHERE e.id = NEW.extraction_id
    ) THEN RAISE(ABORT, 'document chunk outside canonical text') END;
END;
