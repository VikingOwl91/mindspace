-- v0.1: deterministic source identity and lifecycle.
--
-- logical_sources: the stable identity of a source across moves and
-- renames. Mutable lifecycle state lives here (presence, current
-- location); history lives in source_events and the observation records.
--
-- sources become immutable content versions of a logical source: same
-- logical source + same bytes = one version, however often it is
-- re-observed or relocated.
--
-- ingest_sessions + source_observations record what an ingest operation
-- observed; the latest full (directory) session of a root is the baseline
-- for the next one.
--
-- source_events is the lifecycle journal: changes only (inferred moves,
-- disappearances), never routine re-observations.

CREATE TABLE logical_sources (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL,
    state            TEXT NOT NULL CHECK (state IN ('present', 'absent')),
    current_location TEXT,
    created_at       INTEGER NOT NULL
);

CREATE TABLE ingest_sessions (
    id          TEXT PRIMARY KEY,
    root        TEXT NOT NULL,
    scope       TEXT NOT NULL CHECK (scope IN ('directory', 'file')),
    observed_at INTEGER NOT NULL
);

CREATE TABLE source_events (
    id            TEXT PRIMARY KEY,
    logical_id    TEXT NOT NULL REFERENCES logical_sources(id),
    kind          TEXT NOT NULL CHECK (kind IN ('moved', 'disappeared')),
    from_location TEXT,
    to_location   TEXT,
    version_id    TEXT REFERENCES sources(id),
    session_id    TEXT REFERENCES ingest_sessions(id),
    created_at    INTEGER NOT NULL
);

CREATE TABLE source_observations (
    session_id   TEXT NOT NULL REFERENCES ingest_sessions(id),
    logical_id   TEXT NOT NULL REFERENCES logical_sources(id),
    location     TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    observed_at  INTEGER NOT NULL,
    PRIMARY KEY (session_id, location)
);

CREATE INDEX idx_source_observations_session ON source_observations(session_id);
CREATE INDEX idx_source_events_logical ON source_events(logical_id);

-- Rebuild sources: attach every version to a logical source and change the
-- version identity from (kind, location, content_hash) to
-- (logical_id, content_hash). The old key is dropped because the same bytes
-- may legitimately be observed at the same path under different logical
-- identities at different times (ambiguous reappearance).
CREATE TABLE sources_new (
    id           TEXT PRIMARY KEY,
    logical_id   TEXT NOT NULL REFERENCES logical_sources(id),
    kind         TEXT NOT NULL,
    location     TEXT NOT NULL,
    title        TEXT NOT NULL,
    content      TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    media_type   TEXT NOT NULL,
    size_bytes   INTEGER NOT NULL,
    ingested_at  INTEGER NOT NULL,
    UNIQUE (logical_id, content_hash)
);

-- Backfill for databases created before v0.1: one logical source per
-- distinct (kind, location); existing source rows become versions of it.
-- randomblob-based ids are ordinary (non-v7) uuids; identity semantics are
-- unaffected. Disposable development databases only, but the mapping keeps
-- same-location version continuity coherent.
INSERT INTO logical_sources (id, kind, state, current_location, created_at)
SELECT
    lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
          substr(hex(randomblob(2)), 2) || '-' ||
          substr('89ab', abs(random()) % 4 + 1, 1) ||
          substr(hex(randomblob(2)), 2) || '-' || hex(randomblob(6))),
    kind,
    'present',
    location,
    min(ingested_at)
FROM sources
GROUP BY kind, location;

INSERT INTO sources_new
SELECT s.id, l.id, s.kind, s.location, s.title, s.content, s.content_hash,
       s.media_type, s.size_bytes, s.ingested_at
FROM sources s
JOIN logical_sources l
  ON l.kind = s.kind AND l.current_location = s.location;

DROP TABLE sources;
ALTER TABLE sources_new RENAME TO sources;

CREATE INDEX idx_sources_logical ON sources(logical_id);
CREATE INDEX idx_sources_location ON sources(location);

-- The rebuild dropped the old table with its immutability trigger;
-- restore it for the new table.
CREATE TRIGGER sources_immutable
BEFORE UPDATE ON sources
BEGIN
    SELECT RAISE(ABORT, 'sources are immutable; ingestion creates new sources');
END;
