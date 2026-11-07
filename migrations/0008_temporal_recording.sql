ALTER TABLE memories
    ADD COLUMN producer_family TEXT NOT NULL DEFAULT 'legacy:unknown';

CREATE TABLE m6_revisions (
    revision_id INTEGER PRIMARY KEY,
    recorded_at INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('baseline', 'write'))
);

CREATE TABLE m6_history_coverage (
    dimension TEXT PRIMARY KEY,
    baseline_revision INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    complete_from_revision INTEGER,
    status TEXT NOT NULL CHECK (status IN ('covered', 'unsupported')),
    reason TEXT
);

CREATE TABLE m6_row_availability (
    table_name TEXT NOT NULL,
    entity_key TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    PRIMARY KEY (table_name, entity_key)
);

CREATE TABLE m6_memory_history (
    memory_id TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    assertion_id TEXT,
    state TEXT,
    created_at INTEGER,
    superseded_by TEXT,
    superseded_at INTEGER,
    retired_at INTEGER,
    retire_reason TEXT,
    producer_family TEXT NOT NULL,
    PRIMARY KEY (memory_id, revision_id)
);

CREATE TABLE m6_space_history (
    space_id TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    name TEXT,
    description TEXT,
    created_at INTEGER,
    PRIMARY KEY (space_id, revision_id)
);

CREATE TABLE m6_space_link_history (
    from_space TEXT NOT NULL,
    to_space TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    created_at INTEGER,
    PRIMARY KEY (from_space, to_space, revision_id)
);

CREATE TABLE m6_memory_space_history (
    memory_id TEXT NOT NULL,
    space_id TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    created_at INTEGER,
    PRIMARY KEY (memory_id, space_id, revision_id)
);

CREATE TABLE m6_logical_source_history (
    logical_id TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    kind TEXT,
    state TEXT,
    current_location TEXT,
    created_at INTEGER,
    repository_id TEXT,
    commit_oid TEXT,
    git_path TEXT,
    PRIMARY KEY (logical_id, revision_id)
);

CREATE TABLE m6_repository_history (
    repository_id TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    current_path TEXT,
    created_at INTEGER,
    PRIMARY KEY (repository_id, revision_id)
);

CREATE TABLE m6_git_ref_history (
    repository_id TEXT NOT NULL,
    ref_name TEXT NOT NULL,
    revision_id INTEGER NOT NULL REFERENCES m6_revisions(revision_id),
    is_deleted INTEGER NOT NULL CHECK (is_deleted IN (0, 1)),
    target_oid TEXT,
    target_kind TEXT,
    symbolic_target TEXT,
    last_observed_at INTEGER,
    PRIMARY KEY (repository_id, ref_name, revision_id)
);

CREATE INDEX m6_memory_history_by_revision
    ON m6_memory_history(revision_id, memory_id);
CREATE INDEX m6_space_history_by_revision
    ON m6_space_history(revision_id, space_id);
CREATE INDEX m6_logical_source_history_by_revision
    ON m6_logical_source_history(revision_id, logical_id);
CREATE INDEX m6_repository_history_by_revision
    ON m6_repository_history(revision_id, repository_id);
CREATE INDEX m6_git_ref_history_by_revision
    ON m6_git_ref_history(revision_id, repository_id, ref_name);
