-- Mindspace schema v0.5 Phase 2 M1: derived embedding index.
-- Embeddings are a versioned, derived capability over immutable assertions
-- (sources -> evidence -> assertions -> memories). They are an index, not
-- the memory model: assertions, evidence, and sources remain immutable and
-- are never rewritten here. Only this derived table and mind_meta change.
--
-- assertion_embeddings is keyed by (assertion_id, derivation_key), where
-- derivation_key is a hash of the complete embedding pipeline (model id and
-- weight checksum, tokenizer checksum, prefix strategy, pooling, normalization,
-- max sequence length, truncation, dimension, runtime and version). Changing
-- any pipeline parameter yields a new key, so stale vectors are ignored rather
-- than silently reused. Model weights are never stored in SQLite.
--
-- mind_meta is a small generic key/value store for database-level settings,
-- notably the active embedding model binding (embedding.derivation_key, ...).
CREATE TABLE assertion_embeddings (
    assertion_id   TEXT NOT NULL REFERENCES assertions(id),
    derivation_key TEXT NOT NULL,
    model          TEXT NOT NULL,
    model_version  TEXT NOT NULL,
    dimension      INTEGER NOT NULL CHECK (dimension > 0),
    embedding      BLOB NOT NULL,
    created_at     INTEGER NOT NULL,
    PRIMARY KEY (assertion_id, derivation_key)
);

CREATE INDEX idx_assertion_embeddings_model
    ON assertion_embeddings(model, model_version);
CREATE INDEX idx_assertion_embeddings_assertion
    ON assertion_embeddings(assertion_id);

CREATE TABLE mind_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
