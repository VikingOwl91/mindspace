-- v0.2: deterministic Git repository ingest.
--
-- Git history is a graph of immutable objects (commits, trees, blobs,
-- annotated tags) plus a transaction-time journal of mutable refs. Objects
-- are identified by (repository, oid) and are never deduplicated across
-- repositories: two repositories never merge. Ref names are observations,
-- not durable identity; ref events record what changed at observation time
-- and never delete history.
--
-- Object content is stored only for text blobs (the derivation inputs);
-- other blobs are recorded as (oid, size).

-- The ingest session journal now also covers git repository scans.
CREATE TABLE ingest_sessions_new (
    id          TEXT PRIMARY KEY,
    root        TEXT NOT NULL,
    scope       TEXT NOT NULL CHECK (scope IN ('directory', 'file', 'git_repository')),
    observed_at INTEGER NOT NULL
);

INSERT INTO ingest_sessions_new (id, root, scope, observed_at)
SELECT id, root, scope, observed_at FROM ingest_sessions;

DROP TABLE ingest_sessions;
ALTER TABLE ingest_sessions_new RENAME TO ingest_sessions;

-- A repository is a Mindspace identity that lives in the repository's own
-- common dir (a marker file), so moving the repository keeps its identity
-- while a fresh clone gets its own. current_path is the last observed path,
-- like logical_sources.current_location.
CREATE TABLE repositories (
    id           TEXT PRIMARY KEY,
    current_path TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

CREATE TABLE git_commits (
    repository_id  TEXT NOT NULL REFERENCES repositories(id),
    oid            TEXT NOT NULL,
    tree_oid       TEXT NOT NULL,
    author_name    TEXT NOT NULL DEFAULT '',
    author_email   TEXT NOT NULL DEFAULT '',
    author_time    INTEGER NOT NULL,
    committer_name TEXT NOT NULL DEFAULT '',
    committer_email TEXT NOT NULL DEFAULT '',
    commit_time    INTEGER NOT NULL,
    message        TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (repository_id, oid)
);

-- Parent topology is stored exactly: a merge is a commit with two parents,
-- and parent order is meaningful.
CREATE TABLE git_commit_parents (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    commit_oid    TEXT NOT NULL,
    position      INTEGER NOT NULL,
    parent_oid    TEXT NOT NULL,
    PRIMARY KEY (repository_id, commit_oid, position)
);

CREATE TABLE git_trees (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    oid           TEXT NOT NULL,
    PRIMARY KEY (repository_id, oid)
);

CREATE TABLE git_tree_entries (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    tree_oid      TEXT NOT NULL,
    name          TEXT NOT NULL,
    mode          TEXT NOT NULL,
    target_oid    TEXT NOT NULL,
    target_kind   TEXT NOT NULL CHECK (target_kind IN ('commit', 'tree', 'blob')),
    PRIMARY KEY (repository_id, tree_oid, name)
);

CREATE TABLE git_blobs (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    oid           TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    content       TEXT,
    PRIMARY KEY (repository_id, oid)
);

-- Annotated tag objects; lightweight tags are refs that point directly at a
-- commit and have no object of their own.
CREATE TABLE git_tags (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    oid           TEXT NOT NULL,
    target_oid    TEXT NOT NULL,
    target_kind   TEXT NOT NULL CHECK (target_kind IN ('commit', 'tree', 'blob', 'tag')),
    tagger_name   TEXT NOT NULL DEFAULT '',
    tagger_email  TEXT NOT NULL DEFAULT '',
    tag_time      INTEGER,
    message       TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (repository_id, oid)
);

-- Latest observed state of every ref (heads, remotes, tags, HEAD). HEAD is
-- a ref like any other: its symbolic target is recorded when it points at a
-- branch, and its resolved target when detached.
CREATE TABLE git_refs (
    repository_id   TEXT NOT NULL REFERENCES repositories(id),
    name            TEXT NOT NULL,
    target_oid      TEXT NOT NULL,
    target_kind     TEXT NOT NULL CHECK (target_kind IN ('commit', 'tree', 'blob', 'tag')),
    symbolic_target TEXT,
    last_observed_at INTEGER NOT NULL,
    PRIMARY KEY (repository_id, name)
);

-- The ref journal: created, fast_forward, non_fast_forward, deleted. The
-- fast-forward classification is a proven ancestor check; anything else is
-- recorded as non_fast_forward without claiming rebase or force-push.
CREATE TABLE git_ref_events (
    id            TEXT PRIMARY KEY,
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    ref_name      TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('created', 'fast_forward', 'non_fast_forward', 'deleted')),
    from_oid      TEXT,
    to_oid        TEXT,
    session_id    TEXT REFERENCES ingest_sessions(id),
    created_at    INTEGER NOT NULL
);

CREATE INDEX idx_git_ref_events_repo ON git_ref_events(repository_id, ref_name);

-- Git provenance for derived source versions: which blob at which path in
-- which commit of which repository a source version was derived from.
CREATE TABLE git_source_versions (
    source_id     TEXT PRIMARY KEY REFERENCES sources(id),
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    blob_oid      TEXT NOT NULL,
    path          TEXT NOT NULL,
    commit_oid    TEXT NOT NULL
);

CREATE INDEX idx_git_source_versions_commit
    ON git_source_versions(repository_id, commit_oid);

-- Git-derived material gets logical sources scoped per (repository, commit,
-- path): a path in one commit is one identity. Branches are not belief
-- timelines, so git-derived memories never auto-supersede across commits;
-- same content at the same commit reconciles to reuse only.
ALTER TABLE logical_sources ADD COLUMN repository_id TEXT REFERENCES repositories(id);
ALTER TABLE logical_sources ADD COLUMN commit_oid TEXT;
ALTER TABLE logical_sources ADD COLUMN git_path TEXT;

CREATE UNIQUE INDEX idx_logical_sources_git
    ON logical_sources(repository_id, commit_oid, git_path)
    WHERE repository_id IS NOT NULL;
