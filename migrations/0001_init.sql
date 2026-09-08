-- Mindspace schema v0.
-- Sources, evidence, and assertions are immutable: BEFORE UPDATE triggers
-- abort any mutation. Only memories (the lifecycle rows) and the joins
-- change after insertion.

CREATE TABLE spaces (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL
);

CREATE TABLE space_links (
    from_space TEXT NOT NULL REFERENCES spaces(id),
    to_space   TEXT NOT NULL REFERENCES spaces(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (from_space, to_space),
    CHECK (from_space <> to_space)
);

CREATE TABLE sources (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    location    TEXT NOT NULL,
    title       TEXT NOT NULL,
    content     TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    media_type  TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    ingested_at INTEGER NOT NULL,
    UNIQUE (kind, location, content_hash)
);

CREATE TABLE evidence (
    id           TEXT PRIMARY KEY,
    source_id    TEXT NOT NULL REFERENCES sources(id),
    kind         TEXT NOT NULL,
    heading_path TEXT,
    start_line   INTEGER,
    end_line     INTEGER,
    ordinal      INTEGER NOT NULL,
    content      TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    UNIQUE (source_id, kind, ordinal)
);

CREATE TABLE assertions (
    id                 TEXT PRIMARY KEY,
    kind               TEXT NOT NULL,
    subject            TEXT NOT NULL,
    subject_normalized TEXT NOT NULL,
    predicate          TEXT,
    content            TEXT NOT NULL,
    extraction         TEXT NOT NULL,
    evidence_id        TEXT NOT NULL REFERENCES evidence(id)
);

CREATE TABLE memories (
    id            TEXT PRIMARY KEY,
    assertion_id  TEXT NOT NULL UNIQUE REFERENCES assertions(id),
    state         TEXT NOT NULL CHECK (state IN ('active', 'superseded', 'retired')),
    created_at    INTEGER NOT NULL,
    superseded_by TEXT REFERENCES memories(id),
    superseded_at INTEGER,
    retired_at    INTEGER,
    retire_reason TEXT
);

CREATE TABLE memory_spaces (
    memory_id  TEXT NOT NULL REFERENCES memories(id),
    space_id   TEXT NOT NULL REFERENCES spaces(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (memory_id, space_id)
);

CREATE VIRTUAL TABLE assertion_fts USING fts5(
    assertion_id UNINDEXED,
    subject,
    content
);

CREATE INDEX idx_assertions_subject ON assertions(subject_normalized);
CREATE INDEX idx_memories_state ON memories(state);
CREATE INDEX idx_memory_spaces_space ON memory_spaces(space_id);
CREATE INDEX idx_sources_location ON sources(location);
CREATE INDEX idx_evidence_source ON evidence(source_id);

CREATE TRIGGER sources_immutable
BEFORE UPDATE ON sources
BEGIN
    SELECT RAISE(ABORT, 'sources are immutable; ingestion creates new sources');
END;

CREATE TRIGGER evidence_immutable
BEFORE UPDATE ON evidence
BEGIN
    SELECT RAISE(ABORT, 'evidence is immutable; re-ingestion creates new evidence');
END;

CREATE TRIGGER assertions_immutable
BEFORE UPDATE ON assertions
BEGIN
    SELECT RAISE(ABORT, 'assertions are immutable; supersession creates new assertions');
END;
