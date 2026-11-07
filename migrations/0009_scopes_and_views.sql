-- Scopes, remote observations, and current code views.
--
-- A Space is a knowledge domain; a Scope says where ingested material came
-- from (README: "scope versus space"). A scope is a git repository or a
-- directory (or single file) ingest root. Associating scopes with spaces is
-- add-only, like memory_spaces. Spaces never encode repository identity.
--
-- These tables hold current-state associations and observations, not
-- canonical knowledge: they are deliberately outside the M6 write fence and
-- history (the CodeStructure dimension stays unsupported). Older rows are not
-- backfilled; re-ingesting attaches a scope to its spaces. The interim
-- `git_views:<id>` entries in mind_meta are ignored and may be removed later.

CREATE TABLE scopes (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('repository', 'directory')),
    repository_id TEXT REFERENCES repositories(id),
    root          TEXT,
    created_at    INTEGER NOT NULL,
    CHECK ((kind = 'repository' AND repository_id IS NOT NULL AND root IS NULL)
        OR (kind = 'directory' AND root IS NOT NULL AND repository_id IS NULL))
);
CREATE UNIQUE INDEX idx_scopes_repository ON scopes(repository_id) WHERE repository_id IS NOT NULL;
CREATE UNIQUE INDEX idx_scopes_root ON scopes(root) WHERE root IS NOT NULL;

CREATE TABLE scope_spaces (
    scope_id   TEXT NOT NULL REFERENCES scopes(id),
    space_id   TEXT NOT NULL REFERENCES spaces(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (scope_id, space_id)
);
CREATE INDEX idx_scope_spaces_space ON scope_spaces(space_id);

-- Remotes as observed. `url` never contains credentials. A slug is derived
-- from the URL and is not an identity (ADR-015, ADR-027). `is_current` marks
-- the remotes of the latest observation; older rows keep their history.
CREATE TABLE repository_remotes (
    repository_id     TEXT NOT NULL REFERENCES repositories(id),
    name              TEXT NOT NULL,
    url               TEXT NOT NULL,
    slug_key          TEXT,
    slug              TEXT,
    first_observed_at INTEGER NOT NULL,
    last_observed_at  INTEGER NOT NULL,
    is_current        INTEGER NOT NULL DEFAULT 1 CHECK (is_current IN (0, 1)),
    PRIMARY KEY (repository_id, name, url)
);
CREATE INDEX idx_repository_remotes_slug ON repository_remotes(slug_key);

-- The commits a repository's latest ingest derived at (HEAD, worktrees,
-- selected refs; every tip under --history). Syntax facts are immutable, so
-- currency of code definitions is recorded here (ADR-025).
CREATE TABLE git_views (
    repository_id TEXT NOT NULL REFERENCES repositories(id),
    commit_oid    TEXT NOT NULL,
    observed_at   INTEGER NOT NULL,
    PRIMARY KEY (repository_id, commit_oid)
);
